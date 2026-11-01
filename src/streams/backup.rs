// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! D1: complete SQLite online snapshots and fail-closed restart-only restore.
//! All methods are blocking and MUST run off the GTK main context, except the
//! startup-only handoff, which is executed by the first SQLite worker.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use rusqlite::{
    backup::{Backup, StepResult},
    Connection, OpenFlags,
};

use super::storage::{self, Storage, StorageError, StorageResult, APPLICATION_ID, SCHEMA_VERSION};

const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const REQUEST: &str = ".broadcaster-d1-restore-request";
const CANDIDATE_PREFIX: &str = ".broadcaster-d1-import-";
const RECOVERY_PREFIX: &str = ".broadcaster-d1-recovery-";
static STARTUP_ARMED: AtomicBool = AtomicBool::new(false);
static STARTUP_RESTORE_LOCK: Mutex<()> = Mutex::new(());
static RESTORE_PROCESSED: AtomicBool = AtomicBool::new(false);

/// Set only on the primary GtkApplication's startup signal (not a second
/// command-line process registering with an already-running application).
pub(crate) fn arm_primary_startup() {
    STARTUP_ARMED.store(true, Ordering::SeqCst);
}

pub(crate) fn suppress_automatic_refresh_this_startup() -> bool {
    RESTORE_PROCESSED.load(Ordering::SeqCst)
}

pub(super) fn check_startup_restore(storage: &Storage) -> StorageResult<()> {
    // Multiple async initializers can race at first startup. Serialize the
    // entire pre-open restore, not just the ownership flag, so none can open
    // the previous database while its bytes are being replaced.
    let _guard = STARTUP_RESTORE_LOCK
        .lock()
        .map_err(|_| StorageError::StorageFailure)?;
    if STARTUP_ARMED.swap(false, Ordering::SeqCst) {
        // If an old DB exists, the first service's initialization must complete
        // the replacement before any other connection is opened.
        let result = apply_pending(storage.database_path());
        if result.is_err() {
            RESTORE_PROCESSED.store(true, Ordering::SeqCst);
        }
        result?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotCounts {
    pub playlists: i64,
    pub channels: i64,
    pub guides: i64,
    pub programmes: i64,
}

struct Temporary {
    path: PathBuf,
    cleanup: bool,
}
impl Temporary {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            cleanup: true,
        }
    }
    fn keep(mut self) -> PathBuf {
        self.cleanup = false;
        std::mem::take(&mut self.path)
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_file(&self.path);
            for suffix in ["-wal", "-shm", "-journal"] {
                let _ = fs::remove_file(format!("{}{}", self.path.display(), suffix));
            }
        }
    }
}

pub(crate) struct PreparedImport {
    candidate: Temporary,
    pub counts: SnapshotCounts,
}

fn random_file(parent: &Path, prefix: &str) -> PathBuf {
    parent.join(format!("{prefix}{}.sqlite3", uuid::Uuid::new_v4()))
}

fn create_private(path: &Path) -> StorageResult<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| StorageError::PermissionDenied)
}

fn sync_dir(path: &Path) -> StorageResult<()> {
    File::open(path)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| StorageError::StorageFailure)
}

fn stand_alone(path: &Path) -> StorageResult<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        if fs::symlink_metadata(format!("{}{}", path.display(), suffix)).is_ok() {
            return Err(StorageError::IncompatibleDatabase);
        }
    }
    Ok(())
}

fn verify_snapshot(path: &Path, allow_older: bool) -> StorageResult<(Connection, i64)> {
    storage::require_private_file(path)?;
    let metadata = fs::metadata(path).map_err(|_| StorageError::PermissionDenied)?;
    if metadata.len() < 100 || metadata.len() > MAX_SNAPSHOT_BYTES {
        return Err(StorageError::IncompatibleDatabase);
    }
    stand_alone(path)?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    if journal != "delete" {
        return Err(StorageError::IncompatibleDatabase);
    }
    let id: i64 = conn
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    if id != APPLICATION_ID || !(if allow_older { 3..=5 } else { 5..=5 }).contains(&version) {
        return Err(StorageError::IncompatibleDatabase);
    }
    storage::validate_snapshot_schema(&conn, version)?;
    Ok((conn, version))
}

fn counts(conn: &Connection) -> StorageResult<SnapshotCounts> {
    let count = |table: &str| -> StorageResult<i64> {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .map_err(|_| StorageError::CorruptDatabase)
    };
    Ok(SnapshotCounts {
        playlists: count("iptv_sources")?,
        channels: count("iptv_channels")?,
        guides: count("epg_sources")?,
        programmes: count("epg_programmes")?,
    })
}

fn online_backup(source: &Connection, target: &Path) -> StorageResult<()> {
    let file = create_private(target)?;
    drop(file);
    let mut dest = Connection::open_with_flags(target, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| StorageError::StorageFailure)?;
    {
        let backup = Backup::new(source, &mut dest).map_err(storage::map_sql)?;
        // SQLite BUSY/LOCKED can otherwise spin indefinitely when another
        // process keeps a read/write lock. A finite deadline prevents export
        // from retaining a worker forever under a hostile or broken source.
        // 512 pages per step keeps WAL writers responsive for large guides.
        let deadline = Instant::now() + Duration::from_secs(900);
        loop {
            if Instant::now() >= deadline {
                return Err(StorageError::StorageFailure);
            }
            let status = backup.step(512).map_err(storage::map_sql)?;
            match status {
                StepResult::Done => break,
                StepResult::More | StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                // The upstream enum is explicitly non-exhaustive. Never
                // guess whether a future result means a completed snapshot.
                _ => return Err(StorageError::StorageFailure),
            }
        }
    }
    // Online backup of a WAL source can preserve the source journal-mode
    // header. Convert the independent destination to a standalone DELETE
    // snapshot before publication; never publish an orphaned WAL dependency.
    let mode: String = dest
        .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
        .map_err(storage::map_sql)?;
    if mode != "delete" {
        return Err(StorageError::StorageFailure);
    }
    dest.close().map_err(|_| StorageError::StorageFailure)?;
    File::open(target)
        .and_then(|file| file.sync_all())
        .map_err(|_| StorageError::StorageFailure)?;
    Ok(())
}

/// Export as a new, private 0600 file in the user-chosen folder. The hard-link
/// is an atomic no-clobber publish even under a destination symlink race.
pub(crate) fn export(storage: &Storage, destination: &Path) -> StorageResult<SnapshotCounts> {
    let parent = destination.parent().ok_or(StorageError::InvalidInput)?;
    if !parent.is_dir() || destination.file_name().is_none() {
        return Err(StorageError::InvalidInput);
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(StorageError::AlreadyExists);
    }
    let source = storage.connect()?;
    let candidate_path = random_file(parent, ".broadcaster-export-");
    let stage = Temporary::new(candidate_path.clone());
    online_backup(&source, &candidate_path)?;
    let (check, _) = verify_snapshot(&candidate_path, false)?;
    let result = counts(&check)?;
    drop(check);
    sync_dir(parent)?;
    fs::hard_link(&candidate_path, destination).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            StorageError::AlreadyExists
        } else {
            StorageError::StorageFailure
        }
    })?;
    sync_dir(parent)?;
    drop(stage);
    sync_dir(parent)?;
    Ok(result)
}

/// Untrusted picker input: bound a no-follow single-file copy in the private
/// application directory; NEVER open the user-selected original read/write.
pub(crate) fn prepare_import(storage: &Storage, input: &Path) -> StorageResult<PreparedImport> {
    storage::ensure_private_parent(storage.database_path())?;
    let parent = storage
        .database_path()
        .parent()
        .ok_or(StorageError::InvalidInput)?;
    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    #[cfg(unix)]
    const O_NOFOLLOW: i32 = 0o400000;
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    opts.custom_flags(O_NOFOLLOW);
    let mut reader = opts
        .open(input)
        .map_err(|_| StorageError::PermissionDenied)?;
    let source_meta = reader
        .metadata()
        .map_err(|_| StorageError::PermissionDenied)?;
    if !source_meta.is_file() || source_meta.len() < 100 || source_meta.len() > MAX_SNAPSHOT_BYTES {
        return Err(StorageError::IncompatibleDatabase);
    }
    #[cfg(unix)]
    {
        let live =
            fs::metadata(storage.database_path()).map_err(|_| StorageError::PermissionDenied)?;
        if live.dev() == source_meta.dev() && live.ino() == source_meta.ino() {
            return Err(StorageError::InvalidInput);
        }
    }
    let candidate = Temporary::new(random_file(parent, CANDIDATE_PREFIX));
    let mut writer = create_private(&candidate.path)?;
    let mut copied = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| StorageError::StorageFailure)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(read as u64)
            .ok_or(StorageError::IncompatibleDatabase)?;
        if copied > MAX_SNAPSHOT_BYTES || copied > source_meta.len() {
            return Err(StorageError::IncompatibleDatabase);
        }
        writer
            .write_all(&buffer[..read])
            .map_err(|_| StorageError::StorageFailure)?;
    }
    if copied != source_meta.len() {
        return Err(StorageError::IncompatibleDatabase);
    }
    writer
        .sync_all()
        .map_err(|_| StorageError::StorageFailure)?;
    drop(writer);
    let (probe, version) = verify_snapshot(&candidate.path, true)?;
    drop(probe);
    if version < SCHEMA_VERSION {
        storage::upgrade_import_stage(&candidate.path, version)?;
    }
    let (probe, _) = verify_snapshot(&candidate.path, false)?;
    let summary = counts(&probe)?;
    drop(probe);
    sync_dir(parent)?;
    Ok(PreparedImport {
        candidate,
        counts: summary,
    })
}

/// Confirmation does not touch the currently open database. A fixed private
/// marker is published durably; the next primary application startup performs
/// the replacement before the first SQLite connection and scheduler.
pub(crate) fn confirm_import(storage: &Storage, prepared: PreparedImport) -> StorageResult<()> {
    let parent = storage
        .database_path()
        .parent()
        .ok_or(StorageError::InvalidInput)?;
    let marker = parent.join(REQUEST);
    if fs::symlink_metadata(&marker).is_ok() {
        return Err(StorageError::AlreadyExists);
    }
    let recovery = random_file(parent, RECOVERY_PREFIX);
    let candidate_name = prepared
        .candidate
        .path
        .file_name()
        .ok_or(StorageError::InvalidInput)?;
    let recovery_name = recovery.file_name().ok_or(StorageError::InvalidInput)?;
    let staged =
        Temporary::new(parent.join(format!(".broadcaster-d1-marker-{}", uuid::Uuid::new_v4())));
    let mut file = create_private(&staged.path)?;
    write!(
        file,
        "D1\n{}\n{}\n",
        candidate_name.to_string_lossy(),
        recovery_name.to_string_lossy()
    )
    .map_err(|_| StorageError::StorageFailure)?;
    file.sync_all().map_err(|_| StorageError::StorageFailure)?;
    drop(file);
    sync_dir(parent)?;
    fs::hard_link(&staged.path, &marker).map_err(|_| StorageError::AlreadyExists)?;
    sync_dir(parent)?;
    drop(staged);
    sync_dir(parent)?;
    let _ = prepared.candidate.keep();
    Ok(())
}

fn marker_name(parent: &Path, value: &str, prefix: &str) -> StorageResult<PathBuf> {
    let Some(uuid) = value
        .strip_prefix(prefix)
        .and_then(|v| v.strip_suffix(".sqlite3"))
    else {
        return Err(StorageError::IncompatibleDatabase);
    };
    uuid::Uuid::parse_str(uuid).map_err(|_| StorageError::IncompatibleDatabase)?;
    Ok(parent.join(value))
}

fn checkpoint_old(path: &Path) -> StorageResult<()> {
    storage::require_private_file(path)?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| StorageError::StorageFailure)?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(storage::map_sql)?;
    let checkpoint: (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(storage::map_sql)?;
    if checkpoint.0 != 0 {
        return Err(StorageError::StorageFailure);
    }
    let journal: String = conn
        .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
        .map_err(storage::map_sql)?;
    if journal != "delete" {
        return Err(StorageError::StorageFailure);
    }
    conn.close().map_err(|_| StorageError::StorageFailure)?;
    stand_alone(path)
}

fn rollback(recovery: &Path, database: &Path) -> StorageResult<()> {
    let parent = database.parent().ok_or(StorageError::InvalidInput)?;
    let link = Temporary::new(random_file(parent, ".broadcaster-d1-rollback-"));
    fs::hard_link(recovery, &link.path).map_err(|_| StorageError::StorageFailure)?;
    fs::rename(&link.path, database).map_err(|_| StorageError::StorageFailure)?;
    sync_dir(parent)
}

fn apply_pending(database: &Path) -> StorageResult<()> {
    let parent = database.parent().ok_or(StorageError::InvalidInput)?;
    storage::ensure_private_parent(database)?;
    let marker = parent.join(REQUEST);
    match fs::symlink_metadata(&marker) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(StorageError::PermissionDenied),
        Ok(_) => storage::require_private_file(&marker)?,
    }
    RESTORE_PROCESSED.store(true, Ordering::SeqCst);
    let text = fs::read_to_string(&marker).map_err(|_| StorageError::IncompatibleDatabase)?;
    if text.len() > 256 {
        return Err(StorageError::IncompatibleDatabase);
    }
    let mut lines = text.lines();
    if lines.next() != Some("D1") {
        return Err(StorageError::IncompatibleDatabase);
    }
    let candidate = marker_name(
        parent,
        lines.next().ok_or(StorageError::IncompatibleDatabase)?,
        CANDIDATE_PREFIX,
    )?;
    let recovery = marker_name(
        parent,
        lines.next().ok_or(StorageError::IncompatibleDatabase)?,
        RECOVERY_PREFIX,
    )?;
    if lines.next().is_some() {
        return Err(StorageError::IncompatibleDatabase);
    }
    if fs::symlink_metadata(&candidate).is_err() {
        // Crash after publish but before clearing the journal: recover the
        // preserved pre-replacement database deterministically.
        let (copy, _) = verify_snapshot(&recovery, false)?;
        drop(copy);
        rollback(&recovery, database)?;
        fs::remove_file(&marker).map_err(|_| StorageError::StorageFailure)?;
        return sync_dir(parent);
    }
    let (import, _) = verify_snapshot(&candidate, false)?;
    drop(import);
    checkpoint_old(database)?;
    if fs::symlink_metadata(&recovery).is_err() {
        let old = Storage::at(database).connect()?;
        // Publish a fully verified recovery snapshot, never a partially
        // written one. If the application stops halfway through an online
        // backup, restart can safely retry from the still-intact old DB.
        let recovery_stage = Temporary::new(random_file(parent, ".broadcaster-d1-recovery-stage-"));
        online_backup(&old, &recovery_stage.path)?;
        drop(old);
        let (check, _) = verify_snapshot(&recovery_stage.path, false)?;
        drop(check);
        sync_dir(parent)?;
        fs::hard_link(&recovery_stage.path, &recovery).map_err(|_| StorageError::StorageFailure)?;
        sync_dir(parent)?;
        drop(recovery_stage);
        sync_dir(parent)?;
    } else {
        let (check, _) = verify_snapshot(&recovery, false)?;
        drop(check);
    }
    fs::rename(&candidate, database).map_err(|_| StorageError::StorageFailure)?;
    sync_dir(parent)?;
    if Storage::at(database)
        .connect()
        .and_then(|connection| storage::validate_snapshot_schema(&connection, SCHEMA_VERSION))
        .is_err()
    {
        rollback(&recovery, database)?;
        return Err(StorageError::CorruptDatabase);
    }
    fs::remove_file(&marker).map_err(|_| StorageError::StorageFailure)?;
    sync_dir(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("broadcaster-d1-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }
        fn storage(&self) -> Storage {
            Storage::at(self.0.join("live.sqlite3"))
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn seed(storage: &Storage, label: &str) {
        let db = storage.connect().unwrap();
        db.execute("INSERT INTO iptv_sources
            (id,name,origin_kind,origin_locator,enabled,auto_refresh,refresh_interval_seconds,created_at,updated_at)
            VALUES ('playlist-1',?1,'remote_url','https://invalid.example/private?token=secret',1,1,43200,100,100)", params![label]).unwrap();
        db.execute("INSERT INTO iptv_channels
            (id,source_id,playlist_order,imported_name,stream_url,created_at,updated_at)
            VALUES ('channel-1','playlist-1',0,'Fictional channel','https://invalid.example/live',100,100)", []).unwrap();
        db.execute(
            "INSERT INTO epg_sources (id,name,origin_kind,origin_locator,created_at,updated_at)
            VALUES ('guide-1','Fake guide','local_file','/nonexistent/guide.xml',100,100)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO epg_playlist_links
            (playlist_source_id,guide_source_id,updated_at) VALUES ('playlist-1','guide-1',100)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO iptv_channel_favourites (channel_id) VALUES ('channel-1')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn committed_wal_snapshot_is_private_and_preserves_g1_p1_state() {
        let sandbox = Sandbox::new();
        let storage = sandbox.storage();
        storage.initialize().unwrap();
        let source = storage.connect().unwrap();
        source.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        seed(&storage, "original");
        let exported = sandbox.0.join("export.sqlite3");
        assert_eq!(export(&storage, &exported).unwrap().playlists, 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&exported).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let snapshot = Storage::at(exported).connect().unwrap();
        let state: (i64, i64, i64) = snapshot
            .query_row(
                "SELECT auto_refresh,refresh_interval_seconds,
             (SELECT COUNT(*) FROM epg_playlist_links WHERE playlist_source_id='playlist-1')
             FROM iptv_sources WHERE id='playlist-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, (1, 43200, 1));
        let favourites: i64 = snapshot
            .query_row("SELECT COUNT(*) FROM iptv_channel_favourites", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(favourites, 1);
    }

    #[test]
    fn full_replacement_is_deferred_until_restart_and_recoverable() {
        let sandbox = Sandbox::new();
        let storage = sandbox.storage();
        storage.initialize().unwrap();
        seed(&storage, "first");
        let backup = sandbox.0.join("old.sqlite3");
        export(&storage, &backup).unwrap();
        storage
            .connect()
            .unwrap()
            .execute(
                "UPDATE iptv_sources SET name='second' WHERE id='playlist-1'",
                [],
            )
            .unwrap();
        let prepared = prepare_import(&storage, &backup).unwrap();
        assert_eq!(prepared.counts.channels, 1);
        assert_eq!(prepared.counts.guides, 1);
        confirm_import(&storage, prepared).unwrap();
        let before: String = storage
            .connect()
            .unwrap()
            .query_row("SELECT name FROM iptv_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            before, "second",
            "confirmation must never mutate the live database"
        );
        apply_pending(storage.database_path()).unwrap();
        let after: String = storage
            .connect()
            .unwrap()
            .query_row("SELECT name FROM iptv_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, "first");
        let recovery = fs::read_dir(&sandbox.0)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(RECOVERY_PREFIX)
            })
            .unwrap();
        let previous: String = Storage::at(recovery)
            .connect()
            .unwrap()
            .query_row("SELECT name FROM iptv_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(previous, "second");
    }

    #[test]
    fn interrupted_after_publish_rolls_back_to_last_good_database() {
        let sandbox = Sandbox::new();
        let storage = sandbox.storage();
        storage.initialize().unwrap();
        seed(&storage, "before");
        let export_path = sandbox.0.join("backup.sqlite3");
        export(&storage, &export_path).unwrap();
        storage
            .connect()
            .unwrap()
            .execute("UPDATE iptv_sources SET name='last-good'", [])
            .unwrap();
        let prepared = prepare_import(&storage, &export_path).unwrap();
        let candidate = prepared.candidate.path.clone();
        confirm_import(&storage, prepared).unwrap();
        let marker = fs::read_to_string(sandbox.0.join(REQUEST)).unwrap();
        let recovery_name = marker.lines().nth(2).unwrap();
        let recovery = marker_name(&sandbox.0, recovery_name, RECOVERY_PREFIX).unwrap();
        checkpoint_old(storage.database_path()).unwrap();
        let old = storage.connect().unwrap();
        online_backup(&old, &recovery).unwrap();
        drop(old);
        // Simulate a crash immediately after atomic replacement, before the
        // journal marker can be durably retired. No services hold DB handles.
        fs::rename(&candidate, storage.database_path()).unwrap();
        apply_pending(storage.database_path()).unwrap();
        let name: String = storage
            .connect()
            .unwrap()
            .query_row("SELECT name FROM iptv_sources", [], |row| row.get(0))
            .unwrap();
        assert_eq!(name, "last-good");
        assert!(!sandbox.0.join(REQUEST).exists());
    }

    #[test]
    fn no_clobber_foreign_future_corrupt_and_schema_trigger_fail_closed() {
        let sandbox = Sandbox::new();
        let storage = sandbox.storage();
        storage.initialize().unwrap();
        let exported = sandbox.0.join("export.sqlite3");
        export(&storage, &exported).unwrap();
        assert_eq!(
            export(&storage, &exported).unwrap_err(),
            StorageError::AlreadyExists
        );
        let bad = sandbox.0.join("invalid.sqlite3");
        fs::write(&bad, b"garbage").unwrap();
        assert!(prepare_import(&storage, &bad).is_err());
        for modification in [
            "PRAGMA application_id=1;",
            "PRAGMA user_version=999;",
            "CREATE TRIGGER injected AFTER INSERT ON iptv_sources BEGIN SELECT 1; END;",
        ] {
            let case = sandbox
                .0
                .join(format!("case-{}.sqlite3", uuid::Uuid::new_v4()));
            fs::copy(&exported, &case).unwrap();
            let conn = Connection::open(&case).unwrap();
            conn.execute_batch(modification).unwrap();
            drop(conn);
            assert!(
                prepare_import(&storage, &case).is_err(),
                "accepted untrusted schema"
            );
        }
        assert_eq!(
            storage
                .connect()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM iptv_sources", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn older_v3_v4_imports_migrate_only_in_private_staging() {
        let sandbox = Sandbox::new();
        let live = sandbox.storage();
        live.initialize().unwrap();
        for old_version in [3, 4] {
            let path = sandbox.0.join(format!("source-v{old_version}.sqlite3"));
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(storage::MIGRATION_001).unwrap();
            connection.execute_batch(storage::MIGRATION_002).unwrap();
            connection.execute_batch(storage::MIGRATION_003).unwrap();
            if old_version == 4 {
                connection.execute_batch(storage::MIGRATION_004).unwrap();
            }
            connection.close().unwrap();
            let prepared = prepare_import(&live, &path).unwrap();
            let (_, actual_version) = verify_snapshot(&prepared.candidate.path, false).unwrap();
            assert_eq!(actual_version, 5);
            let source_version: i64 =
                Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap()
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .unwrap();
            assert_eq!(
                source_version, old_version,
                "original backup must stay unchanged"
            );
            drop(prepared);
            assert_eq!(
                live.connect()
                    .unwrap()
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                5
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_import_and_missing_candidate_does_not_replace_live_database() {
        use std::os::unix::fs::symlink;
        let sandbox = Sandbox::new();
        let storage = sandbox.storage();
        storage.initialize().unwrap();
        seed(&storage, "safe");
        let exported = sandbox.0.join("export.sqlite3");
        export(&storage, &exported).unwrap();
        let link = sandbox.0.join("link.sqlite3");
        symlink(&exported, &link).unwrap();
        assert!(prepare_import(&storage, &link).is_err());
        let prepared = prepare_import(&storage, &exported).unwrap();
        let candidate = prepared.candidate.path.clone();
        confirm_import(&storage, prepared).unwrap();
        fs::remove_file(candidate).unwrap();
        assert!(apply_pending(storage.database_path()).is_err());
        let name: String = storage
            .connect()
            .unwrap()
            .query_row("SELECT name FROM iptv_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "safe");
    }
}

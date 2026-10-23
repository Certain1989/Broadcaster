// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SQLite repository. All methods are synchronous; only call them on a blocking worker.
//! Async consumers must use tokio::task::spawn_blocking (never GTK MainContext).
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use super::{logo, model::*};
use rusqlite::{
    backup::Backup, params, Connection, OpenFlags, OptionalExtension, TransactionBehavior,
};

pub const APPLICATION_ID: i64 = 0x42524354;
pub const SCHEMA_VERSION: i64 = 2;
pub const MIGRATION_001: &str = include_str!("migrations/001_initial.sql");
pub const MIGRATION_002: &str = include_str!("migrations/002_epg.sql");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageError {
    InvalidInput,
    AlreadyExists,
    NotFound,
    StaleRevision,
    SnapshotMismatch,
    ApprovalRequired,
    Declined,
    IncompatibleDatabase,
    CorruptDatabase,
    PermissionDenied,
    StorageFailure,
}
impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately never forward SQLite error messages, statement values, DB paths,
        // or URLs to UI, logs, tracing, or third-party error chains.
        f.write_str(match self {
            Self::InvalidInput => "Invalid imported channel metadata",
            Self::AlreadyExists => "A source with this name already exists",
            Self::NotFound => "The requested source or channel no longer exists",
            Self::StaleRevision => "The source changed; refresh the preview",
            Self::SnapshotMismatch => "Refresh preview no longer matches the candidate",
            Self::ApprovalRequired => "Explicit confirmation is required",
            Self::Declined => "Operation was declined",
            Self::IncompatibleDatabase => "Unsupported IPTV database version",
            Self::CorruptDatabase => "IPTV database integrity could not be verified",
            Self::PermissionDenied => "Private IPTV storage permissions are unsafe",
            Self::StorageFailure => "IPTV storage operation failed",
        })
    }
}
impl std::error::Error for StorageError {}
pub type StorageResult<T> = Result<T, StorageError>;

pub(crate) fn map_sql(err: rusqlite::Error) -> StorageError {
    if let rusqlite::Error::SqliteFailure(code, _) = &err {
        if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            || code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
        {
            return StorageError::AlreadyExists;
        }
    }
    StorageError::StorageFailure
}

#[derive(Clone)]
pub struct Storage {
    path: PathBuf,
}
impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage")
            .field("path", &"<redacted>")
            .finish()
    }
}
impl Storage {
    pub fn new() -> Self {
        Self {
            path: adw::glib::user_data_dir()
                .join("broadcaster")
                .join("broadcaster-iptv.sqlite3"),
        }
    }
    /// Worker/test-only explicit location. Call from a blocking task.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Test repositories must never run orphan pruning on the real user cache.
    pub(crate) fn uses_default_database(&self) -> bool {
        self.path == Self::new().path
    }

    pub fn initialize(&self) -> StorageResult<()> {
        let path = &self.path;
        ensure_private_parent(path)?;
        match fs::symlink_metadata(path) {
            Ok(_) => validate_existing(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                initialize_new(path, None)
            }
            Err(_) => Err(StorageError::PermissionDenied),
        }
    }

    /// Does not initialize or create a missing database: callers must initialize
    /// from a blocking worker first.
    pub(crate) fn connect(&self) -> StorageResult<Connection> {
        require_private_file(&self.path)?;
        let conn = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| StorageError::StorageFailure)?;
        configure(&conn)?;
        let id: i64 = conn
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .map_err(map_sql)?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(map_sql)?;
        if id != APPLICATION_ID || version != SCHEMA_VERSION {
            return Err(StorageError::IncompatibleDatabase);
        }
        validate_v1_schema_shape(&conn)?;
        validate_v2_schema_shape(&conn)?;
        Ok(conn)
    }

    pub fn list_sources(&self) -> StorageResult<Vec<Source>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare("SELECT id FROM iptv_sources ORDER BY source_order, id")
            .map_err(map_sql)?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql)?;
        ids.iter().map(|id| load_source(&conn, id)).collect()
    }

    /// Hash only URLs from current imported channel snapshots. Disabled
    /// sources still count: hiding one must never erase its offline logos.
    /// Do not include retained missing/tombstone entries. This runs on a
    /// SQLite worker and NEVER returns private URLs or tokens to the UI.
    pub(crate) fn referenced_logo_cache_names(&self) -> StorageResult<HashSet<String>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT logo_url FROM iptv_channels
                 WHERE is_present=1 AND logo_url IS NOT NULL AND logo_url != ''",
            )
            .map_err(map_sql)?;
        let mut names = HashSet::new();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_sql)?;
        for row in rows {
            names.insert(logo::cache_name(&row.map_err(map_sql)?));
        }
        Ok(names)
    }

    /// Counts include cached missing rows, while present counts only the active snapshot.
    pub fn source_counts(&self, id: &str) -> StorageResult<(u64, u64)> {
        let conn = self.connect()?;
        let (present, retained): (i64, i64) = conn
            .query_row(
                "SELECT COALESCE(SUM(is_present), 0), COUNT(*) FROM iptv_channels WHERE source_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(map_sql)?;
        Ok((present as u64, retained as u64))
    }

    /// Move exactly one source relative to its current neighbour. The complete
    /// order is normalized atomically, so duplicate or adjacent prior order
    /// values can never make an Up/Down control silently do nothing.
    pub fn move_source(&self, id: &str, direction: i32, at: i64) -> StorageResult<()> {
        if direction != -1 && direction != 1 {
            return Err(StorageError::InvalidInput);
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        let mut stmt = tx
            .prepare("SELECT id FROM iptv_sources ORDER BY source_order, id")
            .map_err(map_sql)?;
        let mut ids = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql)?;
        drop(stmt);
        let old = ids
            .iter()
            .position(|item| item == id)
            .ok_or(StorageError::NotFound)?;
        let target = if direction < 0 {
            old.checked_sub(1)
        } else {
            old.checked_add(1).filter(|n| *n < ids.len())
        };
        if let Some(target) = target {
            ids.swap(old, target);
        }
        for (ordinal, source_id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE iptv_sources SET source_order=?1, revision=revision+1, updated_at=?2 WHERE id=?3 AND source_order != ?1",
                params![ordinal as u32, at, source_id],
            ).map_err(map_sql)?;
        }
        tx.commit().map_err(map_sql)
    }

    pub fn get_source(&self, id: &str) -> StorageResult<Source> {
        let conn = self.connect()?;
        load_source(&conn, id)
    }

    pub fn rename_source(&self, id: &str, new_name: &str, at: i64) -> StorageResult<()> {
        bounded(new_name, 128)?;
        self.update_source(
            "UPDATE iptv_sources SET name=?1, revision=revision+1,updated_at=?2 WHERE id=?3",
            params![new_name, at, id],
        )
    }

    pub fn set_source_enabled(&self, id: &str, enabled: bool, at: i64) -> StorageResult<()> {
        self.update_source(
            "UPDATE iptv_sources SET enabled=?1, revision=revision+1,updated_at=?2 WHERE id=?3",
            params![enabled as i32, at, id],
        )
    }

    pub fn reorder_source(&self, id: &str, order: u32, at: i64) -> StorageResult<()> {
        self.update_source(
            "UPDATE iptv_sources SET source_order=?1, revision=revision+1,updated_at=?2 WHERE id=?3",
            params![order, at, id],
        )
    }

    fn update_source(&self, sql: &str, args: impl rusqlite::Params) -> StorageResult<()> {
        let conn = self.connect()?;
        if conn.execute(sql, args).map_err(map_sql)? == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub fn set_source_defaults(
        &self,
        id: &str,
        defaults: &SourceDefaults,
        at: i64,
    ) -> StorageResult<()> {
        defaults.validate()?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        let changed = tx.execute(
            "UPDATE iptv_sources SET default_region_code=?1,default_region_label=?2,revision=revision+1,updated_at=?3 WHERE id=?4",
            params![defaults.region.as_ref().and_then(|r|r.code.as_ref()), defaults.region.as_ref().and_then(|r|r.label.as_ref()), at, id],
        ).map_err(map_sql)?;
        if changed == 0 {
            return Err(StorageError::NotFound);
        }
        tx.execute(
            "DELETE FROM iptv_source_default_facets WHERE source_id=?1",
            [id],
        )
        .map_err(map_sql)?;
        for (kind, set) in [
            (FacetDimension::Language, &defaults.languages),
            (FacetDimension::Country, &defaults.countries),
        ] {
            for tag in set {
                tx.execute(
                    "INSERT INTO iptv_source_default_facets (source_id,facet,value) VALUES (?1,?2,?3)",
                    params![id, kind.sql(), tag],
                ).map_err(map_sql)?;
            }
        }
        tx.commit().map_err(map_sql)
    }

    /// Token must be produced by a deliberate confirmation action in the UI.
    pub fn delete_source_confirmed(
        &self,
        confirmation: &ConfirmedSourceDeletion,
    ) -> StorageResult<()> {
        let conn = self.connect()?;
        if conn
            .execute(
                "DELETE FROM iptv_sources WHERE id=?1 AND revision=?2",
                params![confirmation.id, confirmation.revision],
            )
            .map_err(map_sql)?
            == 0
        {
            return Err(StorageError::StaleRevision);
        }
        Ok(())
    }

    pub fn set_channel_overrides(
        &self,
        id: &str,
        edits: &ChannelOverrides,
        at: i64,
    ) -> StorageResult<()> {
        edits.validate()?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM iptv_channels WHERE id=?1)",
                [id],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        if !exists {
            return Err(StorageError::NotFound);
        }
        tx.execute(
            "DELETE FROM iptv_channel_override_facets WHERE channel_id=?1",
            [id],
        )
        .map_err(map_sql)?;
        if !edits.has_values() {
            tx.execute(
                "DELETE FROM iptv_channel_overrides WHERE channel_id=?1",
                [id],
            )
            .map_err(map_sql)?;
        } else {
            tx.execute(
                "INSERT INTO iptv_channel_overrides
                (channel_id,display_name_override,group_mode,group_override,
                 language_mode,country_mode,region_mode,region_code_override,region_label_override,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(channel_id) DO UPDATE SET
                 display_name_override=excluded.display_name_override,
                 group_mode=excluded.group_mode,group_override=excluded.group_override,
                 language_mode=excluded.language_mode,country_mode=excluded.country_mode,
                 region_mode=excluded.region_mode,region_code_override=excluded.region_code_override,
                 region_label_override=excluded.region_label_override,updated_at=excluded.updated_at",
                params![
                    id, edits.display_name, edits.group.mode.sql(), edits.group.value,
                    edits.languages.mode.sql(), edits.countries.mode.sql(), edits.region.mode.sql(),
                    edits.region.value.as_ref().and_then(|r|r.code.as_ref()),
                    edits.region.value.as_ref().and_then(|r|r.label.as_ref()), at,
                ],
            ).map_err(map_sql)?;
            for (kind, field) in [
                (FacetDimension::Language, &edits.languages),
                (FacetDimension::Country, &edits.countries),
            ] {
                if field.mode == FacetMode::Replace {
                    for value in &field.values {
                        tx.execute("INSERT INTO iptv_channel_override_facets (channel_id,facet,value) VALUES(?1,?2,?3)",
                            params![id, kind.sql(), value]).map_err(map_sql)?;
                    }
                }
            }
        }
        tx.commit().map_err(map_sql)
    }

    pub fn clear_channel_overrides(&self, id: &str) -> StorageResult<()> {
        self.set_channel_overrides(id, &ChannelOverrides::default(), 0)
    }

    /// Only fixed, sanitized codes are stored; status writes do not bump the CAS revision.
    pub fn record_refresh_failure(&self, id: &str, code: ErrorCode, at: i64) -> StorageResult<()> {
        let conn = self.connect()?;
        if conn
            .execute(
                "UPDATE iptv_sources SET last_attempt_at=?1,last_error_code=?2 WHERE id=?3",
                params![at, code.sql(), id],
            )
            .map_err(map_sql)?
            == 0
        {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub fn purge_expired_missing(&self, now: i64) -> StorageResult<usize> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM iptv_channels WHERE is_present=0 AND missing_since <= ?1
                AND NOT EXISTS (SELECT 1 FROM iptv_channel_overrides o WHERE o.channel_id=iptv_channels.id)
                AND NOT EXISTS (SELECT 1 FROM iptv_channel_override_facets f WHERE f.channel_id=iptv_channels.id)",
            [now.saturating_sub(90 * 24 * 3600)],
        ).map_err(map_sql)
    }

    pub fn get_channel_detail(&self, id: &str) -> StorageResult<ChannelDetail> {
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(map_sql)?;
        let detail = load_channel_detail(&tx, id)?;
        tx.commit().map_err(map_sql)?;
        Ok(detail)
    }
}
impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

/// App-level deletion requires explicit confirmation tied to a currently displayed source.
#[derive(Clone, Debug)]
pub struct ConfirmedSourceDeletion {
    id: SourceId,
    revision: i64,
}
impl ConfirmedSourceDeletion {
    pub fn after_user_confirmation(source: &Source) -> Self {
        Self {
            id: source.id.clone(),
            revision: source.revision,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Network,
    InvalidPlaylist,
    Permission,
    Reauthorization,
    Cancelled,
    Write,
}
impl ErrorCode {
    fn sql(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::InvalidPlaylist => "invalid_playlist",
            Self::Permission => "permission",
            Self::Reauthorization => "reauthorization",
            Self::Cancelled => "cancelled",
            Self::Write => "write",
        }
    }
}

/// Durability checkpoints are injectable only by tests. A failed initialization
/// never deletes or replaces an existing final database, even if a competitor
/// publishes it while our private staging file is being prepared.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InitCheckpoint {
    TempCreated,
    MigrationComplete,
    BeforePublish,
    AfterPublish,
}

fn initialization_checkpoint(
    failure: Option<InitCheckpoint>,
    stage: InitCheckpoint,
) -> StorageResult<()> {
    if failure == Some(stage) {
        Err(StorageError::StorageFailure)
    } else {
        Ok(())
    }
}

fn validate_existing(path: &Path) -> StorageResult<()> {
    require_private_file(path)?;
    // Probe read-only first: foreign, corrupt and version-zero DBs remain untouched.
    let probe = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    let id: i64 = probe
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(|_| StorageError::CorruptDatabase)?;
    let version: i64 = probe
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|_| StorageError::CorruptDatabase)?;
    if id != APPLICATION_ID || !(version == 1 || version == SCHEMA_VERSION) {
        return Err(StorageError::IncompatibleDatabase);
    }
    validate_v1_schema_shape(&probe)?;
    if version == SCHEMA_VERSION {
        validate_v2_schema_shape(&probe)?;
    }
    validate_integrity(&probe)?;
    drop(probe);
    if version == 1 {
        // The original remains v1 on any failed migration. A durable, independent
        // SQLite-consistent v1 recovery snapshot is required before the write.
        upgrade_existing_v1(path)?;
        let reopened = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| StorageError::StorageFailure)?;
        validate_v1_schema_shape(&reopened)?;
        validate_v2_schema_shape(&reopened)?;
        validate_integrity(&reopened)?;
        let version: i64 = reopened
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(map_sql)?;
        if version != SCHEMA_VERSION {
            return Err(StorageError::IncompatibleDatabase);
        }
    }
    Ok(())
}

/// Upgrade a verified v1 database without replacing its inode or rewriting its
/// published v1 migration. Preserve an independently reopenable recovery copy.
/// This function must only run on a blocking worker, never the GTK thread.
fn upgrade_existing_v1(path: &Path) -> StorageResult<()> {
    upgrade_existing_v1_with(path, MIGRATION_002, false)
}

fn upgrade_existing_v1_with(
    path: &Path,
    migration_sql: &str,
    inject_backup_failure: bool,
) -> StorageResult<()> {
    let parent = path.parent().ok_or(StorageError::InvalidInput)?;
    let name = path.file_name().ok_or(StorageError::InvalidInput)?;
    let backup_path = parent.join(format!(
        ".{}-v1-recovery-{}.sqlite3",
        name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let staged_file = opts
        .open(&backup_path)
        .map_err(|_| StorageError::PermissionDenied)?;
    drop(staged_file);

    let backup_result = (|| -> StorageResult<()> {
        if inject_backup_failure {
            return Err(StorageError::StorageFailure);
        }
        let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| StorageError::StorageFailure)?;
        configure(&source)?;
        let mut dest = Connection::open_with_flags(&backup_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| StorageError::StorageFailure)?;
        {
            let backup = Backup::new(&source, &mut dest).map_err(map_sql)?;
            backup
                .run_to_completion(128, Duration::from_millis(25), None)
                .map_err(map_sql)?;
        }
        let application_id: i64 = dest
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .map_err(map_sql)?;
        let version: i64 = dest
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(map_sql)?;
        if application_id != APPLICATION_ID || version != 1 {
            return Err(StorageError::IncompatibleDatabase);
        }
        validate_v1_schema_shape(&dest)?;
        validate_integrity(&dest)?;
        dest.close().map_err(|_| StorageError::StorageFailure)?;
        fs::File::open(&backup_path)
            .and_then(|f| f.sync_all())
            .map_err(|_| StorageError::StorageFailure)?;
        sync_parent_directory(parent).map_err(|_| StorageError::StorageFailure)?;
        Ok(())
    })();
    if let Err(error) = backup_result {
        // Only our own uniquely created, incomplete backup may be discarded.
        let _ = fs::remove_file(&backup_path);
        return Err(error);
    }

    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| StorageError::StorageFailure)?;
    configure(&conn)?;
    let id: i64 = conn
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(map_sql)?;
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(map_sql)?;
    if id != APPLICATION_ID || version != 1 {
        return Err(StorageError::IncompatibleDatabase);
    }
    validate_v1_schema_shape(&conn)?;
    validate_integrity(&conn)?;
    conn.execute_batch(migration_sql).map_err(|_| {
        let _ = conn.execute_batch("ROLLBACK");
        StorageError::StorageFailure
    })?;
    validate_v2_schema_shape(&conn)?;
    validate_integrity(&conn)?;
    // The recovery copy is intentionally retained, never silently deleted.
    // A failure after commit leaves the copy available for operator recovery.
    Ok(())
}

/// Own only the random temporary inode, never the final database. On failures
/// or panics, clean our own staging files as best effort, never other files.
struct StagedDatabase {
    path: PathBuf,
}
impl Drop for StagedDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut name = self.path.as_os_str().to_os_string();
            name.push(suffix);
            let _ = fs::remove_file(PathBuf::from(name));
        }
    }
}

fn sync_parent_directory(parent: &Path) -> std::io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

fn initialize_new(path: &Path, fail_at: Option<InitCheckpoint>) -> StorageResult<()> {
    initialize_new_with(path, fail_at, MIGRATION_001, &sync_parent_directory)
}

// Keep the injection at the actual migration and directory-fsync calls.
// Production always uses the authoritative migration and real fsync; fixtures
// can substitute a failing migration or an EIO result from the same boundary.
fn initialize_new_with(
    path: &Path,
    fail_at: Option<InitCheckpoint>,
    migration_sql: &str,
    sync_parent: &dyn Fn(&Path) -> std::io::Result<()>,
) -> StorageResult<()> {
    #[cfg(not(unix))]
    {
        let _ = (path, fail_at, migration_sql, sync_parent);
        // No replacement-safe and durably synced publisher has been verified
        // for non-Unix filesystems. Do not create an unsafe final database.
        Err(StorageError::StorageFailure)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let parent = path.parent().ok_or(StorageError::InvalidInput)?;
        let name = path.file_name().ok_or(StorageError::InvalidInput)?;
        // The temporary basename is both unpredictable and unique, with
        // create_new preventing reuse of a leftover or attacker-made file.
        let temp_path = parent.join(format!(
            ".{}-staging-{}",
            name.to_string_lossy(),
            uuid::Uuid::new_v4()
        ));
        let mut creator = fs::OpenOptions::new();
        let created = creator
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)
            .map_err(|_| StorageError::PermissionDenied)?;
        let staged = StagedDatabase {
            path: temp_path.clone(),
        };
        initialization_checkpoint(fail_at, InitCheckpoint::TempCreated)?;
        drop(created);

        let connection = Connection::open(&temp_path).map_err(|_| StorageError::StorageFailure)?;
        configure(&connection)?;
        connection.execute_batch(migration_sql).map_err(|_| {
            let _ = connection.execute_batch("ROLLBACK");
            StorageError::StorageFailure
        })?;
        // A new DB is staged from the unmodified published v1 initializer and
        // immediately evolved to v2 before being atomically published.
        let initial_version: i64 = connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(map_sql)?;
        if initial_version == 1 {
            connection.execute_batch(MIGRATION_002).map_err(|_| {
                let _ = connection.execute_batch("ROLLBACK");
                StorageError::StorageFailure
            })?;
        }
        validate_v1_schema_shape(&connection)?;
        validate_v2_schema_shape(&connection)?;
        validate_integrity(&connection)?;
        let id: i64 = connection
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .map_err(map_sql)?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(map_sql)?;
        if id != APPLICATION_ID || version != SCHEMA_VERSION {
            return Err(StorageError::IncompatibleDatabase);
        }
        // A live connection or SQLite sidecar must not be renamed/published.
        connection
            .close()
            .map_err(|_| StorageError::StorageFailure)?;
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut name = temp_path.as_os_str().to_os_string();
            name.push(suffix);
            if fs::symlink_metadata(PathBuf::from(name)).is_ok() {
                return Err(StorageError::StorageFailure);
            }
        }
        initialization_checkpoint(fail_at, InitCheckpoint::MigrationComplete)?;

        // Persist the initialized inode and staged name before publication.
        fs::File::open(&temp_path)
            .and_then(|file| file.sync_all())
            .map_err(|_| StorageError::StorageFailure)?;
        sync_parent(parent).map_err(|_| StorageError::StorageFailure)?;
        initialization_checkpoint(fail_at, InitCheckpoint::BeforePublish)?;

        // Linux hard links provide an atomic no-replace name publication.
        // Existing final files (including symlinks and empty v0 files) cannot
        // be overwritten, and unsupported hardlink filesystems fail closed.
        match fs::hard_link(&temp_path, path) {
            Ok(()) => {
                // Final directory durability is mandatory. If sync fails,
                // return failure but DO NOT remove the now-published DB.
                sync_parent(parent).map_err(|_| StorageError::StorageFailure)?;
                initialization_checkpoint(fail_at, InitCheckpoint::AfterPublish)?;
                drop(staged);
                sync_parent(parent).map_err(|_| StorageError::StorageFailure)?;
                validate_existing(path)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Another initializer won the race; never delete/replace it.
                drop(staged);
                validate_existing(path)
            }
            Err(_) => Err(StorageError::StorageFailure),
        }
    }
}

fn configure(conn: &Connection) -> StorageResult<()> {
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(map_sql)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(map_sql)?;
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .map_err(map_sql)?;
    if fk != 1 {
        return Err(StorageError::StorageFailure);
    }
    Ok(())
}
fn validate_v1_schema_shape(conn: &Connection) -> StorageResult<()> {
    const TABLES: &[&str] = &[
        "iptv_channel_imported_facets",
        "iptv_channel_override_facets",
        "iptv_channel_overrides",
        "iptv_channels",
        "iptv_source_default_facets",
        "iptv_sources",
    ];
    const INDEXES: &[&str] = &[
        "idx_iptv_channels_missing",
        "idx_iptv_channels_source_group",
        "idx_iptv_channels_source_present_order",
        "idx_iptv_channels_source_tvgid",
        "idx_iptv_imported_facet_filter",
        "idx_iptv_override_facet_filter",
        "idx_iptv_source_default_facet_filter",
        "idx_iptv_sources_enabled_order",
    ];
    for (kind, prefix, expected) in [
        ("table", "iptv_%", TABLES),
        ("index", "idx_iptv_%", INDEXES),
    ] {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type=?1 AND name LIKE ?2 ORDER BY name",
            )
            .map_err(|_| StorageError::IncompatibleDatabase)?;
        let actual = stmt
            .query_map(params![kind, prefix], |row| row.get::<_, String>(0))
            .map_err(|_| StorageError::IncompatibleDatabase)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| StorageError::IncompatibleDatabase)?;
        if !actual
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
        {
            return Err(StorageError::IncompatibleDatabase);
        }
    }
    validate_reference_schema(conn, TABLES, false)
}

fn validate_v2_schema_shape(conn: &Connection) -> StorageResult<()> {
    const TABLES: &[&str] = &[
        "epg_channel_overrides",
        "epg_channels",
        "epg_global_id_aliases",
        "epg_playlist_links",
        "epg_programmes",
        "epg_settings",
        "epg_sources",
    ];
    const INDEXES: &[&str] = &[
        "idx_epg_aliases_target",
        "idx_epg_override_guide",
        "idx_epg_playlist_guide",
        "idx_epg_programmes_now",
        "idx_epg_programmes_retention",
        "idx_epg_programmes_window_stop",
        "idx_epg_sources_enabled",
    ];
    for (kind, pattern, expected) in [("table", "epg_%", TABLES), ("index", "idx_epg_%", INDEXES)] {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type=?1 AND name LIKE ?2 ORDER BY name")
            .map_err(|_| StorageError::IncompatibleDatabase)?;
        let actual = stmt
            .query_map(params![kind, pattern], |row| row.get::<_, String>(0))
            .map_err(|_| StorageError::IncompatibleDatabase)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| StorageError::IncompatibleDatabase)?;
        if !actual
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
        {
            return Err(StorageError::IncompatibleDatabase);
        }
    }
    validate_reference_schema(conn, TABLES, true)
}

/// Check structural SQLite invariants against the actual published initializer
/// and approved v2 SQL. This uses SQLite's parsed schema (columns, keys, FK
/// actions, index columns/collations and uniqueness) rather than comparing
/// entire CREATE statements or trusting object names alone.
fn validate_reference_schema(
    actual: &Connection,
    tables: &[&str],
    include_epg: bool,
) -> StorageResult<()> {
    let reference = Connection::open_in_memory().map_err(|_| StorageError::IncompatibleDatabase)?;
    reference
        .execute_batch(MIGRATION_001)
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    if include_epg {
        reference
            .execute_batch(MIGRATION_002)
            .map_err(|_| StorageError::IncompatibleDatabase)?;
    }
    for table in tables {
        if table_shape(actual, table)? != table_shape(&reference, table)? {
            return Err(StorageError::IncompatibleDatabase);
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
struct TableShape {
    columns: Vec<Vec<rusqlite::types::Value>>,
    foreign_keys: Vec<Vec<rusqlite::types::Value>>,
    checks: Vec<String>,
    indexes: Vec<String>,
}

fn quoted_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn pragma_rows(
    conn: &Connection,
    statement: &str,
    count: usize,
) -> StorageResult<Vec<Vec<rusqlite::types::Value>>> {
    let mut stmt = conn
        .prepare(statement)
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    let rows = stmt
        .query_map([], |row| {
            (0..count)
                .map(|i| row.get::<_, rusqlite::types::Value>(i))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| StorageError::IncompatibleDatabase)
}

/// Normalise whitespace and SQL keyword case, not quoted string literal data.
fn normalise_sql(sql: &str) -> String {
    let mut out = String::new();
    let mut literal = false;
    for c in sql.chars() {
        if c == '\'' {
            literal = !literal;
            out.push(c);
        } else if literal {
            out.push(c);
        } else if !c.is_whitespace() {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Extract balanced CHECK expressions, keeping quoted literals verbatim.
/// Whole-statement text comparisons reject harmless SQL formatting variants.
fn check_clauses(sql: &str) -> StorageResult<Vec<String>> {
    let bytes = sql.as_bytes();
    let mut clauses = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if i + 5 > bytes.len()
            || !bytes[i..i + 5].eq_ignore_ascii_case(b"check")
            || (i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
            || bytes
                .get(i + 5)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            i += 1;
            continue;
        }
        let mut start = i + 5;
        while bytes.get(start).is_some_and(|c| c.is_ascii_whitespace()) {
            start += 1;
        }
        if bytes.get(start) != Some(&b'(') {
            i += 1;
            continue;
        }
        let mut cursor = start;
        let mut depth = 0i32;
        let mut literal = false;
        loop {
            match bytes.get(cursor) {
                Some(b'\'') => {
                    if literal && bytes.get(cursor + 1) == Some(&b'\'') {
                        cursor += 1;
                    } else {
                        literal = !literal;
                    }
                }
                Some(b'(') if !literal => depth += 1,
                Some(b')') if !literal => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                None => return Err(StorageError::IncompatibleDatabase),
                _ => {}
            }
            cursor += 1;
        }
        clauses.push(normalise_sql(&sql[start + 1..cursor]));
        i = cursor + 1;
    }
    clauses.sort();
    Ok(clauses)
}

fn table_shape(conn: &Connection, table: &str) -> StorageResult<TableShape> {
    let escaped = quoted_identifier(table);
    let columns = pragma_rows(conn, &format!("PRAGMA table_xinfo({escaped})"), 7)?;
    if columns.is_empty() {
        return Err(StorageError::IncompatibleDatabase);
    }
    let foreign_keys = pragma_rows(conn, &format!("PRAGMA foreign_key_list({escaped})"), 8)?;
    let definition: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    let checks = check_clauses(&definition)?;
    let mut stmt = conn
        .prepare(&format!("PRAGMA index_list({escaped})"))
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    let listed = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|_| StorageError::IncompatibleDatabase)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| StorageError::IncompatibleDatabase)?;
    let mut indexes = Vec::new();
    for (name, unique, origin, partial) in listed {
        let entries = pragma_rows(
            conn,
            &format!("PRAGMA index_xinfo({})", quoted_identifier(&name)),
            6,
        )?;
        let predicate = if partial != 0 {
            let ddl: String = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type='index' AND name=?1",
                    [&name],
                    |row| row.get(0),
                )
                .map_err(|_| StorageError::IncompatibleDatabase)?;
            normalise_sql(&ddl)
                .split_once("where")
                .map(|(_, clause)| clause.to_owned())
                .ok_or(StorageError::IncompatibleDatabase)?
        } else {
            String::new()
        };
        // Automatic index names are SQLite implementation details; compare
        // their actual key/uniqueness semantics rather than incidental names.
        let explicit_name = if origin == "c" { name.as_str() } else { "" };
        indexes.push(format!(
            "{explicit_name:?}|{unique}|{origin}|{partial}|{entries:?}|{predicate}"
        ));
    }
    indexes.sort();
    Ok(TableShape {
        columns,
        foreign_keys,
        checks,
        indexes,
    })
}

fn validate_integrity(conn: &Connection) -> StorageResult<()> {
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|_| StorageError::CorruptDatabase)?;
    if integrity != "ok" {
        return Err(StorageError::CorruptDatabase);
    }
    let bad: Option<String> = conn
        .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
        .optional()
        .map_err(|_| StorageError::CorruptDatabase)?;
    if bad.is_some() {
        return Err(StorageError::CorruptDatabase);
    }
    Ok(())
}
fn ensure_private_parent(path: &Path) -> StorageResult<()> {
    let parent = path.parent().ok_or(StorageError::InvalidInput)?;
    if !parent.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|_| StorageError::PermissionDenied)?;
        }
        #[cfg(not(unix))]
        {
            fs::create_dir_all(parent).map_err(|_| StorageError::PermissionDenied)?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::symlink_metadata(parent).map_err(|_| StorageError::PermissionDenied)?;
        if !meta.file_type().is_dir()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != unsafe { geteuid() }
        {
            return Err(StorageError::PermissionDenied);
        }
    }
    Ok(())
}
#[cfg(unix)]
extern "C" {
    fn geteuid() -> u32;
}
fn require_private_file(path: &Path) -> StorageResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::symlink_metadata(path).map_err(|_| StorageError::PermissionDenied)?;
        if !meta.file_type().is_file()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != unsafe { geteuid() }
        {
            return Err(StorageError::PermissionDenied);
        }
    }
    #[cfg(not(unix))]
    {
        if !path.is_file() {
            return Err(StorageError::PermissionDenied);
        }
    }
    Ok(())
}

pub(crate) fn load_source(conn: &Connection, id: &str) -> StorageResult<Source> {
    let source = conn
        .query_row(
            "SELECT name,origin_kind,origin_locator,enabled,source_order,
                default_region_code,default_region_label,revision,created_at,updated_at,
                last_attempt_at,last_success_at,last_error_code FROM iptv_sources WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get::<_, u32>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                    r.get::<_, Option<i64>>(11)?,
                    r.get::<_, Option<String>>(12)?,
                ))
            },
        )
        .optional()
        .map_err(map_sql)?
        .ok_or(StorageError::NotFound)?;
    let (
        name,
        kind,
        locator,
        enabled,
        order,
        code,
        label,
        revision,
        created_at,
        updated_at,
        last_attempt_at,
        last_success_at,
        last_error_code,
    ) = source;
    Ok(Source {
        id: id.to_owned(),
        name,
        origin: SourceOrigin {
            kind: OriginKind::from_sql(&kind)?,
            locator,
        },
        enabled,
        order,
        defaults: SourceDefaults {
            languages: load_facets(
                conn,
                "iptv_source_default_facets",
                "source_id",
                id,
                FacetDimension::Language,
            )?,
            countries: load_facets(
                conn,
                "iptv_source_default_facets",
                "source_id",
                id,
                FacetDimension::Country,
            )?,
            region: if code.is_some() || label.is_some() {
                Some(Region { code, label })
            } else {
                None
            },
        },
        revision,
        created_at,
        updated_at,
        last_attempt_at,
        last_success_at,
        last_error_code,
    })
}
pub(crate) fn load_facets(
    conn: &Connection,
    table: &str,
    id_col: &str,
    id: &str,
    facet: FacetDimension,
) -> StorageResult<BTreeSet<String>> {
    let sql = format!("SELECT value FROM {table} WHERE {id_col}=?1 AND facet=?2 ORDER BY value");
    let mut stmt = conn.prepare(&sql).map_err(map_sql)?;
    let rows = stmt
        .query_map(params![id, facet.sql()], |r| r.get::<_, String>(0))
        .map_err(map_sql)?;
    rows.collect::<rusqlite::Result<BTreeSet<_>>>()
        .map_err(map_sql)
}
pub(crate) fn load_channel_detail(conn: &Connection, id: &str) -> StorageResult<ChannelDetail> {
    let raw = conn
        .query_row(
            "SELECT c.source_id,c.playlist_order,c.imported_name,c.stream_url,c.tvg_id,c.tvg_name,
                c.group_title,c.logo_url,c.channel_number,c.imported_region_code,
                c.imported_region_label,c.user_agent,c.http_referer
         FROM iptv_channels c WHERE c.id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, Option<String>>(11)?,
                    r.get::<_, Option<String>>(12)?,
                ))
            },
        )
        .optional()
        .map_err(map_sql)?
        .ok_or(StorageError::NotFound)?;
    let (
        source_id,
        order,
        imported_name,
        stream_url,
        tvg_id,
        tvg_name,
        group_title,
        logo_url,
        channel_number,
        region_code,
        region_label,
        user_agent,
        referer,
    ) = raw;
    let source = load_source(conn, &source_id)?;
    let imported = IncomingChannel {
        order,
        imported_name: imported_name.clone(),
        stream_url: stream_url.clone(),
        tvg_id,
        tvg_name,
        group_title: group_title.clone(),
        logo_url: logo_url.clone(),
        channel_number: channel_number.clone(),
        region: if region_code.is_some() || region_label.is_some() {
            Some(Region {
                code: region_code,
                label: region_label,
            })
        } else {
            None
        },
        languages: load_facets(
            conn,
            "iptv_channel_imported_facets",
            "channel_id",
            id,
            FacetDimension::Language,
        )?,
        countries: load_facets(
            conn,
            "iptv_channel_imported_facets",
            "channel_id",
            id,
            FacetDimension::Country,
        )?,
        user_agent,
        referer,
    };
    let values = conn
        .query_row(
            "SELECT display_name_override,group_mode,group_override,language_mode,country_mode,
                region_mode,region_code_override,region_label_override
         FROM iptv_channel_overrides WHERE channel_id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()
        .map_err(map_sql)?;
    let overrides = if let Some((
        display_name,
        group_mode,
        group_value,
        lang_mode,
        country_mode,
        region_mode,
        code,
        label,
    )) = values
    {
        ChannelOverrides {
            display_name,
            group: GroupOverride {
                mode: FacetMode::from_sql(&group_mode)?,
                value: group_value,
            },
            languages: SetOverride {
                mode: FacetMode::from_sql(&lang_mode)?,
                values: load_facets(
                    conn,
                    "iptv_channel_override_facets",
                    "channel_id",
                    id,
                    FacetDimension::Language,
                )?,
            },
            countries: SetOverride {
                mode: FacetMode::from_sql(&country_mode)?,
                values: load_facets(
                    conn,
                    "iptv_channel_override_facets",
                    "channel_id",
                    id,
                    FacetDimension::Country,
                )?,
            },
            region: RegionOverride {
                mode: FacetMode::from_sql(&region_mode)?,
                value: if code.is_some() || label.is_some() {
                    Some(Region { code, label })
                } else {
                    None
                },
            },
        }
    } else {
        ChannelOverrides::default()
    };

    let (visible_name, name_from) = if let Some(name) = &overrides.display_name {
        (name.clone(), FieldOrigin::Manual)
    } else {
        (imported_name, FieldOrigin::Imported)
    };
    let (group, group_from) = match overrides.group.mode {
        FacetMode::Replace => (overrides.group.value.clone(), FieldOrigin::Manual),
        FacetMode::Unknown => (None, FieldOrigin::Unknown),
        FacetMode::Auto if group_title.is_some() => (group_title, FieldOrigin::Imported),
        _ => (None, FieldOrigin::Unknown),
    };
    let (region, region_from) = match overrides.region.mode {
        FacetMode::Replace => (overrides.region.value.clone(), FieldOrigin::Manual),
        FacetMode::Unknown => (None, FieldOrigin::Unknown),
        FacetMode::Auto if imported.region.is_some() => {
            (imported.region.clone(), FieldOrigin::Imported)
        }
        FacetMode::Auto if source.defaults.region.is_some() => {
            (source.defaults.region.clone(), FieldOrigin::SourceFallback)
        }
        _ => (None, FieldOrigin::Unknown),
    };
    let (languages, languages_from) = effective_set(
        &overrides.languages,
        &imported.languages,
        &source.defaults.languages,
    );
    let (countries, countries_from) = effective_set(
        &overrides.countries,
        &imported.countries,
        &source.defaults.countries,
    );
    let effective = ChannelEffective {
        id: id.to_owned(),
        source_id,
        source_name: source.name,
        visible_name,
        stream_url,
        group,
        languages,
        countries,
        region,
        logo_url,
        channel_number,
        has_user_overrides: overrides.has_values(),
    };
    Ok(ChannelDetail {
        effective,
        imported,
        source_defaults: source.defaults,
        overrides,
        provenance: EditProvenance {
            name: name_from,
            group: group_from,
            languages: languages_from,
            countries: countries_from,
            region: region_from,
        },
    })
}
fn effective_set(
    manual: &SetOverride,
    imported: &BTreeSet<String>,
    fallback: &BTreeSet<String>,
) -> (BTreeSet<String>, FieldOrigin) {
    match manual.mode {
        FacetMode::Replace => (manual.values.clone(), FieldOrigin::Manual),
        FacetMode::Unknown => (BTreeSet::new(), FieldOrigin::Unknown),
        FacetMode::Auto if !imported.is_empty() => (imported.clone(), FieldOrigin::Imported),
        FacetMode::Auto if !fallback.is_empty() => (fallback.clone(), FieldOrigin::SourceFallback),
        _ => (BTreeSet::new(), FieldOrigin::Unknown),
    }
}

#[cfg(test)]
mod staged_database_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    struct PrivateDir(PathBuf);
    impl PrivateDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("broadcaster-db-stage-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }

        fn database(&self) -> PathBuf {
            self.0.join("catalogue.sqlite3")
        }

        fn entries(&self) -> Vec<String> {
            fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        }
    }
    impl Drop for PrivateDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn first_initialization_is_durable_private_and_reopenable() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = PrivateDir::new();
        let path = fixture.database();
        let storage = Storage::at(&path);
        storage.initialize().unwrap();
        let entries = fixture.entries();
        assert_eq!(entries, vec!["catalogue.sqlite3"]);
        let permissions = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(permissions, 0o600);
        storage.initialize().unwrap();
        assert!(storage.list_sources().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn injected_staging_failures_leave_no_final_database_or_own_temp() {
        for checkpoint in [
            InitCheckpoint::TempCreated,
            InitCheckpoint::MigrationComplete,
            InitCheckpoint::BeforePublish,
        ] {
            let fixture = PrivateDir::new();
            let path = fixture.database();
            let error = initialize_new(&path, Some(checkpoint)).unwrap_err();
            assert_eq!(error, StorageError::StorageFailure);
            assert!(
                !path.exists(),
                "failed stage unexpectedly published the database"
            );
            assert!(
                fixture.entries().is_empty(),
                "failed stage leaked staging files"
            );
            Storage::at(path).initialize().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_migration_transaction_cannot_publish_partial_schema() {
        let fixture = PrivateDir::new();
        let path = fixture.database();
        // Deliberately fail an actual SQLite statement AFTER BEGIN and a
        // successful CREATE, not merely at a checkpoint before migration.
        const FAILING_SQL: &str = "PRAGMA foreign_keys=ON; BEGIN IMMEDIATE;
            CREATE TABLE partial_stage_data(v INTEGER);
            INSERT INTO table_that_does_not_exist VALUES (7);
            COMMIT;";
        let error = initialize_new_with(&path, None, FAILING_SQL, &sync_parent_directory);
        assert_eq!(error, Err(StorageError::StorageFailure));
        assert!(!path.exists(), "failed migration published a partial DB");
        assert!(
            fixture.entries().is_empty(),
            "failed migration left a sidecar"
        );
        // A valid subsequent attempt must succeed with unchanged v1 SQL.
        Storage::at(&path).initialize().unwrap();
        assert_eq!(fixture.entries(), vec!["catalogue.sqlite3"]);
    }

    #[cfg(unix)]
    #[test]
    fn parent_sync_io_failure_preserves_publication_boundary() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        for fail_on_sync in [1, 2, 3] {
            let fixture = PrivateDir::new();
            let path = fixture.database();
            let sync_calls = AtomicUsize::new(0);
            let injected_sync = |parent: &Path| -> std::io::Result<()> {
                let call = sync_calls.fetch_add(1, Ordering::SeqCst) + 1;
                if call == fail_on_sync {
                    Err(std::io::Error::from_raw_os_error(5)) // synthetic EIO
                } else {
                    sync_parent_directory(parent)
                }
            };
            let error = initialize_new_with(&path, None, MIGRATION_001, &injected_sync);
            assert_eq!(error, Err(StorageError::StorageFailure));
            assert_eq!(sync_calls.load(Ordering::SeqCst), fail_on_sync);
            if fail_on_sync == 1 {
                assert!(!path.exists(), "prepublish fsync error exposed a final DB");
                assert!(fixture.entries().is_empty(), "staging file not removed");
                Storage::at(&path).initialize().unwrap();
            } else {
                // Once the no-replace hard link exists, it is user data. Even
                // a subsequent fsync error must never delete or replace it.
                assert!(path.exists(), "published data was deleted on EIO");
                Storage::at(&path).initialize().unwrap();
                assert_eq!(fixture.entries(), vec!["catalogue.sqlite3"]);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn failure_after_no_replace_publish_preserves_valid_database() {
        let fixture = PrivateDir::new();
        let path = fixture.database();
        let error = initialize_new(&path, Some(InitCheckpoint::AfterPublish)).unwrap_err();
        assert_eq!(error, StorageError::StorageFailure);
        // After successful atomic publication no cleanup is allowed to remove
        // the durable winner, even if a later durability phase reports error.
        let bytes = fs::read(&path).unwrap();
        assert!(!bytes.is_empty());
        Storage::at(&path).initialize().unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fixture.entries(), vec!["catalogue.sqlite3"]);
    }

    #[cfg(unix)]
    #[test]
    fn invalid_and_legacy_existing_files_are_never_overwritten() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = PrivateDir::new();
        let path = fixture.database();
        let original = b"not a SQLite database - preserve all original user data";
        fs::write(&path, original).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            Storage::at(&path).initialize(),
            Err(StorageError::CorruptDatabase | StorageError::IncompatibleDatabase)
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(fixture.entries().len() == 1);

        fs::remove_file(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE user_original(v TEXT); INSERT INTO user_original VALUES('retain');",
        )
        .unwrap();
        drop(conn);
        // External rusqlite::Connection::open inherits the process umask
        // (typically 0644), so make the fixture private before testing the
        // version-zero refusal rather than tripping the earlier permission gate.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(matches!(
            Storage::at(&path).initialize(),
            Err(StorageError::IncompatibleDatabase)
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn external_final_creator_wins_without_byte_changes() {
        let fixture = PrivateDir::new();
        let path = fixture.database();
        let content = b"concurrent external user data";
        fs::write(&path, content).unwrap();
        let failure = initialize_new(&path, None);
        assert!(failure.is_err());
        assert_eq!(fs::read(&path).unwrap(), content);
        assert_eq!(fixture.entries(), vec!["catalogue.sqlite3"]);
    }

    #[cfg(unix)]
    #[test]
    fn simultaneous_initializers_cannot_replace_winning_database() {
        let fixture = PrivateDir::new();
        let path = fixture.database();
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let barrier = barrier.clone();
            let path = path.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                Storage::at(path).initialize()
            }));
        }
        barrier.wait();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        assert_eq!(fixture.entries(), vec!["catalogue.sqlite3"]);
        Storage::at(&path).initialize().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unrelated_stale_staging_file_is_not_deleted() {
        let fixture = PrivateDir::new();
        let stale = fixture.0.join(".catalogue.sqlite3-staging-previous-run");
        let payload = b"old, unknown file. must not be touched.";
        fs::write(&stale, payload).unwrap();
        Storage::at(fixture.database()).initialize().unwrap();
        assert_eq!(fs::read(&stale).unwrap(), payload);
    }

    #[cfg(unix)]
    #[test]
    fn injected_backup_and_post_create_migration_failures_preserve_v1() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = PrivateDir::new();
        let path = fixture.database();
        let db = Connection::open(&path).unwrap();
        db.execute_batch(MIGRATION_001).unwrap();
        db.execute(
            "INSERT INTO iptv_sources(id,name,origin_kind,origin_locator,created_at,updated_at)
             VALUES('p1','Fictional','local_file','/fake',1,1)",
            [],
        )
        .unwrap();
        drop(db);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let original = fs::read(&path).unwrap();

        // Refuse before the recoverable backup is complete, without touching v1.
        assert_eq!(
            upgrade_existing_v1_with(&path, MIGRATION_002, true),
            Err(StorageError::StorageFailure),
        );
        assert_eq!(fs::read(&path).unwrap(), original);

        // Execute all the real migration statements inside BEGIN IMMEDIATE,
        // then force a SQL error before COMMIT to verify transactional rollback.
        let injected = MIGRATION_002.replace(
            "PRAGMA user_version = 2;",
            "INSERT INTO nonexistent_epg_failure_point VALUES(7);\nPRAGMA user_version = 2;",
        );
        assert_eq!(
            upgrade_existing_v1_with(&path, &injected, false),
            Err(StorageError::StorageFailure),
        );
        let reopened = Connection::open(&path).unwrap();
        let version: i64 = reopened
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let preserved: String = reopened
            .query_row("SELECT name FROM iptv_sources WHERE id='p1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(preserved, "Fictional");
        let epg_exists: i64 = reopened
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='epg_sources'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(epg_exists, 0);
        drop(reopened);
        Storage::at(path).initialize().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_parent_permissions_refuse_database_creation() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = PrivateDir::new();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o777)).unwrap();
        let path = fixture.database();
        assert_eq!(
            Storage::at(&path).initialize(),
            Err(StorageError::PermissionDenied)
        );
        assert!(!path.exists());
    }
}

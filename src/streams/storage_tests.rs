// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Disposable on-disk migration fixtures using the actual published v1 SQL.
use super::storage::{
    Storage, APPLICATION_ID, MIGRATION_001, MIGRATION_002, MIGRATION_003, MIGRATION_004,
    MIGRATION_005, SCHEMA_VERSION,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::{fs, path::PathBuf};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "broadcaster-epg-migration-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("original.sqlite3")
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn seed_real_v1(path: &PathBuf) {
    seed_real_v1_with_sql(path, MIGRATION_001);
}

fn seed_real_v1_with_sql(path: &PathBuf, migration: &str) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(migration).unwrap();
    db.execute(
        "INSERT INTO iptv_sources
         (id,name,origin_kind,origin_locator,created_at,updated_at)
         VALUES (?1,?2,'local_file',?3,100,100)",
        params!["playlist-one", "Fictional playlist", "/fake/example.m3u"],
    )
    .unwrap();
    db.execute(
        "INSERT INTO iptv_channels
         (id,source_id,playlist_order,imported_name,stream_url,tvg_id,
          user_agent,http_referer,created_at,updated_at)
         VALUES (?1,?2,0,'Fictional Channel','https://invalid.example/one.m3u8',
                 'guide.fake','Example-Test-UA','https://invalid.example/',100,100)",
        params!["stream-one", "playlist-one"],
    )
    .unwrap();
    db.execute(
        "INSERT INTO iptv_channel_overrides
         (channel_id,display_name_override,updated_at)
         VALUES ('stream-one','Owner title',100)",
        [],
    )
    .unwrap();
    drop(db);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

// Regression sentinel: the bundled libsqlite3-sys upgrade must actually link
// SQLite 3.54.0, not accidentally fall back to the host's SQLite library.
#[test]
fn bundled_sqlite_runtime_is_3_54_1() {
    let db = Connection::open_in_memory().unwrap();
    let version: String = db
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, "3.54.1");
}

#[test]
fn clean_new_database_is_schema_v5_after_reopening() {
    let dir = Sandbox::new();
    let storage = Storage::at(dir.path());
    storage.initialize().unwrap();
    let conn = storage.connect().unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let id: i64 = conn
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(id, APPLICATION_ID);
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN
         ('epg_sources','epg_channels','epg_programmes','epg_playlist_links',
          'epg_channel_overrides','epg_settings','epg_global_id_aliases')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 7);
}

#[cfg(unix)]
#[test]
fn upgrade_actual_v1_preserves_iptv_and_separates_shared_guide_cascade() {
    let dir = Sandbox::new();
    let path = dir.path();
    seed_real_v1(&path);
    let storage = Storage::at(path.clone());
    storage.initialize().unwrap();
    let conn = storage.connect().unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let retained: (String, String, String, String) = conn
        .query_row(
            "SELECT c.stream_url,c.user_agent,c.http_referer,o.display_name_override
         FROM iptv_channels c JOIN iptv_channel_overrides o ON o.channel_id=c.id
         WHERE c.id='stream-one'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(retained.3, "Owner title");
    assert!(retained.0.starts_with("https://invalid.example/"));
    assert_eq!(retained.1, "Example-Test-UA");
    assert_eq!(retained.2, "https://invalid.example/");

    // A valid independent SQLite recovery copy was made BEFORE altering v1.
    let backups = fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("-v1-recovery-")
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    let backup =
        Connection::open_with_flags(&backups[0], rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let original_version: i64 = backup
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(original_version, 1);
    let original_override: String = backup.query_row(
        "SELECT display_name_override FROM iptv_channel_overrides WHERE channel_id='stream-one'",
        [], |r|r.get(0),
    ).unwrap();
    assert_eq!(original_override, "Owner title");
    drop(backup);

    conn.execute(
        "INSERT INTO epg_sources
         (id,name,origin_kind,origin_locator,created_at,updated_at)
         VALUES ('guide-one','Shared fictional guide','local_file','/fake/guide.xml',100,100)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO epg_global_id_aliases
         (guide_source_id,iptv_tvg_id,guide_channel_id,updated_at)
         VALUES ('guide-one','guide.fake','xml.fake',100)",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE epg_settings SET shared_guide_source_id='guide-one' WHERE singleton=1",
        [],
    )
    .unwrap();
    conn.execute("DELETE FROM iptv_sources WHERE id='playlist-one'", [])
        .unwrap();
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM epg_global_id_aliases WHERE guide_source_id='guide-one'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 1);
    let global: String = conn
        .query_row(
            "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(global, "guide-one");
    let fk: Option<String> = conn
        .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
        .optional()
        .unwrap();
    assert!(fk.is_none());
    drop(conn);
    storage.initialize().unwrap(); // v2 reopen and shape validation
}

#[cfg(unix)]
#[test]
fn foreign_or_version_zero_database_is_never_upgraded() {
    let dir = Sandbox::new();
    let path = dir.path();
    fs::write(&path, b"not a user-approved Broadcaster database").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let original = fs::read(&path).unwrap();
    assert!(Storage::at(path.clone()).initialize().is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn spoofed_v2_names_cannot_bypass_column_index_or_constraint_validation() {
    let dir = Sandbox::new();
    let path = dir.path();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(MIGRATION_001).unwrap();
    for table in [
        "epg_sources",
        "epg_channels",
        "epg_programmes",
        "epg_playlist_links",
        "epg_channel_overrides",
        "epg_settings",
        "epg_global_id_aliases",
    ] {
        conn.execute_batch(&format!("CREATE TABLE {table} (arbitrary INTEGER)"))
            .unwrap();
    }
    for index in [
        "idx_epg_aliases_target",
        "idx_epg_override_guide",
        "idx_epg_playlist_guide",
        "idx_epg_programmes_now",
        "idx_epg_programmes_retention",
        "idx_epg_programmes_window_stop",
        "idx_epg_sources_enabled",
    ] {
        conn.execute_batch(&format!("CREATE INDEX {index} ON epg_sources(arbitrary)"))
            .unwrap();
    }
    conn.execute_batch("PRAGMA user_version=2").unwrap();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    let fk_violations: Option<String> = conn
        .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
        .optional()
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(fk_violations.is_none());
    drop(conn);
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(Storage::at(path.clone()).initialize().is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn spoofed_v1_index_name_refuses_migration_without_modifying_original() {
    let dir = Sandbox::new();
    let path = dir.path();
    seed_real_v1(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP INDEX idx_iptv_channels_source_tvgid;
         CREATE INDEX idx_iptv_channels_source_tvgid ON iptv_channels(source_id);",
    )
    .unwrap();
    drop(conn);
    let before = fs::read(&path).unwrap();
    assert!(Storage::at(path.clone()).initialize().is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

fn replace_exact_once(sql: &str, expected: &str, replacement: &str) -> String {
    assert_eq!(
        sql.matches(expected).count(),
        1,
        "expected one SQL fixture anchor"
    );
    sql.replacen(expected, replacement, 1)
}

#[cfg(unix)]
#[test]
fn commented_out_v1_or_v2_checks_refuse_without_mutation_or_recovery_files() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let checked_id = "id TEXT NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64)";
    for line_comment in [false, true] {
        let unguarded_id = if line_comment {
            "id TEXT NOT NULL PRIMARY KEY -- CHECK (length(id) BETWEEN 1 AND 64)\n    "
        } else {
            "id TEXT NOT NULL PRIMARY KEY /* CHECK (length(id) BETWEEN 1 AND 64) */"
        };
        for spoof_v2 in [false, true] {
            let dir = Sandbox::new();
            let path = dir.path();
            if spoof_v2 {
                seed_real_v1(&path);
                let original = format!("CREATE TABLE epg_sources (\n    {checked_id},");
                let spoof = format!("CREATE TABLE epg_sources (\n    {unguarded_id},");
                let modified_v2 = replace_exact_once(MIGRATION_002, &original, &spoof);
                let db = Connection::open(&path).unwrap();
                db.execute_batch(&modified_v2).unwrap();
            } else {
                let original = format!("CREATE TABLE iptv_sources (\n    {checked_id},");
                let spoof = format!("CREATE TABLE iptv_sources (\n    {unguarded_id},");
                let modified_v1 = replace_exact_once(MIGRATION_001, &original, &spoof);
                seed_real_v1_with_sql(&path, &modified_v1);
            }
            let before = fs::read(&path).unwrap();
            let inode = fs::metadata(&path).unwrap().ino();
            assert!(Storage::at(path.clone()).initialize().is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
            assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
            assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
            let conn = Connection::open(&path).unwrap();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, if spoof_v2 { 2 } else { 1 });
            let retained: (String, String, String) = conn
                .query_row(
                    "SELECT c.user_agent,c.http_referer,o.display_name_override FROM iptv_channels c JOIN iptv_channel_overrides o ON o.channel_id=c.id WHERE c.id='stream-one'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(retained.0, "Example-Test-UA");
            assert_eq!(retained.1, "https://invalid.example/");
            assert_eq!(retained.2, "Owner title");
            drop(conn);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn actual_v1_check_with_internal_comment_still_upgrades_and_reopens() {
    let dir = Sandbox::new();
    let path = dir.path();
    let checked = "CHECK (length(id) BETWEEN 1 AND 64)";
    let decorated =
        "CHECK (length(id) /* quoted 'CHECK (wrong)' and -- are comments */ BETWEEN 1 AND 64)";
    let v1 = MIGRATION_001.replacen(checked, decorated, 1);
    seed_real_v1_with_sql(&path, &v1);
    let storage = Storage::at(path.clone());
    storage.initialize().unwrap();
    storage.initialize().unwrap();
    let conn = storage.connect().unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let retained: (String, String, String) = conn
        .query_row(
            "SELECT c.user_agent,c.http_referer,o.display_name_override FROM iptv_channels c JOIN iptv_channel_overrides o ON o.channel_id=c.id WHERE c.id='stream-one'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(retained.0, "Example-Test-UA");
    assert_eq!(retained.1, "https://invalid.example/");
    assert_eq!(retained.2, "Owner title");
    drop(conn);
    let backups = fs::read_dir(&dir.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("-v1-recovery-"))
        .count();
    assert_eq!(backups, 1);
}

#[cfg(unix)]
#[test]
fn epg_manual_override_survives_ninety_day_missing_purge_until_real_guide_deletion() {
    let dir = Sandbox::new();
    seed_real_v1(&dir.path());
    let storage = Storage::at(dir.path());
    storage.initialize().unwrap();
    let conn = storage.connect().unwrap();
    conn.execute(
        "INSERT INTO epg_sources
         (id,name,origin_kind,origin_locator,created_at,updated_at)
         VALUES ('fixture-guide','Synthetic guide','local_file','/fabricated/guide',1,1)",
        [],
    )
    .unwrap();
    for id in ["epg-mapped", "unmapped"] {
        conn.execute(
            "INSERT INTO iptv_channels
             (id,source_id,playlist_order,imported_name,stream_url,
              is_present,missing_since,created_at,updated_at)
             VALUES (?1,'playlist-one',2,'Archived',
                     'https://invalid.example/archived.m3u8',0,1,1,1)",
            [id],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO epg_channel_overrides
         (iptv_channel_id,guide_source_id,guide_channel_id,updated_at)
         VALUES ('epg-mapped','fixture-guide','region',1)",
        [],
    )
    .unwrap();
    drop(conn);
    assert_eq!(storage.purge_expired_missing(10_000_000).unwrap(), 1);
    let conn = storage.connect().unwrap();
    let mapped: String = conn
        .query_row(
            "SELECT stream_url FROM iptv_channels WHERE id='epg-mapped'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mapped, "https://invalid.example/archived.m3u8");
    let missing: Option<String> = conn
        .query_row(
            "SELECT id FROM iptv_channels WHERE id='unmapped'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    assert!(missing.is_none());
    conn.execute("DELETE FROM epg_sources WHERE id='fixture-guide'", [])
        .unwrap();
    let epg_overrides: i64 = conn
        .query_row("SELECT COUNT(*) FROM epg_channel_overrides", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(epg_overrides, 0);
    drop(conn);
    assert_eq!(storage.purge_expired_missing(10_000_000).unwrap(), 1);
    let conn = storage.connect().unwrap();
    let remaining: Option<String> = conn
        .query_row(
            "SELECT id FROM iptv_channels WHERE id='epg-mapped'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    assert!(remaining.is_none());
    // Existing editor customization remains independently retained.
    let user_row: String = conn
        .query_row(
            "SELECT id FROM iptv_channels WHERE id='stream-one'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(user_row, "stream-one");
}

#[cfg(unix)]
#[test]
fn guide_picker_v2_upgrade_retains_alias_and_produces_independent_v2_backup() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Sandbox::new();
    let path = dir.path();
    seed_real_v1(&path);
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch(MIGRATION_002).unwrap();
        db.execute(
            "INSERT INTO epg_sources
            (id,name,origin_kind,origin_locator,created_at,updated_at)
            VALUES('v2-guide','Synthetic','local_file','/fabricated/guide',1,1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO epg_global_id_aliases
            (guide_source_id,iptv_tvg_id,guide_channel_id,updated_at)
            VALUES('v2-guide','guide.fake','exact.station',2)",
            [],
        )
        .unwrap();
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let storage = Storage::at(path.clone());
    storage.initialize().unwrap();
    assert!(!storage.favourites_only().unwrap());
    let conn = storage.connect().unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    let alias: String = conn
        .query_row(
            "SELECT guide_channel_id FROM epg_global_id_aliases WHERE iptv_tvg_id='guide.fake'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(alias, "exact.station");
    drop(conn);
    let backups: Vec<_> = fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("-v2-recovery-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    let db = Connection::open_with_flags(&backups[0], rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='iptv_channel_favourites'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[cfg(unix)]
#[test]
fn guide_v3_to_v4_preserves_user_data_and_recovery_copy() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Sandbox::new();
    let path = dir.path();
    seed_real_v1(&path);
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch(MIGRATION_002).unwrap();
        c.execute_batch(MIGRATION_003).unwrap();
        c.execute(
            "INSERT INTO iptv_channel_favourites(channel_id) VALUES('stream-one')",
            [],
        )
        .unwrap();
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let storage = Storage::at(&path);
    storage.initialize().unwrap();
    let c = storage.connect().unwrap();
    let version: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM iptv_channel_favourites", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM epg_playlist_guide_optouts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let backups: Vec<_> = fs::read_dir(&dir.0)
        .unwrap()
        .map(|f| f.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("-v3-recovery-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    let recover = Connection::open(&backups[0]).unwrap();
    assert_eq!(
        recover
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    storage.initialize().unwrap();
}

#[cfg(unix)]
#[test]
fn guide_invalid_v4_optout_schema_rejected_without_mutation() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Sandbox::new();
    let path = dir.path();
    seed_real_v1(&path);
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch(MIGRATION_002).unwrap();
        c.execute_batch(MIGRATION_003).unwrap();
        let invalid = MIGRATION_004.replace(
            "updated_at INTEGER NOT NULL CHECK (updated_at >= 0)",
            "updated_at INTEGER NOT NULL",
        );
        c.execute_batch(&invalid).unwrap();
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let original = fs::read(&path).unwrap();
    assert!(Storage::at(&path).initialize().is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn guide_reopening_corrupt_dual_selection_is_rejected() {
    let dir = Sandbox::new();
    let path = dir.path();
    seed_real_v1(&path);
    let storage = Storage::at(&path);
    storage.initialize().unwrap();
    {
        let conn = storage.connect().unwrap();
        conn.execute(
            "INSERT INTO iptv_sources(id,name,origin_kind,origin_locator,created_at,updated_at)
             VALUES('source','Synthetic','local_file','/fabricated/source',1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO epg_sources(id,name,origin_kind,origin_locator,created_at,updated_at)
             VALUES('guide','Synthetic','local_file','/fabricated/guide',1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO epg_playlist_links(playlist_source_id,guide_source_id,updated_at)
             VALUES('source','guide',5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO epg_playlist_guide_optouts(playlist_source_id,updated_at)
             VALUES('source',6)",
            [],
        )
        .unwrap();
    }
    assert!(
        storage.connect().is_err(),
        "dual selection corruption must fail validation"
    );
}

#[cfg(unix)]
#[test]
fn playlist_refresh_v4_to_v5_preserves_guide_optout_channel_and_recovery() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Sandbox::new();
    let path = dir.path();
    seed_real_v1(&path);
    {
        let conn = Connection::open(&path).unwrap();
        for part in [MIGRATION_002, MIGRATION_003, MIGRATION_004] {
            conn.execute_batch(part).unwrap();
        }
        conn.execute("INSERT INTO epg_playlist_guide_optouts (playlist_source_id,updated_at) VALUES('playlist-one',200)",[]).unwrap();
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let storage = Storage::at(&path);
    storage.initialize().unwrap();
    let conn = storage.connect().unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
    let source = storage.get_source("playlist-one").unwrap();
    assert!(!source.auto_refresh);
    assert_eq!(source.refresh_interval_seconds, 86400);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM epg_playlist_guide_optouts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM iptv_channels", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM iptv_channel_overrides", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(conn);
    let copies: Vec<_> = fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().contains("-v4-recovery-"))
        .collect();
    assert_eq!(copies.len(), 1);
    assert_eq!(
        Connection::open(&copies[0])
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn playlist_refresh_schedule_opt_in_backoff_attention_and_isolation() {
    use super::storage::ErrorCode;
    let dir = Sandbox::new();
    let storage = Storage::at(dir.path());
    storage.initialize().unwrap();
    let conn = storage.connect().unwrap();
    for id in ["playlist-a", "playlist-b"] {
        conn.execute("INSERT INTO iptv_sources(id,name,origin_kind,origin_locator,created_at,updated_at) VALUES(?1,?1,'local_file','/fabricated/source',1,1)",[id]).unwrap();
    }
    drop(conn);
    let t = 1_800_000_000;
    assert!(storage.due_source_ids(t, 2).unwrap().is_empty());
    assert!(storage
        .set_source_schedule("playlist-a", true, 5 * 3600, t)
        .is_err());
    assert!(storage
        .set_source_schedule("playlist-a", true, 169 * 3600, t)
        .is_err());
    storage
        .set_source_schedule("playlist-a", true, 6 * 3600, t)
        .unwrap();
    assert_eq!(storage.due_source_ids(t, 2).unwrap(), vec!["playlist-a"]);
    storage
        .record_refresh_failure("playlist-a", ErrorCode::Network, t)
        .unwrap();
    assert!(storage.due_source_ids(t + 21599, 2).unwrap().is_empty());
    assert_eq!(
        storage.due_source_ids(t + 21600, 2).unwrap(),
        vec!["playlist-a"]
    );
    storage
        .record_refresh_failure("playlist-a", ErrorCode::ApprovalRequired, t + 21600)
        .unwrap();
    assert!(storage.due_source_ids(t + 172800, 2).unwrap().is_empty());
    storage
        .set_source_schedule("playlist-a", true, 6 * 3600, t + 172800)
        .unwrap();
    assert_eq!(
        storage.due_source_ids(t + 172800, 2).unwrap(),
        vec!["playlist-a"]
    );
    storage
        .set_source_enabled("playlist-a", false, t + 172801)
        .unwrap();
    assert!(storage.due_source_ids(t + 999999, 2).unwrap().is_empty());
    assert!(!storage.get_source("playlist-b").unwrap().auto_refresh);
}

#[cfg(unix)]
#[test]
fn playlist_refresh_invalid_v5_shape_is_rejected_without_mutating_db() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Sandbox::new();
    let path = dir.path();
    seed_real_v1(&path);
    {
        let c = Connection::open(&path).unwrap();
        for sql in [MIGRATION_002, MIGRATION_003, MIGRATION_004] {
            c.execute_batch(sql).unwrap();
        }
        let malicious = MIGRATION_005.replace(
            "CHECK (auto_refresh IN (0,1))",
            "CHECK (auto_refresh IN (0,1,2))",
        );
        c.execute_batch(&malicious).unwrap();
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(Storage::at(&path).initialize().is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

/// Manual deterministic host benchmark of the *real* connection path. The
/// ignored selector prevents large fixture creation in ordinary CI. This is
/// not a physical N100/eMMC or full GTK startup measurement.
#[test]
#[ignore = "manual synthetic SQLite profile; run explicitly with --ignored --nocapture"]
fn profile_storage_initialization_and_connections() {
    use std::time::Instant;
    for programmes in [0_i64, 50_000, 250_000] {
        let dir = Sandbox::new();
        let storage = Storage::at(dir.path());
        let cold = Instant::now();
        storage.initialize().unwrap();
        let first_create_ms = cold.elapsed().as_secs_f64() * 1_000.0;
        if programmes > 0 {
            let mut conn = storage.connect().unwrap();
            let tx = conn.transaction().unwrap();
            tx.execute_batch(
                "INSERT INTO epg_sources
                 (id,name,origin_kind,origin_locator,created_at,updated_at)
                 VALUES ('synthetic','Synthetic','local_file','/synthetic/fixture.xml',1,1);
                 INSERT INTO epg_channels(source_id,channel_id,display_name)
                 VALUES ('synthetic','sample','Synthetic channel');",
            )
            .unwrap();
            tx.execute(
                "WITH RECURSIVE n(x) AS
                 (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < ?1)
                 INSERT INTO epg_programmes(source_id,channel_id,start_utc,stop_utc,title)
                 SELECT 'synthetic','sample',x*1800,x*1800+1700,
                        'Synthetic ' || x FROM n",
                [programmes],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let bytes = fs::metadata(dir.path()).unwrap().len();
        let warm_start = Instant::now();
        storage.initialize().unwrap();
        let warm_start_ms = warm_start.elapsed().as_secs_f64() * 1_000.0;
        let mut elapsed_ms = Vec::with_capacity(30);
        for _ in 0..30 {
            let now = Instant::now();
            drop(storage.connect().unwrap());
            elapsed_ms.push(now.elapsed().as_secs_f64() * 1_000.0);
        }
        elapsed_ms.sort_by(f64::total_cmp);
        let mean = elapsed_ms.iter().sum::<f64>() / elapsed_ms.len() as f64;
        println!(
            "STORAGE_PROFILE programmes={programmes} bytes={bytes} cold_create_ms={first_create_ms:.2} \
             warm_start_ms={warm_start_ms:.2} connect_mean_ms={mean:.2} connect_p50_ms={:.2} connect_p95_ms={:.2}",
            elapsed_ms[15], elapsed_ms[28]
        );
    }
}

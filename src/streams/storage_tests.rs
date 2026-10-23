// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Disposable on-disk migration fixtures using the actual published v1 SQL.
use super::storage::{Storage, APPLICATION_ID, MIGRATION_001, MIGRATION_002, SCHEMA_VERSION};
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

#[test]
fn clean_new_database_is_schema_v2_after_reopening() {
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
    assert_eq!(version, 2);
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
    assert_eq!(sql.matches(expected).count(), 1, "expected one SQL fixture anchor");
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
    let decorated = "CHECK (length(id) /* quoted 'CHECK (wrong)' and -- are comments */ BETWEEN 1 AND 64)";
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

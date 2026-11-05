// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later
// This file is include!d inside backup::tests; private startup helpers remain in scope.

// These two compressed images were generated with the *released* v0.3.1
    // Cargo.lock and bundled SQLite 3.53.2, before any I1 dependency upgrade.
    // See tests/fixtures/sqlite-v031/README.md for exact source SHA and hashes.
    #[cfg(unix)]
    const RELEASED_V031_LIVE: &[u8] =
        include_bytes!("../../tests/fixtures/sqlite-v031/released-v031-live.sqlite3.gz");
    #[cfg(unix)]
    const RELEASED_V031_EXPORT: &[u8] =
        include_bytes!("../../tests/fixtures/sqlite-v031/released-v031-export.sqlite3.gz");

    #[cfg(unix)]
    fn write_released_sqlite_fixture(path: &Path, gzip: &[u8]) {
        use std::os::unix::fs::PermissionsExt;

        let decoder = flate2::read::GzDecoder::new(gzip);
        // The committed fixture must be a small, bounded SQLite image.
        let mut reader = std::io::Read::take(decoder, 2 * 1024 * 1024 + 1);
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        let written = std::io::copy(&mut reader, &mut out).unwrap();
        assert!((4096..=2 * 1024 * 1024).contains(&written));
        out.sync_all().unwrap();
        drop(out);
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(unix)]
    fn assert_released_v031_data(db: &Connection) {
        let app_id: i64 = db
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .unwrap();
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!((app_id, version), (APPLICATION_ID, SCHEMA_VERSION));
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert!(db
            .query_row("PRAGMA foreign_key_check", [], |r| r.get::<_, String>(0))
            .optional()
            .unwrap()
            .is_none());

        let playlist_and_override: (String, String, String, i64, i64, i64) = db
            .query_row(
                "SELECT s.name, c.stream_url, o.display_name_override,
                        s.auto_refresh, s.refresh_interval_seconds,
                        (SELECT favourites_only FROM iptv_view_preferences WHERE id=1)
                 FROM iptv_sources s
                 JOIN iptv_channels c ON c.source_id=s.id
                 JOIN iptv_channel_overrides o ON o.channel_id=c.id
                 WHERE s.id='legacy-playlist' AND c.id='legacy-channel'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            playlist_and_override,
            (
                "Released playlist".to_string(),
                "https://example.invalid/watch.m3u8".to_string(),
                "Retained channel name".to_string(),
                1,
                43200,
                1
            )
        );
        for table in [
            "iptv_sources",
            "iptv_channels",
            "iptv_channel_overrides",
            "iptv_channel_favourites",
            "epg_sources",
            "epg_channels",
            "epg_programmes",
            "epg_playlist_links",
            "epg_channel_overrides",
            "epg_global_id_aliases",
        ] {
            let count: i64 = db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "released fixture count for {table}");
        }
        let programme: (i64, i64, String, String) = db
            .query_row(
                "SELECT start_utc, stop_utc, title, subtitle
                 FROM epg_programmes WHERE source_id='legacy-guide'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            programme,
            (
                120000,
                121800,
                "Released programme".into(),
                "Released subtitle".into()
            )
        );
        let link: (String, String, String) = db
            .query_row(
                "SELECT (SELECT guide_source_id FROM epg_playlist_links
                          WHERE playlist_source_id='legacy-playlist'),
                        (SELECT guide_channel_id FROM epg_channel_overrides
                          WHERE iptv_channel_id='legacy-channel'),
                        (SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            link,
            (
                "legacy-guide".into(),
                "legacy.guide.channel".into(),
                "legacy-guide".into()
            )
        );
        let alias: (String, String) = db
            .query_row(
                "SELECT iptv_tvg_id,guide_channel_id FROM epg_global_id_aliases",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(alias, ("legacy.tvg".into(), "legacy.guide.channel".into()));
    }

    #[cfg(unix)]
    #[test]
    fn released_v031_sqlite_3532_live_and_backup_export_restore_with_3541() {
        use std::os::unix::fs::MetadataExt;

        assert_eq!(rusqlite::version(), "3.54.1");
        let sandbox = Sandbox::new();
        let storage = sandbox.storage();
        write_released_sqlite_fixture(storage.database_path(), RELEASED_V031_LIVE);
        // Actual published v0.3.1 schema-v5 bytes must reopen under the new
        // engine without mutation, migration or loss of user-visible rows.
        storage.initialize().unwrap();
        assert_released_v031_data(&storage.connect().unwrap());

        let old_export = sandbox.0.join("released-v031-export.sqlite3");
        write_released_sqlite_fixture(&old_export, RELEASED_V031_EXPORT);
        assert_released_v031_data(&Storage::at(&old_export).connect().unwrap());

        // Distinguish the active DB from the to-be-restored older snapshot.
        storage
            .connect()
            .unwrap()
            .execute(
                "UPDATE iptv_sources SET name='Changed after upgrade'
                 WHERE id='legacy-playlist'",
                [],
            )
            .unwrap();
        let inode = fs::metadata(storage.database_path()).unwrap().ino();
        let malformed = sandbox.0.join("corrupt-v031-export.sqlite3");
        write_released_sqlite_fixture(&malformed, RELEASED_V031_EXPORT);
        let mut corrupt_bytes = fs::read(&malformed).unwrap();
        corrupt_bytes[0] ^= 0xff;
        fs::write(&malformed, corrupt_bytes).unwrap();
        assert!(prepare_import(&storage, &malformed).is_err());
        assert_eq!(fs::metadata(storage.database_path()).unwrap().ino(), inode);
        assert!(
            !sandbox.0.join(REQUEST).exists(),
            "rejected import was scheduled"
        );
        let name: String = storage
            .connect()
            .unwrap()
            .query_row(
                "SELECT name FROM iptv_sources WHERE id='legacy-playlist'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "Changed after upgrade");

        // Use production backup staging, confirmation, and the exact serialized
        // primary-startup pre-open gate with a fresh isolated startup flag.
        let prepared = prepare_import(&storage, &old_export).unwrap();
        assert_eq!(
            (
                prepared.counts.playlists,
                prepared.counts.channels,
                prepared.counts.guides,
                prepared.counts.programmes
            ),
            (1, 1, 1, 1)
        );
        confirm_import(&storage, prepared).unwrap();
        assert!(sandbox.0.join(REQUEST).exists());
        let before_restart: String = storage
            .connect()
            .unwrap()
            .query_row(
                "SELECT name FROM iptv_sources WHERE id='legacy-playlist'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(before_restart, "Changed after upgrade");

        let lock = Mutex::new(());
        let armed = AtomicBool::new(true);
        let failed = AtomicBool::new(false);
        let processed = AtomicBool::new(false);
        check_startup_restore_with(&storage, &lock, &armed, &failed, &processed, apply_pending)
            .unwrap();
        assert!(!failed.load(Ordering::SeqCst));
        assert!(!armed.load(Ordering::SeqCst));
        assert!(!sandbox.0.join(REQUEST).exists());
        storage.initialize().unwrap();
        assert_released_v031_data(&storage.connect().unwrap());

        // The pre-import active database remains recoverable indefinitely.
        let recovery = fs::read_dir(&sandbox.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(RECOVERY_PREFIX)
            })
            .expect("startup restore must retain the previous active database");
        let preserved: String = Storage::at(recovery)
            .connect()
            .unwrap()
            .query_row(
                "SELECT name FROM iptv_sources WHERE id='legacy-playlist'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(preserved, "Changed after upgrade");

        let new_export = sandbox.0.join("new-engine-export.sqlite3");
        assert_eq!(export(&storage, &new_export).unwrap().programmes, 1);
        assert_released_v031_data(&Storage::at(new_export).connect().unwrap());
    }

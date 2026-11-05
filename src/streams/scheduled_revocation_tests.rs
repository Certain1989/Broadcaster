// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Isolated real-keyfile, real-process and service-scheduler LAN revocation test.
//! Neither the DNS answer nor the fixture transport sends to an actual LAN.
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    consent_ledger,
    fetch::FetchError,
    service::{ServiceError, StreamsService},
    storage::Storage,
};
use crate::{config::BASE_APP_ID, settings::TvSettings};
use adw::{gio, glib, prelude::*};
use rusqlite::params;

const SOURCE: &str = "70d17873-5f61-46b5-a5a4-c4f8f18ab099";
const LOCATOR: &str = "http://lan-fixture.invalid/playlist";
const CHILD: &str = "streams::scheduled_revocation_tests::cold_process_stage";

fn marker_path(root: &Path) -> PathBuf {
    root.join("markers")
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
fn stored_consents() -> Vec<String> {
    gio::Settings::new(BASE_APP_ID)
        .strv("playlist-lan-consents")
        .iter()
        .map(ToString::to_string)
        .collect()
}
fn reset_due() {
    Storage::new().connect().unwrap().execute(
        "UPDATE iptv_sources SET last_attempt_at=NULL,last_success_at=NULL,last_error_code=NULL WHERE id=?1",
        [SOURCE],
    ).unwrap();
}
fn data_paths(root: &Path) -> (PathBuf, PathBuf) {
    (root.join("data"), root.join("config"))
}

#[test]
#[ignore = "requires dedicated isolated keyfile GSettings and fresh child processes"]
fn real_keyfile_revocation_survives_process_restart_and_due_refresh() {
    let root = std::env::temp_dir().join(format!("broadcaster-lan-t1-{}", uuid::Uuid::new_v4()));
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let _cleanup = Cleanup(root.clone());
    let (data, config) = data_paths(&root);
    let cache = root.join("cache");
    for path in [&data, &config, &cache] {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(std::env::var("GSETTINGS_BACKEND").unwrap(), "keyfile");
    assert!(std::env::var_os("GSETTINGS_SCHEMA_DIR").is_some());
    let child = std::env::current_exe().unwrap();
    let run = |stage: &str| {
        Command::new(&child)
            .arg(CHILD)
            .args(["--exact", "--ignored", "--nocapture", "--test-threads=1"])
            .env("BROADCASTER_T1_STAGE", stage)
            .env("BROADCASTER_T1_ROOT", &root)
            .env("BROADCASTER_T1_ISOLATED_NETWORK", "1")
            .env("BROADCASTER_T1_MARKERS", marker_path(&root))
            .env("XDG_DATA_HOME", &data)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_CACHE_HOME", &cache)
            .env("GSETTINGS_BACKEND", "keyfile")
            .output()
            .expect("child process must launch")
    };
    let must_pass = |stage: &str| {
        let result = run(stage);
        assert!(
            result.status.success(),
            "{stage} child failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    for stage in ["seed", "revoke"] {
        must_pass(stage);
    }
    // The oracle must fail for the specifically expected missing-tombstone
    // assertion, not an unrelated subprocess/setup error.
    let oracle = run("oracle");
    assert!(
        !oracle.status.success(),
        "ledger-bypass oracle falsely passed"
    );
    assert!(
        String::from_utf8_lossy(&oracle.stderr).contains("LAN_TOMBSTONE_EXPECTED_NEGATIVE_ORACLE"),
        "oracle did not reach the expected ledger-bypass counterexample"
    );
    let hidden = root.join("oracle-ledger");
    assert!(
        hidden.exists(),
        "oracle did not actually hide the tombstone"
    );
    fs::rename(hidden, consent_ledger_path(&data)).unwrap();
    for stage in [
        "negative",
        "grant",
        "positive",
        "both_fail",
        "both_fail_restart",
    ] {
        must_pass(stage);
    }
}

fn consent_ledger_path(data: &Path) -> PathBuf {
    data.join("broadcaster")
        .join(".playlist-lan-revocations-v1")
}

#[tokio::test]
#[ignore = "helper invoked exactly once per stage by the parent native test"]
async fn cold_process_stage() {
    let stage = std::env::var("BROADCASTER_T1_STAGE")
        .expect("helper must only be invoked by orchestrated cross-process test");
    let root = PathBuf::from(std::env::var("BROADCASTER_T1_ROOT").unwrap());
    let isolated_cache = root.join("cache");
    assert_eq!(
        std::env::var_os("XDG_CACHE_HOME"),
        Some(isolated_cache.clone().into_os_string()),
        "the native child must not inherit the user's cache"
    );
    assert_eq!(
        glib::user_cache_dir(),
        isolated_cache,
        "GLib logo-cache access must resolve inside the disposable fixture"
    );
    let storage = Storage::new();
    let settings = TvSettings::get();
    match stage.as_str() {
        "seed" => {
            storage.initialize().unwrap();
            storage.connect().unwrap().execute(
                "INSERT INTO iptv_sources (id,name,origin_kind,origin_locator,enabled,auto_refresh,refresh_interval_seconds,created_at,updated_at)
                 VALUES (?1,'Synthetic T1','remote_url',?2,1,1,21600,0,0)",
                params![SOURCE, LOCATOR],
            ).unwrap();
            assert!(settings.set_playlist_lan_consent(SOURCE, LOCATOR, true));
            gio::Settings::sync();
            assert!(settings.playlist_lan_allowed(SOURCE, LOCATOR));
            assert!(!settings.playlist_lan_allowed(SOURCE, "http://other.invalid/playlist"));
            assert!(!settings.playlist_lan_allowed("different-source", LOCATOR));
            assert_eq!(stored_consents().len(), 1);
            // A different SQLite inode cannot inherit the previous database's
            // grant even with the same source ID and exact URL.
            let db = storage.database_path();
            let original = root.join("original-db-inode.sqlite3");
            let alternate = root.join("alternate-db-inode.sqlite3");
            fs::copy(db, &alternate).unwrap();
            fs::rename(db, &original).unwrap();
            fs::rename(&alternate, db).unwrap();
            assert!(!settings.playlist_lan_allowed(SOURCE, LOCATOR));
            fs::remove_file(db).unwrap();
            fs::rename(original, db).unwrap();
            assert!(settings.playlist_lan_allowed(SOURCE, LOCATOR));
        }
        "revoke" => {
            assert_eq!(
                stored_consents().len(),
                1,
                "real saved grant not preserved from prior process"
            );
            assert!(settings.playlist_lan_allowed(SOURCE, LOCATOR));
            std::env::set_var("BROADCASTER_T1_REFUSE_KEYFILE_WRITE", "1");
            std::env::set_var("BROADCASTER_T1_REFUSE_KEYFILE_UNTIL_EXIT", "1");
            let changed = settings.set_playlist_lan_consent(SOURCE, LOCATOR, false);
            gio::Settings::sync();
            assert!(!changed, "the real backend must refuse the removal");
            assert!(!settings.playlist_lan_allowed(SOURCE, LOCATOR));
            // RLIMIT_NOFILE=0 stays in force until child exit. The separate
            // oracle process reopens the already-fsynced production ledger.
        }
        "oracle" => {
            assert_eq!(
                stored_consents().len(),
                1,
                "stale persisted GSettings disappeared"
            );
            let path = consent_ledger::path();
            assert_eq!(path, consent_ledger_path(&root.join("data")));
            assert!(consent_ledger::is_revoked(&path, &database_token(&storage)).unwrap());
            fs::rename(path, root.join("oracle-ledger")).unwrap();
            // This is the *same* production read/assertion as the negative
            // stage. It MUST fail when the durable ledger is absent.
            let bypassed = settings.playlist_lan_allowed(SOURCE, LOCATOR);
            if bypassed {
                eprintln!("LAN_TOMBSTONE_EXPECTED_NEGATIVE_ORACLE");
            }
            assert!(
                !bypassed,
                "ORACLE: saved stale grant became effective without tombstone"
            );
        }
        "negative" => {
            assert_eq!(
                stored_consents().len(),
                1,
                "real stale GSettings grant missing"
            );
            assert!(
                consent_ledger::is_revoked(&consent_ledger::path(), &database_token(&storage))
                    .unwrap()
            );
            assert!(!settings.playlist_lan_allowed(SOURCE, LOCATOR));
            scheduled_scan(false, &root).await;
        }
        "grant" => {
            assert_eq!(stored_consents().len(), 1);
            assert!(settings.set_playlist_lan_consent(SOURCE, LOCATOR, true));
            gio::Settings::sync();
            assert!(settings.playlist_lan_allowed(SOURCE, LOCATOR));
            assert!(!consent_ledger::is_revoked(
                &consent_ledger::path(),
                &database_token(&storage)
            )
            .unwrap());
            reset_due();
        }
        "positive" => {
            assert_eq!(stored_consents().len(), 1);
            assert!(settings.playlist_lan_allowed(SOURCE, LOCATOR));
            // The real successful-refresh path prunes logo files. Prove it
            // targets only this child's private cache rather than the user's.
            let logos = root.join("cache").join("broadcaster").join("logos");
            fs::create_dir_all(&logos).unwrap();
            fs::set_permissions(&logos, fs::Permissions::from_mode(0o700)).unwrap();
            let stale = logos.join(format!("{}.img", "f".repeat(64)));
            fs::write(&stale, b"unreferenced synthetic logo").unwrap();
            assert!(stale.is_file());
            scheduled_scan(true, &root).await;
            assert!(
                !stale.exists(),
                "successful refresh must prune only the disposable cache"
            );
        }
        "both_fail" => {
            // Exercise the *other* policy boundary separately from the
            // successfully persisted tombstone. No durable denial can be
            // promised if the backend and ledger both reject publication.
            assert_eq!(stored_consents().len(), 1);
            assert!(settings.playlist_lan_allowed(SOURCE, LOCATOR));
            let path = consent_ledger::path();
            assert!(path.is_file(), "fresh grant must leave a valid ledger");
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
            std::env::set_var("BROADCASTER_T1_REFUSE_KEYFILE_WRITE", "1");
            std::env::set_var("BROADCASTER_T1_REFUSE_KEYFILE_UNTIL_EXIT", "1");
            assert!(!settings.set_playlist_lan_consent(SOURCE, LOCATOR, false));
            gio::Settings::sync();
            assert!(
                !settings.playlist_lan_allowed(SOURCE, LOCATOR),
                "failed revoke must fence the current process"
            );
        }
        "both_fail_restart" => {
            assert_eq!(stored_consents().len(), 1, "saved grant must survive");
            let path = consent_ledger::path();
            assert!(path.is_dir(), "ledger refusal was not exercised");
            assert!(
                !settings.playlist_lan_allowed(SOURCE, LOCATOR),
                "invalid ledger must deny while present"
            );
            fs::remove_dir(&path).unwrap();
            assert!(
                settings.playlist_lan_allowed(SOURCE, LOCATOR),
                "accepted limitation: without any durable denial, the old grant may recur"
            );
        }
        _ => panic!("unknown subprocess stage"),
    }
}

fn database_token(storage: &Storage) -> String {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(storage.database_path()).unwrap();
    crate::settings::playlist_lan_revocation_token(
        SOURCE,
        &format!("{}:{}", meta.dev(), meta.ino()),
    )
}

async fn scheduled_scan(permitted: bool, root: &Path) {
    let server = if permitted {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::env::set_var("BROADCASTER_T1_LOCAL_PORT", port.to_string());
        Some(std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let mut request = [0u8; 256];
                        let n = sock.read(&mut request).unwrap();
                        assert!(
                            String::from_utf8_lossy(&request[..n]).starts_with("GET /playlist ")
                        );
                        let body =
                            b"#EXTM3U\n#EXTINF:-1,Synthetic\nhttps://example.invalid/stream\n";
                        write!(
                            sock,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .unwrap();
                        sock.write_all(body).unwrap();
                        return;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "mock LAN GET never arrived"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(e) => panic!("loopback fixture failed: {e}"),
                }
            }
        }))
    } else {
        None
    };
    let service = StreamsService::with_storage(Storage::new()).await.unwrap();
    let source = service.source(SOURCE).await.unwrap();
    assert!(source.enabled && source.auto_refresh);
    assert!(source.last_success_at.is_none());
    let selected = service.due_ids(now()).await.unwrap();
    assert_eq!(
        selected,
        vec![SOURCE.to_owned()],
        "real scheduler did not select source"
    );
    let outcome = service.refresh_scheduled(SOURCE.to_owned()).await;
    if permitted {
        assert!(
            outcome.is_ok(),
            "fresh grant scheduled refresh failed: {outcome:?}"
        );
        server.unwrap().join().unwrap();
    } else {
        assert!(
            matches!(outcome, Err(ServiceError::Fetch(FetchError::InvalidOrigin))),
            "must reject at the LAN policy gate, got {outcome:?}"
        );
    }
    let markers = fs::read_to_string(marker_path(root)).unwrap();
    assert_eq!(
        markers.matches("resolver\n").count(),
        if permitted { 2 } else { 1 },
        "scheduler must reach the policy resolver once per selected stage"
    );
    assert_eq!(
        markers.matches("transport\n").count(),
        if permitted { 1 } else { 0 },
        "revoked scheduler must not invoke any LAN GET transport"
    );
}

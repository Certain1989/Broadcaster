// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Real persistent GSettings and scheduled-refresh restart regression.
//! Every stage is an entirely new test process, with isolated data and a
//! loopback HTTP response reachable only after the real private-LAN gate.
use super::{
    consent_ledger,
    fetch::{self, FetchError},
    model::{
        IncomingChannel, OriginKind, SourceDefaults, SourceDraft, SourceOrigin, ValidatedSnapshot,
    },
    service::{ServiceError, StreamsService},
    storage::Storage,
};
use crate::{config::BASE_APP_ID, settings::TvSettings};
use adw::{gio, prelude::*};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};


const ROOT_ENV: &str = "BROADCASTER_LAN_RESTART_ROOT";
const PORT_ENV: &str = "BROADCASTER_LAN_RESTART_PORT";
const URL_HOST: &str = "lan-fixture.invalid";
const PLAYLIST: &str =
    "#EXTM3U\n#EXTINF:-1 tvg-id=\"channel-0\",Channel 0\nhttps://example.invalid/live/0\n";

fn root() -> PathBuf {
    let path = PathBuf::from(std::env::var_os(ROOT_ENV).expect("isolated test root required"));
    assert!(path.starts_with(std::env::temp_dir()));
    assert_eq!(
        PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap()),
        path.join("data")
    );
    assert_eq!(
        PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap()),
        path.join("config")
    );
    assert_eq!(std::env::var("GSETTINGS_BACKEND").unwrap(), "keyfile");
    assert!(std::env::var_os("GSETTINGS_SCHEMA_DIR").is_some());
    path
}

fn source_id() -> String {
    fs::read_to_string(root().join("source-id"))
        .expect("seed must publish ID")
        .trim()
        .to_owned()
}

fn locator() -> String {
    let port: u16 = std::env::var(PORT_ENV).unwrap().parse().unwrap();
    format!("http://{URL_HOST}:{port}/playlist")
}

fn has_saved_grant() -> bool {
    gio::Settings::new(BASE_APP_ID)
        .strv("playlist-lan-consents")
        .iter()
        .any(|entry| entry.starts_with(&format!("{}:", source_id())))
}

fn single_channel() -> IncomingChannel {
    IncomingChannel {
        order: 0,
        imported_name: "Channel 0".into(),
        stream_url: "https://example.invalid/live/0".into(),
        tvg_id: Some("channel-0".into()),
        tvg_name: None,
        group_title: None,
        logo_url: None,
        channel_number: None,
        region: None,
        languages: BTreeSet::new(),
        countries: BTreeSet::new(),
        user_agent: None,
        referer: None,
    }
}

#[test]
#[ignore = "separate process invoked only by the supervised native parent"]
fn lan_restart_seed_grant() {
    let root = root();
    let store = Storage::new();
    store.initialize().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let source = SourceDraft {
        name: "Synthetic scheduled private playlist".into(),
        origin: SourceOrigin {
            kind: OriginKind::RemoteUrl,
            locator: locator(),
        },
        enabled: true,
        order: 0,
        defaults: SourceDefaults::default(),
    };
    let snap = ValidatedSnapshot::new(id.clone(), vec![single_channel()]).unwrap();
    store
        .create_source_with_initial_snapshot(&id, &source, &snap, 1)
        .unwrap();
    store.set_source_schedule(&id, true, 21_600, 1).unwrap();
    fs::write(root.join("source-id"), &id).unwrap();
    let settings = TvSettings::get();
    assert!(settings.set_playlist_lan_consent(&id, &locator(), true));
    gio::Settings::sync();
    assert!(
        has_saved_grant(),
        "real persistent GSettings grant required"
    );
    assert!(settings.playlist_lan_allowed(&id, &locator()));
    assert!(!settings.playlist_lan_allowed("other-source", &locator()));
    assert!(!settings.playlist_lan_allowed(&id, "http://lan-fixture.invalid/other"));
}

#[test]
#[ignore = "separate process invoked only by the supervised native parent"]
fn lan_restart_refuse_backend_write() {
    let _ = root();
    let id = source_id();
    assert!(
        has_saved_grant(),
        "grant must survive a genuine process exit"
    );
    // The GIO-supplied null backend refuses the actual production setter.
    // The separate keyfile backend still contains the old disk-backed grant.
    let settings = TvSettings::get();
    assert!(!settings.is_writable("playlist-lan-consents"));
    assert!(!settings.set_playlist_lan_consent(&id, &locator(), false));
    assert!(has_saved_grant(), "old keyfile grant must remain persisted");
    let db = Storage::new();
    let metadata = fs::metadata(db.database_path()).unwrap();
    use std::os::unix::fs::MetadataExt;
    let token = crate::settings::playlist_lan_revocation_token(
        &id,
        &format!("{}:{}", metadata.dev(), metadata.ino()),
    );
    let ledger = consent_ledger::path();
    assert!(consent_ledger::is_revoked(&ledger, &token).unwrap());
    assert_eq!(
        fs::metadata(&ledger).unwrap().permissions().mode() & 0o077,
        0
    );
    // A separate all-writes-fail control documents the accepted limitation.
    let unavailable = Path::new("/proc/self/broadcaster-impossible-tombstone");
    assert!(!crate::settings::persist_lan_consent_with_ledger(
        unavailable,
        &token,
        false,
        || settings.set_strv("playlist-lan-consents", &[]).is_ok()
    ));
    assert!(consent_ledger::is_revoked(&ledger, &token).unwrap());
}

#[tokio::test]
#[ignore = "separate process invoked only by the supervised native parent"]
async fn lan_restart_denied_scheduled_due() {
    let _ = root();
    let id = source_id();
    assert!(
        has_saved_grant(),
        "stale grant must survive into fresh process"
    );
    let settings = TvSettings::get();
    assert!(!settings.playlist_lan_allowed(&id, &locator()));
    let service = StreamsService::with_storage(Storage::new()).await.unwrap();
    let state = service.source(&id).await.unwrap();
    assert!(state.enabled && state.auto_refresh);
    assert_eq!(state.origin.locator, locator());
    let due = service
        .due_ids(super::service::now_for_test())
        .await
        .unwrap();
    assert_eq!(
        due,
        vec![id.clone()],
        "actual scheduler scan must select source"
    );
    let result = service.refresh_scheduled(due[0].clone()).await;
    assert!(
        matches!(result, Err(ServiceError::Fetch(FetchError::InvalidOrigin))),
        "{result:?}"
    );
    let (resolved, sent) = fetch::lan_restart_probe_counts();
    assert_eq!(resolved, 1, "must reach private DNS production guard");
    assert_eq!(sent, 0, "zero private GETs after revocation");
    assert_eq!(
        service
            .source(&id)
            .await
            .unwrap()
            .last_error_code
            .as_deref(),
        Some("network")
    );
}

#[test]
#[ignore = "separate process invoked only by the supervised native parent"]
fn lan_restart_tombstone_sensitivity_oracle() {
    let _ = root();
    let id = source_id();
    let ledger = consent_ledger::path();
    let shadow = ledger.with_extension("sensitivity-oracle");
    assert!(has_saved_grant());
    assert!(!TvSettings::get().playlist_lan_allowed(&id, &locator()));
    fs::rename(&ledger, &shadow).unwrap();
    // Removing only the tombstone must turn the unmodified stale keyfile
    // consent back on: a broken tombstone check makes the negative false-green.
    let accepted_without_tombstone = TvSettings::get().playlist_lan_allowed(&id, &locator());
    fs::rename(&shadow, &ledger).unwrap();
    assert!(
        accepted_without_tombstone,
        "negative must be sensitive to the ledger override"
    );
    assert!(!TvSettings::get().playlist_lan_allowed(&id, &locator()));
}

#[test]
#[ignore = "separate process invoked only by the supervised native parent"]
fn lan_restart_fresh_persistent_grant() {
    let _ = root();
    let id = source_id();
    assert!(has_saved_grant());
    assert!(!TvSettings::get().playlist_lan_allowed(&id, &locator()));
    assert!(TvSettings::get().set_playlist_lan_consent(&id, &locator(), true));
    gio::Settings::sync();
    assert!(TvSettings::get().playlist_lan_allowed(&id, &locator()));
    assert!(has_saved_grant());
    // The failed scheduled request set backoff; make this fixture due again.
    Storage::new()
        .connect()
        .unwrap()
        .execute(
            "UPDATE iptv_sources SET last_attempt_at=1,last_success_at=1 WHERE id=?1",
            [&id],
        )
        .unwrap();
}

#[tokio::test]
#[ignore = "separate process invoked only by the supervised native parent"]
async fn lan_restart_authorized_scheduled_get() {
    let _ = root();
    let id = source_id();
    assert!(has_saved_grant());
    assert!(TvSettings::get().playlist_lan_allowed(&id, &locator()));
    let service = StreamsService::with_storage(Storage::new()).await.unwrap();
    assert_eq!(
        service
            .due_ids(super::service::now_for_test())
            .await
            .unwrap(),
        vec![id.clone()]
    );
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        service.refresh_scheduled(id.clone()),
    )
    .await
    .expect("bounded due refresh");
    assert!(
        result.is_ok(),
        "newly permitted due refresh must succeed: {result:?}"
    );
    let (resolved, sent) = fetch::lan_restart_probe_counts();
    assert_eq!(resolved, 1);
    assert_eq!(sent, 1, "exactly one guarded synthetic GET");
    assert!(service.source(&id).await.unwrap().last_success_at.unwrap() > 1);
}

#[test]
#[ignore = "separate process invoked only by the supervised native parent"]
fn lan_restart_db_identity_mismatch() {
    let _ = root();
    let id = source_id();
    let original = Storage::new();
    let previous = original.database_path().with_extension("prior-inode");
    fs::rename(original.database_path(), &previous).unwrap();
    Storage::new().initialize().unwrap();
    assert!(has_saved_grant());
    assert!(!TvSettings::get().playlist_lan_allowed(&id, &locator()));
}

struct HttpOnlyLoopback {
    root: PathBuf,
    port: u16,
    gets: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl HttpOnlyLoopback {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("broadcaster-lan-restart-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        for child in ["data", "config"] {
            let dir = root.join(child);
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let gets = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (counter, stopped) = (gets.clone(), stop.clone());
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut connection, _)) => {
                        connection
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut request = [0u8; 2048];
                        let count = connection.read(&mut request).unwrap();
                        assert!(std::str::from_utf8(&request[..count])
                            .unwrap()
                            .starts_with("GET /playlist "));
                        counter.fetch_add(1, Ordering::SeqCst);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            PLAYLIST.len(),
                            PLAYLIST
                        );
                        connection.write_all(response.as_bytes()).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("isolated fixture accept failed: {error}"),
                }
            }
        });
        Self {
            root,
            port,
            gets,
            stop,
            worker: Some(worker),
        }
    }
    fn child(&self, case: &str, readonly: bool) {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg(format!("streams::lan_restart_tests::{case}"))
            .args(["--exact", "--ignored", "--nocapture", "--test-threads=1"])
            .env(ROOT_ENV, &self.root)
            .env(PORT_ENV, self.port.to_string())
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("GSETTINGS_BACKEND", "keyfile")
            .env("BROADCASTER_TEST_LAN_RESTART_FIXTURE", "isolated");
        if readonly {
            command.env("BROADCASTER_TEST_READONLY_GSETTINGS", "lan-restart");
        } else {
            command.env_remove("BROADCASTER_TEST_READONLY_GSETTINGS");
        }
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > Duration::from_secs(20) {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("isolated child {case} exceeded 20s");
            }
            thread::sleep(Duration::from_millis(20));
        };
        let output = child.wait_with_output().unwrap();
        assert!(
            status.success(),
            "child {case} exited {status}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        println!("isolated child {case}: passed");
    }
}
impl Drop for HttpOnlyLoopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
#[ignore = "real keyfile-backed GSettings and cold process restart, native CI only"]
fn native_real_gsettings_restart_scheduled_lan_revoke() {
    assert!(
        std::env::var_os("GSETTINGS_SCHEMA_DIR").is_some(),
        "compiled schemas required"
    );
    let fixture = HttpOnlyLoopback::new();
    fixture.child("lan_restart_seed_grant", false);
    fixture.child("lan_restart_refuse_backend_write", true);
    fixture.child("lan_restart_denied_scheduled_due", false);
    assert_eq!(
        fixture.gets.load(Ordering::SeqCst),
        0,
        "revoked refresh may not reach loopback transport"
    );
    fixture.child("lan_restart_tombstone_sensitivity_oracle", false);
    fixture.child("lan_restart_fresh_persistent_grant", false);
    fixture.child("lan_restart_authorized_scheduled_get", false);
    assert_eq!(
        fixture.gets.load(Ordering::SeqCst),
        1,
        "fresh grant must emit one guarded synthetic GET"
    );
    fixture.child("lan_restart_db_identity_mismatch", false);
}

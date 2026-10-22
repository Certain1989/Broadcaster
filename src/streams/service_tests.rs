// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reproducible service-level fixtures; no public Internet or real IPTV URLs.
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use adw::{gio, glib};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::{
    model::{ChannelQuery, SourceDefaults},
    service::{RefreshOutcome, ServiceError, SourceSelection, StreamsService},
    storage::{Storage, StorageError},
};

struct Fixture {
    path: PathBuf,
    service: StreamsService,
}
impl Fixture {
    async fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("broadcaster-service-{}", uuid::Uuid::new_v4()));
        let path = directory.join("broadcaster.sqlite3");
        let service = StreamsService::with_storage(Storage::at(&path))
            .await
            .unwrap();
        Self { path, service }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

struct HttpFixture {
    addr: std::net::SocketAddr,
    body: Arc<Mutex<Vec<u8>>>,
    task: tokio::task::JoinHandle<()>,
}
impl HttpFixture {
    async fn new(body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shared = Arc::new(Mutex::new(body));
        let body = shared.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    let count = match stream.read(&mut buffer).await {
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    let request = &buffer[..count];
                    if request.starts_with(b"GET /slow ") {
                        tokio::time::sleep(Duration::from_millis(350)).await;
                    }
                    let current = body.lock().unwrap().clone();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        current.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.write_all(&current).await;
                });
            }
        });
        Self {
            addr,
            body: shared,
            task,
        }
    }
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
    fn replace(&self, data: Vec<u8>) {
        *self.body.lock().unwrap() = data;
    }
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn playlist(channels: usize) -> Vec<u8> {
    let mut result = "#EXTM3U\n".to_owned();
    for i in 0..channels {
        result.push_str(&format!(
            "#EXTINF:-1 tvg-id=\"test-{i}\" group-title=\"News\",Channel {i}\nhttps://example.invalid/live/{i}\n"
        ));
    }
    result.into_bytes()
}

#[tokio::test]
async fn import_refresh_rejects_shrink_without_approval_and_keeps_offline_cache() {
    let test = Fixture::new().await;
    let server = HttpFixture::new(playlist(20)).await;
    let service = &test.service;
    let source = service
        .add_url(
            "Synthetic",
            server.url("/list"),
            0,
            SourceDefaults::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        20
    );

    server.replace(playlist(4));
    let ticket = match service.refresh(&source.id).await.unwrap() {
        RefreshOutcome::ConfirmationRequired(ticket) => ticket,
        RefreshOutcome::Committed(_) => panic!("shrinking 20 to 4 must need confirmation"),
    };
    assert!(ticket.preview.requires_confirmation);
    assert_eq!(ticket.preview.old_present, 20);
    assert_eq!(ticket.preview.new_present, 4);
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        20
    );

    assert!(matches!(
        service.confirm_refresh(ticket, false).await,
        Err(ServiceError::Declined)
    ));
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        20
    );

    let ticket = match service.refresh(&source.id).await.unwrap() {
        RefreshOutcome::ConfirmationRequired(ticket) => ticket,
        RefreshOutcome::Committed(_) => panic!("must confirm"),
    };
    service.confirm_refresh(ticket, true).await.unwrap();
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        4
    );

    service.toggle_source(&source.id, false).await.unwrap();
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        0
    );
    service.toggle_source(&source.id, true).await.unwrap();
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        4
    );
}

#[tokio::test]
async fn relative_streams_resolve_against_final_remote_playlist_address() {
    let test = Fixture::new().await;
    let server = HttpFixture::new(b"#EXTM3U\n#EXTINF:-1,Relative\n../stream.m3u8\n".to_vec()).await;
    let source = test
        .service
        .add_url(
            "Relative fixture",
            server.url("/lists/import.m3u"),
            0,
            SourceDefaults::default(),
        )
        .await
        .unwrap();
    let page = test.service.query(ChannelQuery::default()).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].source_id, source.id);
    assert_eq!(page.items[0].stream_url, server.url("/stream.m3u8"));
}

#[tokio::test]
async fn malformed_reimport_does_not_create_or_replace_catalogue() {
    let test = Fixture::new().await;
    let server = HttpFixture::new(b"#EXT-X-VERSION:3\n".to_vec()).await;
    let service = &test.service;
    let failed = service
        .add_url("Invalid", server.url("/list"), 0, SourceDefaults::default())
        .await;
    assert!(matches!(failed, Err(ServiceError::Parse(_))));
    assert!(service.list_sources().await.unwrap().is_empty());

    server.replace(playlist(2));
    let source = service
        .add_url("Valid", server.url("/list"), 0, SourceDefaults::default())
        .await
        .unwrap();
    server.replace(b"#EXTM3U\n".to_vec());
    assert!(matches!(
        service.refresh(&source.id).await,
        Err(ServiceError::Parse(_))
    ));
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        2
    );

    server.replace(b"#EXT-X-STREAM-INF:BANDWIDTH=1000\n".to_vec());
    assert!(matches!(
        service.refresh(&source.id).await,
        Err(ServiceError::Parse(_))
    ));
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        2
    );
}

#[tokio::test]
async fn cancellation_and_newer_refresh_invalidate_old_confirmations() {
    let test = Fixture::new().await;
    let server = HttpFixture::new(playlist(20)).await;
    let service = &test.service;
    let source = service
        .add_url("Race", server.url("/list"), 0, SourceDefaults::default())
        .await
        .unwrap();
    server.replace(playlist(4));
    let ticket = match service.refresh(&source.id).await.unwrap() {
        RefreshOutcome::ConfirmationRequired(ticket) => ticket,
        _ => panic!("confirmation required"),
    };
    assert!(service.cancel(&source.id).await);
    assert!(matches!(
        service.confirm_refresh(ticket, true).await,
        Err(ServiceError::Cancelled)
    ));
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        20
    );

    let old = match service.refresh(&source.id).await.unwrap() {
        RefreshOutcome::ConfirmationRequired(ticket) => ticket,
        _ => panic!("confirmation required"),
    };
    server.replace(playlist(18));
    let refreshed = service.refresh(&source.id).await.unwrap();
    assert!(matches!(refreshed, RefreshOutcome::Committed(_)));
    assert!(matches!(
        service.confirm_refresh(old, true).await,
        Err(ServiceError::Cancelled)
    ));
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        18
    );
}

#[tokio::test]
async fn superseded_network_operation_cannot_overwrite_successor() {
    let test = Fixture::new().await;
    let server = HttpFixture::new(playlist(20)).await;
    let service = test.service.clone();
    let source = service
        .add_url(
            "Overlapping",
            server.url("/slow"),
            0,
            SourceDefaults::default(),
        )
        .await
        .unwrap();
    server.replace(playlist(7));
    // GIO-backed refresh futures are intentionally !Send: run both on the
    // current executor thread instead of sending one to a Tokio worker.
    let first = service.refresh(&source.id);
    let second = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.replace(playlist(19));
        service.refresh(&source.id).await
    };
    let (first, second) = tokio::join!(first, second);
    assert!(matches!(second.unwrap(), RefreshOutcome::Committed(_)));
    assert!(matches!(first, Err(ServiceError::Cancelled)));
    assert_eq!(
        service.query(ChannelQuery::default()).await.unwrap().total,
        19
    );
}

#[test]
fn local_gio_reselect_preserves_cached_channels_until_atomic_replacement() {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            context.block_on(async {
                let test = Fixture::new().await;
                let first_path = test.path.parent().unwrap().join("original.m3u");
                let second_path = test.path.parent().unwrap().join("reselected.m3u");
                fs::write(&first_path, playlist(2)).unwrap();
                fs::write(&second_path, playlist(3)).unwrap();
                let original = gio::File::for_path(&first_path);
                let source = test
                    .service
                    .add_local("Selected", original, 0, SourceDefaults::default())
                    .await
                    .unwrap();
                fs::remove_file(&first_path).unwrap();

                assert!(matches!(
                    test.service.refresh(&source.id).await,
                    Err(ServiceError::NeedsReauthorization)
                ));
                assert_eq!(
                    test.service
                        .query(ChannelQuery::default())
                        .await
                        .unwrap()
                        .total,
                    2
                );
                let chosen = gio::File::for_path(&second_path);
                let replaced = test
                    .service
                    .replace_origin(&source.id, SourceSelection::Local(chosen))
                    .await
                    .unwrap();
                assert!(matches!(replaced, RefreshOutcome::Committed(_)));
                let updated = test.service.source(&source.id).await.unwrap();
                assert_ne!(updated.origin.locator, source.origin.locator);
                assert_eq!(
                    test.service
                        .query(ChannelQuery::default())
                        .await
                        .unwrap()
                        .total,
                    3
                );
            })
        })
        .unwrap();
}

#[tokio::test]
async fn production_startup_purge_keeps_overridden_and_young_missing_channels() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let test = Fixture::new().await;
    let server = HttpFixture::new(playlist(4)).await;
    let _source = test
        .service
        .add_url(
            "Retention fixture",
            server.url("/list"),
            0,
            SourceDefaults::default(),
        )
        .await
        .unwrap();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let conn = rusqlite::Connection::open(&test.path).unwrap();
    let mut stmt = conn
        .prepare("SELECT id FROM iptv_channels ORDER BY playlist_order")
        .unwrap();
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    drop(stmt);
    assert_eq!(ids.len(), 4);

    for (index, since) in [
        (0, now - 91 * 86400),
        (1, now - 91 * 86400),
        (2, now - 89 * 86400),
    ] {
        conn.execute(
            "UPDATE iptv_channels SET is_present=0, missing_since=?1 WHERE id=?2",
            rusqlite::params![since, ids[index]],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO iptv_channel_overrides (channel_id,display_name_override,updated_at)
         VALUES (?1,'Kept user edit',?2)",
        rusqlite::params![ids[1], now],
    )
    .unwrap();
    drop(conn);

    // This runs the real service initialization path, not a detached SQL call.
    // Its cleanup must occur on the SQLite blocking worker and never touch
    // overridden or recent missing entries.
    let reopened = StreamsService::with_storage(Storage::at(&test.path))
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(&test.path).unwrap();
    let mut stmt = conn
        .prepare("SELECT id FROM iptv_channels ORDER BY playlist_order")
        .unwrap();
    let remaining: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    drop(stmt);
    assert_eq!(remaining.len(), 3);
    assert!(
        !remaining.contains(&ids[0]),
        "expired unedited tombstone survived"
    );
    assert!(remaining.contains(&ids[1]), "user override was deleted");
    assert!(remaining.contains(&ids[2]), "recent tombstone was deleted");
    assert!(remaining.contains(&ids[3]), "present channel was deleted");
    drop(conn);

    assert_eq!(reopened.list_sources().await.unwrap().len(), 1);
    let _again = StreamsService::with_storage(Storage::at(&test.path))
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(&test.path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM iptv_channels", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 3, "startup cleanup is not idempotent");
}

#[tokio::test]
async fn retention_daily_gate_failure_retry_clock_shift_and_concurrent_calls() {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    const DAY: i64 = 86400;
    let test = Fixture::new().await;
    let service = &test.service;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // Guaranteed beyond the initial startup run's successful timestamp.
    let first_due = now + DAY * 2;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = |counter: Arc<AtomicUsize>| {
        move |_store: Storage, _at: i64| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        }
    };

    service
        .purge_expired_missing_if_due_at(first_due, count(calls.clone()))
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    service
        .purge_expired_missing_if_due_at(first_due + DAY - 1, count(calls.clone()))
        .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "purge ran twice within a day"
    );
    service
        .purge_expired_missing_if_due_at(first_due + DAY, count(calls.clone()))
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // If SQLite returns an error, keep the catalogue operational and allow
    // retry on a subsequent successful refresh instead of suppressing it.
    let fail_at = first_due + DAY * 2;
    let failing_calls = calls.clone();
    service
        .purge_expired_missing_if_due_at(fail_at, move |_store, _at| {
            failing_calls.fetch_add(1, Ordering::SeqCst);
            Err(StorageError::StorageFailure)
        })
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    service
        .purge_expired_missing_if_due_at(fail_at, count(calls.clone()))
        .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "purge failure was not retryable"
    );
    assert!(service.list_sources().await.is_ok());

    // Backwards system-clock steps must not cause duplicate retention work.
    service
        .purge_expired_missing_if_due_at(fail_at - DAY, count(calls.clone()))
        .await;
    assert_eq!(calls.load(Ordering::SeqCst), 4);

    // Two simultaneous successful refreshes must not run duplicate purges.
    let concurrent_at = fail_at + DAY;
    let ((), ()) = tokio::join!(
        service.purge_expired_missing_if_due_at(concurrent_at, count(calls.clone())),
        service.purge_expired_missing_if_due_at(concurrent_at, count(calls.clone())),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn purge_and_refresh_serialize_without_catalogue_loss() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let test = Fixture::new().await;
    let server = HttpFixture::new(playlist(3)).await;
    let source = test
        .service
        .add_url(
            "Concurrent retention",
            server.url("/list"),
            0,
            SourceDefaults::default(),
        )
        .await
        .unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let conn = rusqlite::Connection::open(&test.path).unwrap();
    conn.execute(
        "UPDATE iptv_channels SET is_present=0, missing_since=?1
         WHERE source_id=?2 AND playlist_order=0",
        rusqlite::params![now - 91 * 86400, source.id],
    )
    .unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();

    // Hold an actual SQLite writer lock to force both the purge worker and
    // refresh transaction to contend, then release the lock together.
    // A refresh future can hold non-Send GIO types even for URL-backed
    // sources. Join these local futures rather than requiring tokio::spawn's
    // Send bound; the SQLite operations still execute on blocking workers.
    let cleanup = test
        .service
        .purge_expired_missing_if_due_at(now + 2 * 86400, |store, at| {
            store.purge_expired_missing(at)
        });
    let refresh = test.service.refresh(&source.id);
    let release_writer = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        conn.execute_batch("COMMIT").unwrap();
    };
    let ((), refresh_result, ()) = tokio::join!(cleanup, refresh, release_writer);
    assert!(matches!(
        refresh_result.unwrap(),
        RefreshOutcome::Committed(_)
    ));

    // The refresh is still authoritative irrespective of which SQLite writer
    // reached the lock first. A stale purge cannot wipe refreshed rows.
    let conn = rusqlite::Connection::open(&test.path).unwrap();
    let present: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM iptv_channels WHERE source_id=?1 AND is_present=1",
            [&source.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(present, 3);
    assert_eq!(
        test.service
            .query(ChannelQuery::default())
            .await
            .unwrap()
            .total,
        3
    );
}

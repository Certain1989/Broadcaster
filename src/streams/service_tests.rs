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
    storage::Storage,
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
    let first = {
        let service = service.clone();
        let id = source.id.clone();
        tokio::spawn(async move { service.refresh(&id).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.replace(playlist(19));
    let second = service.refresh(&source.id).await.unwrap();
    assert!(matches!(second, RefreshOutcome::Committed(_)));
    assert!(matches!(first.await.unwrap(), Err(ServiceError::Cancelled)));
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

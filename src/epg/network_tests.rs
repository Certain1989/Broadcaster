// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Adversarial synthetic redirect and explicit-LAN transport tests. No
//! external HTTP requests or real subscriptions are used.
use super::network::{
    fetch_guide, fetch_guide_with, GuideCancellation, GuideFetchError, GuideResolver,
    GuideTransport, ReqwestGuideTransport, TrustScope,
};
use reqwest::{redirect::Policy, Client, Response, Url};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct FixtureResolver;
impl GuideResolver for FixtureResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, GuideFetchError> {
        let ip = match host {
            "public-fixture.invalid" => "8.8.8.8",
            "private-fixture.invalid" => "127.0.0.1",
            "mixed-fixture.invalid" => {
                return Ok(vec![
                    format!("8.8.8.8:{port}").parse().unwrap(),
                    format!("127.0.0.1:{port}").parse().unwrap(),
                ]);
            }
            _ => return Err(GuideFetchError::Network),
        };
        Ok(vec![format!("{ip}:{port}").parse().unwrap()])
    }
}
struct ReplyTransport {
    response_port: u16,
    outgoing: Arc<AtomicUsize>,
}
impl GuideTransport for ReplyTransport {
    async fn send(&self, _client: &Client, url: &Url) -> Result<Response, GuideFetchError> {
        self.outgoing.fetch_add(1, Ordering::SeqCst);
        if url.host_str() != Some("public-fixture.invalid") {
            panic!("forbidden redirected connection reached transport");
        }
        Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .build()
            .unwrap()
            .get(format!("http://127.0.0.1:{}/response", self.response_port))
            .send()
            .await
            .map_err(|_| GuideFetchError::Network)
    }
}
async fn serve_once(reply: Vec<u8>) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut read = [0u8; 1024];
        let _ = socket.read(&mut read).await.unwrap();
        socket.write_all(&reply).await.unwrap();
    });
    (port, task)
}

#[tokio::test]
async fn public_redirects_to_private_or_mixed_dns_never_reach_transport() {
    for target in ["private-fixture.invalid", "mixed-fixture.invalid"] {
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{target}/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ).into_bytes();
        let (port, server) = serve_once(response).await;
        let count = Arc::new(AtomicUsize::new(0));
        let sender = ReplyTransport {
            response_port: port,
            outgoing: count.clone(),
        };
        let result = fetch_guide_with(
            "http://public-fixture.invalid/start",
            TrustScope::Public,
            &GuideCancellation::new(),
            &FixtureResolver,
            &sender,
        )
        .await;
        assert!(matches!(result, Err(GuideFetchError::NetworkScopeDenied)));
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "redirect must fail before GET"
        );
        server.await.unwrap();
    }
}
#[tokio::test]
async fn explicit_lan_source_fetches_only_after_direct_selection() {
    let body = b"<tv></tv>";
    let mut reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    reply.extend_from_slice(body);
    let (port, server) = serve_once(reply).await;
    let url = format!("http://127.0.0.1:{port}/guide");
    let payload = fetch_guide(&url, TrustScope::ExplicitLan, &GuideCancellation::new())
        .await
        .unwrap();
    assert_eq!(payload.bytes, body);
    assert!(!payload.gzip);
    server.await.unwrap();
}
#[tokio::test]
async fn public_scope_denies_loopback_before_any_tcp_dial() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let result = fetch_guide(
        &format!("http://127.0.0.1:{port}/secret"),
        TrustScope::Public,
        &GuideCancellation::new(),
    )
    .await;
    assert!(matches!(result, Err(GuideFetchError::NetworkScopeDenied)));
    assert!(
        tokio::time::timeout(Duration::from_millis(150), listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn lan_redirects_are_rejected_even_with_same_origin() {
    let (port, server) = serve_once(
        b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec(),
    )
    .await;
    let result = fetch_guide(
        &format!("http://127.0.0.1:{port}/start"),
        TrustScope::ExplicitLan,
        &GuideCancellation::new(),
    )
    .await;
    assert!(matches!(result, Err(GuideFetchError::RedirectDenied)));
    server.await.unwrap();
}
#[tokio::test]
async fn oversized_body_headers_fail_without_allocation() {
    let (port, server) = serve_once(
        b"HTTP/1.1 200 OK\r\nContent-Length: 16777217\r\nConnection: close\r\n\r\n".to_vec(),
    )
    .await;
    let result = fetch_guide(
        &format!("http://127.0.0.1:{port}/xmltv"),
        TrustScope::ExplicitLan,
        &GuideCancellation::new(),
    )
    .await;
    assert!(matches!(result, Err(GuideFetchError::Oversize)));
    server.await.unwrap();
}
#[tokio::test]
async fn cancellation_aborts_body_stall_instead_of_waiting_for_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (started, seen) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut read = [0u8; 1024];
        let _ = socket.read(&mut read).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 128\r\n\r\n")
            .await
            .unwrap();
        let _ = started.send(());
        tokio::time::sleep(Duration::from_secs(10)).await;
    });
    let cancellation = GuideCancellation::new();
    let token = cancellation.clone();
    let future = tokio::spawn(async move {
        fetch_guide(
            &format!("http://127.0.0.1:{port}/xmltv"),
            TrustScope::ExplicitLan,
            &token,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), seen)
        .await
        .unwrap()
        .unwrap();
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(GuideFetchError::Cancelled)));
    task.abort();
}

struct PinnedLoopbackResolver {
    port: u16,
}
impl GuideResolver for PinnedLoopbackResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, GuideFetchError> {
        assert_eq!(host, "epg-peer-test.invalid");
        assert_eq!(port, self.port);
        Ok(vec![SocketAddr::from(([127, 0, 0, 1], self.port))])
    }
}

#[tokio::test]
async fn production_reqwest_transport_reaches_only_pinned_peer_and_keeps_http_host() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0u8; 4096];
        let read = socket.read(&mut request).await.unwrap();
        let text = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
        assert!(text.contains(&format!("host: epg-peer-test.invalid:{port}")));
        for forbidden in [
            "authorization:",
            "cookie:",
            "referer:",
            "x-iptv",
            "proxy-authorization:",
        ] {
            assert!(!text.contains(forbidden), "{forbidden}");
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nTEST")
            .await
            .unwrap();
    });
    // Unlike ReplyTransport, this uses the supplied production reqwest Client:
    // DNS resolves to the test peer while the URL/Host remains a distinct name.
    let payload = fetch_guide_with(
        &format!("http://epg-peer-test.invalid:{port}/xmltv"),
        TrustScope::ExplicitLan,
        &GuideCancellation::new(),
        &PinnedLoopbackResolver { port },
        &ReqwestGuideTransport,
    )
    .await
    .unwrap();
    assert_eq!(payload.bytes, b"TEST");
    server.await.unwrap();
}

#[tokio::test]
async fn actual_reqwest_client_refuses_mixed_dns_without_connecting() {
    struct Mixed;
    impl GuideResolver for Mixed {
        async fn resolve(&self, _: &str, port: u16) -> Result<Vec<SocketAddr>, GuideFetchError> {
            Ok(vec![
                SocketAddr::from(([127, 0, 0, 1], port)),
                SocketAddr::from(([8, 8, 8, 8], port)),
            ])
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let err = fetch_guide_with(
        &format!("http://epg-peer-test.invalid:{port}/xmltv"),
        TrustScope::ExplicitLan,
        &GuideCancellation::new(),
        &Mixed,
        &ReqwestGuideTransport,
    )
    .await;
    assert!(matches!(err, Err(GuideFetchError::NetworkScopeDenied)));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

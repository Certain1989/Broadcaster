// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Synthetic reqwest connection-path tests. No production network endpoints.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use reqwest::{redirect::Policy, Client, Proxy};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn injected_dns_addresses_are_the_actual_dial_and_explicit_proxy_is_disabled() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    let target_requests = Arc::new(AtomicUsize::new(0));
    let proxy_requests = Arc::new(AtomicUsize::new(0));

    let target_count = target_requests.clone();
    let target_worker = tokio::spawn(async move {
        let (mut stream, _) = target.accept().await.unwrap();
        let mut bytes = [0u8; 2048];
        let n = stream.read(&mut bytes).await.unwrap();
        assert!(String::from_utf8_lossy(&bytes[..n]).starts_with("GET /playlist "));
        target_count.fetch_add(1, Ordering::SeqCst);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let proxy_count = proxy_requests.clone();
    let proxy_worker = tokio::spawn(async move {
        if let Ok(Ok((_stream, _))) =
            tokio::time::timeout(Duration::from_millis(400), proxy.accept()).await
        {
            proxy_count.fetch_add(1, Ordering::SeqCst);
        }
    });

    // A deliberately configured proxy would resolve the .invalid hostname itself.
    // no_proxy() must instead force this connection through our pinned address.
    let client: Client = Client::builder()
        .proxy(Proxy::all(format!("http://{proxy_addr}")).unwrap())
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .resolve_to_addrs(
            "playlist-fixture.invalid",
            &[SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                target_addr.port(),
            )],
        )
        .build()
        .unwrap();

    let response = client
        .get(format!(
            "http://playlist-fixture.invalid:{}/playlist",
            target_addr.port()
        ))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    target_worker.await.unwrap();
    proxy_worker.await.unwrap();

    assert_eq!(target_requests.load(Ordering::SeqCst), 1);
    assert_eq!(proxy_requests.load(Ordering::SeqCst), 0);
}

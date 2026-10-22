// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Synthetic reqwest connection-path tests. No production network endpoints.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use reqwest::{redirect::Policy, Client, Proxy, Url};

use super::fetch::{
    authorize_hop, classify_addresses, fetch_redirects_with_resolver, AddressClass,
    FetchError, PlaylistResolver,
};
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

struct FixtureResolver {
    answers: HashMap<String, Vec<SocketAddr>>,
}

impl PlaylistResolver for FixtureResolver {
    async fn addresses(&self, hostname: &str, _port: u16) -> Result<Vec<SocketAddr>, FetchError> {
        self.answers
            .get(hostname)
            .cloned()
            .ok_or(FetchError::Network)
    }
}

fn endpoint(address: &str, port: u16) -> SocketAddr {
    SocketAddr::new(address.parse().unwrap(), port)
}

#[test]
fn mixed_answers_and_special_destinations_never_gain_local_trust() {
    let global = endpoint("8.8.8.8", 80);
    let local = endpoint("127.0.0.1", 80);

    for address in [
        "10.20.0.1",
        "172.16.0.1",
        "192.168.1.1",
        "100.64.1.1",
        "169.254.1.1",
        "::1",
        "fd00::1234",
        "fe80::1",
        "::ffff:192.168.1.1",
    ] {
        assert_eq!(
            classify_addresses(&[endpoint(address, 80)]),
            Some(AddressClass::Local),
            "{address}"
        );
        assert_eq!(
            authorize_hop(Some(AddressClass::Global), &[endpoint(address, 80)]),
            Err(FetchError::Redirect),
            "{address}"
        );
    }

    for address in [
        "0.0.0.0",
        "224.0.0.1",
        "255.255.255.255",
        "192.0.2.1",
        "198.51.100.1",
        "203.0.113.1",
        "198.18.0.1",
        "::",
        "ff02::1",
        "2001:db8::1",
        "2002:c0a8:101::1",
        "64:ff9b::c0a8:101",
    ] {
        assert_eq!(
            classify_addresses(&[endpoint(address, 80)]),
            None,
            "{address}"
        );
    }
    assert_eq!(classify_addresses(&[global]), Some(AddressClass::Global));
    assert_eq!(classify_addresses(&[local, global]), None);
    assert_eq!(
        authorize_hop(None, &[local, global]),
        Err(FetchError::InvalidOrigin)
    );
    assert_eq!(
        authorize_hop(Some(AddressClass::Global), &[local, global]),
        Err(FetchError::Redirect)
    );
    // The original explicitly local source retains its trust through
    // local -> global -> local; no redirected URL can grant it anew.
    assert_eq!(
        authorize_hop(Some(AddressClass::Local), &[global]),
        Ok(AddressClass::Global)
    );
    assert_eq!(
        authorize_hop(Some(AddressClass::Local), &[local]),
        Ok(AddressClass::Local)
    );
}

#[tokio::test]
async fn configured_all_local_hostname_dials_only_injected_address_after_redirect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let server = tokio::spawn(async move {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0u8; 2048];
            let n = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]);
            if index == 0 {
                assert!(request.starts_with("GET /start "));
                stream
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nLocation: /playlist\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .unwrap();
            } else {
                assert!(request.starts_with("GET /playlist "));
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n#EXTM3U\n",
                    )
                    .await
                    .unwrap();
            }
            seen.fetch_add(1, Ordering::SeqCst);
        }
    });

    let hostname = "nas.home.arpa";
    let resolver = FixtureResolver {
        answers: HashMap::from([(hostname.to_owned(), vec![addr])]),
    };
    let mut url = Url::parse(&format!("http://{hostname}:{}/start", addr.port())).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        fetch_redirects_with_resolver(&mut url, &resolver),
    )
    .await
    .unwrap()
    .unwrap();
    server.await.unwrap();

    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert_eq!(result.bytes, b"#EXTM3U\n");
    assert!(result.final_url.ends_with("/playlist"));
}

#[tokio::test]
async fn mixed_initial_hostname_is_denied_before_local_get() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hostname = "mixed-fixture.invalid";
    let resolver = FixtureResolver {
        answers: HashMap::from([(
            hostname.to_owned(),
            vec![addr, endpoint("8.8.8.8", addr.port())],
        )]),
    };
    let mut url = Url::parse(&format!("http://{hostname}:{}/playlist", addr.port())).unwrap();
    let result = fetch_redirects_with_resolver(&mut url, &resolver).await;
    assert!(matches!(result, Err(FetchError::InvalidOrigin)));
    assert!(
        tokio::time::timeout(Duration::from_millis(150), listener.accept())
            .await
            .is_err(),
        "mixed DNS was allowed to send a forbidden local GET"
    );
}

#[test]
fn global_origin_rebinding_to_private_is_rejected_at_every_hop() {
    let global = endpoint("1.1.1.1", 80);
    assert_eq!(authorize_hop(None, &[global]), Ok(AddressClass::Global));
    for attempt in [
        vec![endpoint("127.0.0.1", 80)],
        vec![endpoint("::1", 80)],
        vec![endpoint("10.0.0.2", 80)],
        vec![endpoint("fd00::1", 80)],
        vec![global, endpoint("192.168.0.1", 80)],
    ] {
        assert_eq!(
            authorize_hop(Some(AddressClass::Global), &attempt),
            Err(FetchError::Redirect)
        );
    }
}

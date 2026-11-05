// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded retrieval of user-selected playlists. Never expose a URL, path, header
//! or dependency error in Debug, Display, tracing or a UI-facing failure.
use std::{
    fmt,
    future::Future,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use adw::{gio, glib, prelude::*};
use reqwest::{header::LOCATION, redirect::Policy, Client, Url};
use tokio::sync::watch;

use super::parser::MAX_PLAYLIST_BYTES;

#[derive(Clone)]
pub(crate) struct Cancellation {
    sender: watch::Sender<bool>,
}

impl Cancellation {
    pub(crate) fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }

    pub(crate) fn cancel(&self) {
        self.sender.send_replace(true);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }

    async fn cancelled(&self) {
        let mut receiver = self.sender.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FetchError {
    InvalidOrigin,
    Redirect,
    HttpStatus,
    Network,
    Timeout,
    BodyTooLarge,
    LocalAccess,
    Cancelled,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidOrigin => "Invalid playlist origin",
            Self::Redirect => "Playlist redirect was rejected",
            Self::HttpStatus => "Playlist server returned an error status",
            Self::Network => "Playlist connection failed",
            Self::Timeout => "Playlist retrieval timed out",
            Self::BodyTooLarge => "Playlist exceeds the 32 MiB limit",
            Self::LocalAccess => "Local playlist cannot be accessed",
            Self::Cancelled => "Playlist retrieval was cancelled",
        })
    }
}

pub(crate) struct Download {
    pub bytes: Vec<u8>,
    /// Validated HTTP(S) URL *after* manual redirects; may carry private tokens.
    pub final_url: String,
}

pub(crate) fn http_client() -> Result<Client, FetchError> {
    Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .redirect(Policy::none())
        .build()
        .map_err(|_| FetchError::Network)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AddressClass {
    Global,
    Local,
}

/// A first hop is either public or private LAN. Loopback, link-local,
/// CGNAT and all special ranges never qualify for playlist consent.
pub(super) fn classify_addresses(addresses: &[SocketAddr]) -> Option<AddressClass> {
    let mapped = |addr: &SocketAddr| match crate::network_scope::classify(addr.ip()) {
        crate::network_scope::Destination::Public => Some(AddressClass::Global),
        crate::network_scope::Destination::Lan => Some(AddressClass::Local),
        _ => None,
    };
    let first = mapped(addresses.first()?)?;
    addresses
        .iter()
        .all(|addr| mapped(addr) == Some(first))
        .then_some(first)
}

pub(super) fn authorize_hop(
    original: Option<AddressClass>,
    addresses: &[SocketAddr],
) -> Result<AddressClass, FetchError> {
    let hop = classify_addresses(addresses).ok_or(if original.is_some() {
        FetchError::Redirect
    } else {
        FetchError::InvalidOrigin
    })?;
    if original == Some(AddressClass::Global) && hop != AddressClass::Global {
        return Err(FetchError::Redirect);
    }
    Ok(hop)
}

/// Only the addresses returned by this resolver may be passed into the
/// reqwest client's DNS override. Test implementations inject synthetic DNS.
#[allow(async_fn_in_trait)]
pub(super) trait PlaylistResolver {
    async fn addresses(&self, hostname: &str, port: u16) -> Result<Vec<SocketAddr>, FetchError>;
}

struct SystemResolver;
impl PlaylistResolver for SystemResolver {
    async fn addresses(&self, hostname: &str, port: u16) -> Result<Vec<SocketAddr>, FetchError> {
        tokio::net::lookup_host((hostname, port))
            .await
            .map(|iter| iter.collect())
            .map_err(|_| FetchError::Network)
    }
}

/// A strict fallback prevents DNS overrides from silently falling through
/// to a fresh system lookup if a hostname spelling ever fails to match.
pub(super) struct PinnedResolver {
    hostname: String,
    addresses: Vec<SocketAddr>,
}

impl PinnedResolver {
    pub(super) fn new(hostname: String, addresses: Vec<SocketAddr>) -> Self {
        Self {
            hostname,
            addresses,
        }
    }
}

impl reqwest::dns::Resolve for PinnedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let permitted = name.as_str().eq_ignore_ascii_case(&self.hostname);
        let addresses = self.addresses.clone();
        Box::pin(async move {
            if !permitted {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "unvalidated DNS request",
                )
                .into());
            }
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// A validated address set becomes the **only** dial-eligible address set.
/// Fresh per-hop clients also prevent connection-pool reuse across trust scopes.
async fn guarded_client<R: PlaylistResolver>(
    url: &Url,
    previous: Option<AddressClass>,
    allow_lan: bool,
    resolver: &R,
) -> Result<(Client, AddressClass), FetchError> {
    let hostname = url.host_str().ok_or(FetchError::InvalidOrigin)?;
    let port = url
        .port_or_known_default()
        .ok_or(FetchError::InvalidOrigin)?;
    let numeric = hostname
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .ok();
    let mut addresses = if let Some(ip) = numeric {
        vec![SocketAddr::new(ip, port)]
    } else {
        resolver.addresses(hostname, port).await?
    };
    addresses.sort_unstable();
    addresses.dedup();
    // The isolated Rust integration fixtures use literal 127.0.0.1 HTTP
    // servers. Their explicitly opted-in *test-only* transport can exercise
    // parsing/cancellation without weakening production loopback denial.
    #[cfg(test)]
    let class = if allow_lan
        && hostname == "127.0.0.1"
        && !addresses.is_empty()
        && addresses.iter().all(|a| a.ip().is_loopback())
    {
        if previous == Some(AddressClass::Global) {
            return Err(FetchError::Redirect);
        }
        AddressClass::Local
    } else {
        authorize_hop(previous, &addresses)?
    };
    #[cfg(not(test))]
    let class = authorize_hop(previous, &addresses)?;
    if class == AddressClass::Local && !allow_lan {
        return Err(if previous.is_some() {
            FetchError::Redirect
        } else {
            FetchError::InvalidOrigin
        });
    }

    // reqwest 0.13.5's no_proxy() disables system/environment and explicitly
    // configured proxies on this playlist-only client. The per-hop DNS
    // override binds validation to the exact candidate sockets used by hyper:
    // no second, unconstrained hostname lookup can change eligibility.
    let mut builder = Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .redirect(Policy::none());
    if numeric.is_none() {
        builder = builder
            .dns_resolver(std::sync::Arc::new(PinnedResolver::new(
                hostname.to_owned(),
                addresses.clone(),
            )))
            .resolve_to_addrs(hostname, &addresses);
    }
    let client = builder.build().map_err(|_| FetchError::Network)?;
    Ok((client, class))
}

fn valid_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host().is_some()
}

/// A 30-second budget spans ALL redirects and streamed chunks; the size cap
/// applies to bytes returned by reqwest's decoder, not untrusted Content-Length.
pub(crate) async fn fetch_remote(
    client: &Client,
    origin: &str,
    cancellation: &Cancellation,
    allow_lan: bool,
) -> Result<Download, FetchError> {
    fetch_remote_with_timeout(
        client,
        origin,
        cancellation,
        allow_lan,
        Duration::from_secs(30),
    )
    .await
}

/// Testable deadline, applied to the complete request including redirects.
async fn fetch_remote_with_timeout(
    client: &Client,
    origin: &str,
    cancellation: &Cancellation,
    allow_lan: bool,
    timeout: Duration,
) -> Result<Download, FetchError> {
    let mut url = Url::parse(origin).map_err(|_| FetchError::InvalidOrigin)?;
    if !valid_url(&url) || origin.len() > 8192 {
        return Err(FetchError::InvalidOrigin);
    }
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(FetchError::Cancelled),
        result = tokio::time::timeout(timeout, fetch_redirects(client, &mut url, allow_lan)) => {
            result.map_err(|_| FetchError::Timeout)?
        }
    }
}

async fn fetch_redirects(
    _client: &Client,
    url: &mut Url,
    allow_lan: bool,
) -> Result<Download, FetchError> {
    // The existing service client is retained for API compatibility; a fresh
    // per-hop pinned client is mandatory, rather than its shared connection pool.
    #[cfg(test)]
    if url.host_str() == Some("lan-fixture.invalid")
        && std::env::var_os("BROADCASTER_T1_ISOLATED_NETWORK").is_some()
    {
        return fetch_redirects_with_transport_policy(
            url,
            &ScheduledTestResolver,
            &ScheduledTestTransport,
            allow_lan,
        )
        .await;
    }
    fetch_redirects_with_resolver_with_consent(url, &SystemResolver, allow_lan).await
}

// Only in test binaries. The production guard classifies RFC1918 DNS answers;
// the fake transport maps an *admitted* request to a loopback HTTP fixture.
// No real RFC1918 or Internet socket is ever opened by this fixture.
#[cfg(test)]
fn scheduled_test_mark(message: &str) {
    use std::io::Write;
    let path = std::env::var("BROADCASTER_T1_MARKERS").expect("isolated marker path");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap()
        .write_all(message.as_bytes())
        .unwrap();
}

#[cfg(test)]
struct ScheduledTestResolver;
#[cfg(test)]
impl PlaylistResolver for ScheduledTestResolver {
    async fn addresses(&self, hostname: &str, port: u16) -> Result<Vec<SocketAddr>, FetchError> {
        assert_eq!(hostname, "lan-fixture.invalid");
        scheduled_test_mark("resolver\n");
        Ok(vec![SocketAddr::new("10.23.4.5".parse().unwrap(), port)])
    }
}

#[cfg(test)]
struct ScheduledTestTransport;
#[cfg(test)]
impl PlaylistTransport for ScheduledTestTransport {
    async fn send(
        &self,
        _guarded: &Client,
        url: &Url,
    ) -> Result<reqwest::Response, reqwest::Error> {
        assert_eq!(url.host_str(), Some("lan-fixture.invalid"));
        scheduled_test_mark("transport\n");
        let port: u16 = std::env::var("BROADCASTER_T1_LOCAL_PORT")
            .expect("positive fixture must provide a loopback port")
            .parse()
            .unwrap();
        http_client()
            .unwrap()
            .get(format!("http://127.0.0.1:{port}/playlist"))
            .send()
            .await
    }
}

pub(super) async fn fetch_redirects_with_resolver<R: PlaylistResolver>(
    url: &mut Url,
    resolver: &R,
) -> Result<Download, FetchError> {
    fetch_redirects_with_resolver_with_consent(url, resolver, false).await
}

#[cfg(test)]
pub(super) async fn fetch_redirects_with_resolver_with_consent<R: PlaylistResolver>(
    url: &mut Url,
    resolver: &R,
    allow_lan: bool,
) -> Result<Download, FetchError> {
    fetch_redirects_with_transport_policy(url, resolver, &ReqwestTransport, allow_lan).await
}

#[cfg(not(test))]
async fn fetch_redirects_with_resolver_with_consent<R: PlaylistResolver>(
    url: &mut Url,
    resolver: &R,
    allow_lan: bool,
) -> Result<Download, FetchError> {
    fetch_redirects_with_transport_policy(url, resolver, &ReqwestTransport, allow_lan).await
}

/// Production always sends through the guarded, per-hop reqwest client.
/// The test-only transport supplies a synthetic initial public HTTP response
/// without dialing a real public address, then uses the guarded client on
/// every subsequent hop to detect an unauthorized local GET.
#[allow(async_fn_in_trait)]
pub(super) trait PlaylistTransport {
    async fn send(&self, client: &Client, url: &Url) -> Result<reqwest::Response, reqwest::Error>;
}

struct ReqwestTransport;
impl PlaylistTransport for ReqwestTransport {
    async fn send(&self, client: &Client, url: &Url) -> Result<reqwest::Response, reqwest::Error> {
        client.get(url.clone()).send().await
    }
}

pub(super) async fn fetch_redirects_with_transport<R: PlaylistResolver, T: PlaylistTransport>(
    url: &mut Url,
    resolver: &R,
    transport: &T,
) -> Result<Download, FetchError> {
    fetch_redirects_with_transport_policy(url, resolver, transport, false).await
}

pub(super) async fn fetch_redirects_with_transport_policy<
    R: PlaylistResolver,
    T: PlaylistTransport,
>(
    url: &mut Url,
    resolver: &R,
    transport: &T,
    allow_lan: bool,
) -> Result<Download, FetchError> {
    let mut previous_class = None;
    let original_origin = url.origin();
    for redirects in 0..=3 {
        let (client, hop_class) = guarded_client(url, previous_class, allow_lan, resolver).await?;
        if hop_class == AddressClass::Local
            && previous_class == Some(AddressClass::Local)
            && url.origin() != original_origin
        {
            return Err(FetchError::Redirect);
        }
        previous_class = Some(hop_class);
        let mut response = transport.send(&client, url).await.map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Network
            }
        })?;
        if response.status().is_redirection() {
            if redirects == 3 {
                return Err(FetchError::Redirect);
            }
            let header = response
                .headers()
                .get(LOCATION)
                .ok_or(FetchError::Redirect)?;
            let location = header.to_str().map_err(|_| FetchError::Redirect)?;
            let next = url.join(location).map_err(|_| FetchError::Redirect)?;
            if !valid_url(&next)
                || next.as_str().len() > 8192
                || (url.scheme() == "https" && next.scheme() != "https")
            {
                return Err(FetchError::Redirect);
            }
            *url = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(FetchError::HttpStatus);
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_PLAYLIST_BYTES as u64)
        {
            return Err(FetchError::BodyTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Network
            }
        })? {
            if chunk.len() > MAX_PLAYLIST_BYTES.saturating_sub(bytes.len()) {
                return Err(FetchError::BodyTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(Download {
            bytes,
            final_url: url.as_str().to_owned(),
        });
    }
    Err(FetchError::Redirect)
}

/// GtkFileDialog owns selection/portal permission. The returned gio::File must
/// be read on a GLib context, never by turning its path into a std::fs::File.
pub(crate) async fn fetch_local(
    file: &gio::File,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, FetchError> {
    bounded_local_read(
        fetch_local_inner(file, cancellation),
        cancellation,
        Duration::from_secs(30),
    )
    .await
}

/// Entire local read, including document-portal opening and every chunk, has
/// one deadline. GioFuture::drop cancels its GIO Cancellable on timeout or
/// source cancellation; no partial bytes or last-good modifications escape.
async fn bounded_local_read<F>(
    read: F,
    cancellation: &Cancellation,
    timeout: Duration,
) -> Result<Vec<u8>, FetchError>
where
    F: Future<Output = Result<Vec<u8>, FetchError>>,
{
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(FetchError::Cancelled),
        _ = glib::timeout_future(timeout) => Err(FetchError::Timeout),
        result = read => result,
    }
}

async fn fetch_local_inner(
    file: &gio::File,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, FetchError> {
    if cancellation.is_cancelled() {
        return Err(FetchError::Cancelled);
    }
    let stream = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(FetchError::Cancelled),
        result = file.read_future(glib::Priority::DEFAULT) => {
            result.map_err(|_| FetchError::LocalAccess)?
        }
    };
    let mut contents = Vec::new();
    loop {
        if cancellation.is_cancelled() {
            return Err(FetchError::Cancelled);
        }
        let chunk = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(FetchError::Cancelled),
            result = stream.read_bytes_future(64 * 1024, glib::Priority::DEFAULT) => {
                result.map_err(|_| FetchError::LocalAccess)?
            }
        };
        if chunk.is_empty() {
            break;
        }
        if chunk.len() > MAX_PLAYLIST_BYTES.saturating_sub(contents.len()) {
            return Err(FetchError::BodyTooLarge);
        }
        contents.extend_from_slice(chunk.as_ref());
    }
    if cancellation.is_cancelled() {
        return Err(FetchError::Cancelled);
    }
    Ok(contents)
}

/// Portable when the underlying GIO implementation retains a usable URI.
/// A document-portal grant is not promised to survive reinstall/restart.
pub(crate) fn local_locator(file: &gio::File) -> Result<String, FetchError> {
    if file.uri_scheme().as_deref() != Some("file") {
        return Err(FetchError::InvalidOrigin);
    }
    let locator = file.uri().to_string();
    if locator.is_empty() || locator.len() > 8192 {
        return Err(FetchError::InvalidOrigin);
    }
    Ok(locator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_origin_and_sanitizes_errors() {
        for raw in [
            "file:///etc/passwd",
            "data:text/html,xyz",
            "ftp://localhost/x",
        ] {
            assert_eq!(fetch_remote_url_check(raw), Err(FetchError::InvalidOrigin));
        }
        for e in [
            FetchError::Network,
            FetchError::Redirect,
            FetchError::LocalAccess,
            FetchError::InvalidOrigin,
        ] {
            assert!(!format!("{e:?}: {e}").contains("CANARY-SECRET"));
        }
    }

    fn fetch_remote_url_check(value: &str) -> Result<(), FetchError> {
        let url = Url::parse(value).map_err(|_| FetchError::InvalidOrigin)?;
        if !valid_url(&url) {
            return Err(FetchError::InvalidOrigin);
        }
        Ok(())
    }

    #[tokio::test]
    async fn local_http_fetch_redirect_and_oversize() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 1024];
                let size = stream.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..size]);
                let response = if request.starts_with("GET /redirect ") {
                    "HTTP/1.1 302 Found\r\nLocation: /playlist\r\nContent-Length: 0\r\n\r\n"
                        .to_string()
                } else if request.starts_with("GET /oversize ") {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                        MAX_PLAYLIST_BYTES + 1
                    )
                } else {
                    let body = "#EXTM3U\n#EXTINF:-1,Fixture\nhttps://example.invalid/stream\n";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                };
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = http_client().unwrap();
        let cancellation = Cancellation::new();
        let fetched = fetch_remote(
            &client,
            &format!("http://{address}/redirect"),
            &cancellation,
            true,
        )
        .await
        .unwrap();
        assert!(fetched.final_url.ends_with("/playlist"));
        assert!(fetched.bytes.starts_with(b"#EXTM3U"));
        let too_big = fetch_remote(
            &client,
            &format!("http://{address}/oversize"),
            &cancellation,
            true,
        )
        .await;
        assert!(matches!(too_big, Err(FetchError::BodyTooLarge)));
        server.await.unwrap();
    }
    #[tokio::test]
    async fn rejects_malicious_redirect_redirect_loops_and_http_errors() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 1024];
                let count = stream.read(&mut request).await.unwrap();
                let first = String::from_utf8_lossy(&request[..count]);
                let response = if first.starts_with("GET /bad ") {
                    "HTTP/1.1 302 Found\r\nLocation: file:///etc/passwd\r\nContent-Length: 0\r\n\r\n"
                        .to_owned()
                } else if first.starts_with("GET /forbidden ") {
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_owned()
                } else {
                    let path = first
                        .split_ascii_whitespace()
                        .nth(1)
                        .unwrap()
                        .trim_start_matches("/chain-")
                        .parse::<usize>()
                        .unwrap();
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: /chain-{}\r\nContent-Length: 0\r\n\r\n",
                        path + 1
                    )
                };
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = http_client().unwrap();
        let token = Cancellation::new();
        for (path, expected) in [
            ("bad", FetchError::Redirect),
            ("chain-0", FetchError::Redirect),
            ("forbidden", FetchError::HttpStatus),
        ] {
            let answer =
                fetch_remote(&client, &format!("http://{address}/{path}"), &token, true).await;
            assert!(matches!(answer, Err(error) if error == expected));
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn bounded_total_deadline_and_pre_cancelled_request() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
        });
        let client = http_client().unwrap();
        let token = Cancellation::new();
        let url = format!("http://{address}/slow");
        let timed_out =
            fetch_remote_with_timeout(&client, &url, &token, true, Duration::from_millis(30)).await;
        assert!(matches!(timed_out, Err(FetchError::Timeout)));
        token.cancel();
        let cancelled = fetch_remote(&client, &url, &token, true).await;
        assert!(matches!(cancelled, Err(FetchError::Cancelled)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn failed_connect_has_only_sanitized_error_without_userinfo() {
        let client = http_client().unwrap();
        let result = fetch_remote(
            &client,
            "http://synthetic-user:SECRET-CANARY@127.0.0.1:0/list",
            &Cancellation::new(),
            true,
        )
        .await;
        assert!(matches!(result, Err(FetchError::Network)));
        let error = result.err().unwrap();
        assert!(!format!("{error:?}: {error}").contains("SECRET-CANARY"));
    }

    #[tokio::test]
    async fn rejects_streamed_body_without_content_length_above_limit() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            let block = vec![b'a'; 64 * 1024];
            let prefix = format!("{:x}\r\n", block.len());
            for _ in 0..=MAX_PLAYLIST_BYTES / block.len() {
                if socket.write_all(prefix.as_bytes()).await.is_err() {
                    break;
                }
                if socket.write_all(&block).await.is_err() {
                    break;
                }
                if socket.write_all(b"\r\n").await.is_err() {
                    break;
                }
            }
        });
        let client = http_client().unwrap();
        let result = fetch_remote(
            &client,
            &format!("http://{address}/stream"),
            &Cancellation::new(),
            true,
        )
        .await;
        assert!(matches!(result, Err(FetchError::BodyTooLarge)));
        server.await.unwrap();
    }
}

#[cfg(test)]
mod playlist_local_deadline_tests {
    use super::*;
    use std::future::pending;

    #[test]
    fn playlist_never_resolving_gio_read_times_out_and_drops_pending_operation() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let token = Cancellation::new();
                    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    struct PendingRead(std::sync::Arc<std::sync::atomic::AtomicBool>);
                    impl Drop for PendingRead {
                        fn drop(&mut self) {
                            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                    let held = PendingRead(dropped.clone());
                    let read = async move {
                        let _held = held;
                        pending::<Result<Vec<u8>, FetchError>>().await
                    };
                    let result = bounded_local_read(read, &token, Duration::from_millis(20)).await;
                    assert_eq!(result, Err(FetchError::Timeout));
                    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
                });
            })
            .unwrap();
    }

    #[test]
    fn playlist_unresponsive_local_read_is_cancellable_without_waiting_for_deadline() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let token = Cancellation::new();
                    let trigger = token.clone();
                    let cancelling = async move {
                        glib::timeout_future(Duration::from_millis(10)).await;
                        trigger.cancel();
                    };
                    let reading = bounded_local_read(
                        pending::<Result<Vec<u8>, FetchError>>(),
                        &token,
                        Duration::from_secs(30),
                    );
                    let (result, ()) = futures_util::future::join(reading, cancelling).await;
                    assert_eq!(result, Err(FetchError::Cancelled));
                });
            })
            .unwrap();
    }
}

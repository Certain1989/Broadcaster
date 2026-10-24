// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Separate guarded transport for *user-approved* XMLTV sources.
//! Deliberately does not accept IPTV playback headers or implicit playlist hints.
use super::headroom;
use reqwest::{
    dns::{Addrs, Name, Resolve, Resolving},
    header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, LOCATION},
    redirect::Policy,
    Client, Response, Url,
};
use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;

const MAX_WIRE: usize = 16 * 1024 * 1024;
const MAX_URL: usize = 8192;

#[derive(Clone)]
pub struct GuideCancellation {
    signal: watch::Sender<bool>,
}
impl GuideCancellation {
    pub fn new() -> Self {
        Self {
            signal: watch::channel(false).0,
        }
    }
    pub fn cancel(&self) {
        self.signal.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.signal.borrow()
    }
    pub(crate) async fn cancelled(&self) {
        let mut receiver = self.signal.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
}
impl Default for GuideCancellation {
    fn default() -> Self {
        Self::new()
    }
}
impl fmt::Debug for GuideCancellation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GuideCancellation(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuideFetchError {
    InvalidOrigin,
    NetworkScopeDenied,
    RedirectDenied,
    HttpStatus,
    Network,
    Storage,
    StoragePermission,
    DiskFull,
    Timeout,
    Oversize,
    WireLimit,
    Encoding,
    Cancelled,
}
impl GuideFetchError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidOrigin => "invalid_origin",
            Self::NetworkScopeDenied => "network_scope_denied",
            Self::RedirectDenied => "redirect_denied",
            Self::HttpStatus => "http_status",
            Self::Network => "network",
            Self::Storage => "storage_failed",
            Self::StoragePermission => "storage_permission",
            Self::DiskFull => "disk_full",
            Self::Timeout => "timeout",
            Self::Oversize => "oversize",
            Self::WireLimit => "oversize_wire",
            Self::Encoding => "encoding",
            Self::Cancelled => "cancelled",
        }
    }
}
impl fmt::Display for GuideFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for GuideFetchError {}

async fn download_space_allowed(path: &std::path::Path) -> bool {
    let owned = path.to_path_buf();
    // Querying mounted filesystem metadata must not block GTK or Tokio's
    // async reactor. An unavailable worker/probe is not evidence of ENOSPC.
    tokio::task::spawn_blocking(move || headroom::download_allowed(&owned))
        .await
        .unwrap_or(true)
}

fn completed_download_intervals(total_bytes: u64) -> u64 {
    total_bytes / headroom::DOWNLOAD_CHECK_BYTES
}

fn spool_io_error(error: std::io::Error) -> GuideFetchError {
    match error.kind() {
        std::io::ErrorKind::StorageFull => GuideFetchError::DiskFull,
        std::io::ErrorKind::PermissionDenied => GuideFetchError::StoragePermission,
        _ => GuideFetchError::Storage,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustScope {
    Public,
    ExplicitLan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddressClass {
    Public,
    Lan,
}

/// No URL or response headers are retained in this return type.
pub struct GuidePayload {
    pub bytes: Vec<u8>,
    pub gzip: bool,
}

/// Check the original authority before WHATWG URL canonicalization can mask
/// octal, hex, shorthand IPv4, percent-encoded authority or backslashes.
pub fn validate_remote_url(input: &str) -> Result<Url, GuideFetchError> {
    if input.len() > MAX_URL
        || input.chars().any(char::is_control)
        || input.trim() != input
        || input.contains('\\')
    {
        return Err(GuideFetchError::InvalidOrigin);
    }
    let (scheme, rest) = input
        .split_once("://")
        .ok_or(GuideFetchError::InvalidOrigin)?;
    if !matches!(scheme, "http" | "https") {
        return Err(GuideFetchError::InvalidOrigin);
    }
    let authority = rest
        .split(&['/', '?', '#'][..])
        .next()
        .ok_or(GuideFetchError::InvalidOrigin)?;
    if authority.is_empty()
        || authority.contains('@')
        || authority.contains('%')
        || authority.contains('#')
        || authority.contains(' ')
        || authority.contains('\\')
    {
        return Err(GuideFetchError::InvalidOrigin);
    }
    let url = Url::parse(input).map_err(|_| GuideFetchError::InvalidOrigin)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(GuideFetchError::InvalidOrigin);
    }
    let host = url.host_str().ok_or(GuideFetchError::InvalidOrigin)?;
    if host.is_empty() || host.ends_with('.') || host.contains("..") || host.contains('%') {
        return Err(GuideFetchError::InvalidOrigin);
    }
    let raw_host = if authority.starts_with('[') {
        authority
            .split(']')
            .next()
            .ok_or(GuideFetchError::InvalidOrigin)?
            .trim_start_matches('[')
    } else {
        authority
            .split(':')
            .next()
            .ok_or(GuideFetchError::InvalidOrigin)?
    };
    // For numeric addresses the original spelling must already be canonical.
    // For DNS hostnames case is insensitive, but URL normalization must not
    // convert an exotic numeric host spelling to an IPv4 literal.
    let normalized = host.trim_start_matches('[').trim_end_matches(']');
    if !raw_host.eq_ignore_ascii_case(normalized) {
        return Err(GuideFetchError::InvalidOrigin);
    }
    if raw_host.starts_with('[') || authority.contains('[') != authority.contains(']') {
        return Err(GuideFetchError::InvalidOrigin);
    }
    Ok(url)
}

/// Reject raw redirect authorities before URL joining can repair them.
/// Relative references cannot introduce a scheme or authority by ambiguity.
fn validate_redirect_location(base: &Url, location: &str) -> Result<Url, GuideFetchError> {
    if location.len() > MAX_URL
        || location.is_empty()
        || location.trim() != location
        || location.chars().any(char::is_control)
        || location.contains('\\')
        || location.chars().any(char::is_whitespace)
        || location.starts_with("///")
    {
        return Err(GuideFetchError::RedirectDenied);
    }
    let raw = if location.starts_with("//") {
        format!("{}:{location}", base.scheme())
    } else if location.starts_with("http://") || location.starts_with("https://") {
        location.to_owned()
    } else {
        // A colon before a path/query/fragment separator is an attempted
        // scheme (or an ambiguous authority), not a harmless relative path.
        let prefix = location.split(&['/', '?', '#'][..]).next().unwrap_or("");
        if prefix.contains(':') {
            return Err(GuideFetchError::RedirectDenied);
        }
        base.join(location)
            .map_err(|_| GuideFetchError::RedirectDenied)?
            .to_string()
    };
    validate_remote_url(&raw).map_err(|_| GuideFetchError::RedirectDenied)
}

fn ipv4_class(ip: Ipv4Addr) -> Option<AddressClass> {
    let [a, b, c, _] = ip.octets();
    if a == 127 || a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168) {
        return Some(AddressClass::Lan);
    }
    // Reject RFC6890 special-use, CGNAT, link-local/metadata, docs, multicast
    // and future-use rather than guessing whether an address is reachable.
    if a == 0
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && matches!(b, 18 | 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
    {
        return None;
    }
    Some(AddressClass::Public)
}
fn ipv6_class(ip: Ipv6Addr) -> Option<AddressClass> {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return ipv4_class(mapped);
    }
    if ip.is_loopback() || ip.is_unique_local() {
        return Some(AddressClass::Lan);
    }
    if ip.is_unspecified() || ip.is_unicast_link_local() {
        return None;
    }
    let [first, second, ..] = ip.segments();
    if first & 0xe000 != 0x2000
        || (first == 0x2001 && (second <= 0x01ff || second == 0x0db8))
        || first == 0x2002
        || first == 0x3fff
    {
        return None;
    }
    Some(AddressClass::Public)
}
fn address_class(ip: IpAddr) -> Option<AddressClass> {
    match ip {
        IpAddr::V4(v4) => ipv4_class(v4),
        IpAddr::V6(v6) => ipv6_class(v6),
    }
}
fn authorize_addresses(addrs: &[SocketAddr], trust: TrustScope) -> Result<(), GuideFetchError> {
    let expected = match trust {
        TrustScope::Public => AddressClass::Public,
        TrustScope::ExplicitLan => AddressClass::Lan,
    };
    if addrs.is_empty()
        || addrs
            .iter()
            .any(|a| address_class(a.ip()) != Some(expected))
    {
        return Err(GuideFetchError::NetworkScopeDenied);
    }
    Ok(())
}

#[allow(async_fn_in_trait)]
pub trait GuideResolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, GuideFetchError>;
}
pub struct SystemGuideResolver;
impl GuideResolver for SystemGuideResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, GuideFetchError> {
        tokio::net::lookup_host((host, port))
            .await
            .map(|addresses| addresses.collect())
            .map_err(|_| GuideFetchError::Network)
    }
}
struct OnlyApprovedDns {
    host: String,
    addresses: Vec<SocketAddr>,
}
impl Resolve for OnlyApprovedDns {
    fn resolve(&self, name: Name) -> Resolving {
        let allowed = name.as_str().eq_ignore_ascii_case(&self.host);
        let addrs = self.addresses.clone();
        Box::pin(async move {
            if !allowed {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "guide DNS scope denied",
                )
                .into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// An injected transport is used only by tests; production builds a fresh
/// peer-pinned client for each hop, with no pooled cross-origin connections.
#[allow(async_fn_in_trait)]
pub trait GuideTransport: Send + Sync {
    async fn send(&self, client: &Client, url: &Url) -> Result<Response, GuideFetchError>;
}
pub struct ReqwestGuideTransport;
impl GuideTransport for ReqwestGuideTransport {
    async fn send(&self, client: &Client, url: &Url) -> Result<Response, GuideFetchError> {
        client
            .get(url.clone())
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    GuideFetchError::Timeout
                } else {
                    GuideFetchError::Network
                }
            })
    }
}

async fn pinned_client<R: GuideResolver>(
    url: &Url,
    trust: TrustScope,
    resolver: &R,
) -> Result<Client, GuideFetchError> {
    let host = url.host_str().ok_or(GuideFetchError::InvalidOrigin)?;
    let port = url
        .port_or_known_default()
        .ok_or(GuideFetchError::InvalidOrigin)?;
    let numeric = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .ok();
    let mut addresses = if let Some(ip) = numeric {
        vec![SocketAddr::new(ip, port)]
    } else {
        resolver.resolve(host, port).await?
    };
    addresses.sort_unstable();
    addresses.dedup();
    authorize_addresses(&addresses, trust)?;
    let mut builder = Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .redirect(Policy::none());
    if numeric.is_none() {
        builder = builder
            .dns_resolver(Arc::new(OnlyApprovedDns {
                host: host.to_owned(),
                addresses: addresses.clone(),
            }))
            .resolve_to_addrs(host, &addresses);
    }
    builder.build().map_err(|_| GuideFetchError::Network)
}

pub async fn fetch_guide(
    origin: &str,
    scope: TrustScope,
    cancellation: &GuideCancellation,
) -> Result<GuidePayload, GuideFetchError> {
    fetch_guide_with(
        origin,
        scope,
        cancellation,
        &SystemGuideResolver,
        &ReqwestGuideTransport,
    )
    .await
}

/// Whole-operation timeout covers resolver, dial, redirects and blocked body.
/// Dropping the future cancels reqwest transport instead of merely setting an
/// XML parser flag; timed reads also enforce a 30 s inactivity ceiling.
pub async fn fetch_guide_with<R: GuideResolver, T: GuideTransport>(
    origin: &str,
    scope: TrustScope,
    cancellation: &GuideCancellation,
    resolver: &R,
    transport: &T,
) -> Result<GuidePayload, GuideFetchError> {
    let url = validate_remote_url(origin)?;
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(GuideFetchError::Cancelled),
        r = tokio::time::timeout(Duration::from_secs(90),
            fetch_hops(url, scope, resolver, transport)) => r.map_err(|_| GuideFetchError::Timeout)?,
    }
}
async fn fetch_hops<R: GuideResolver, T: GuideTransport>(
    mut url: Url,
    scope: TrustScope,
    resolver: &R,
    transport: &T,
) -> Result<GuidePayload, GuideFetchError> {
    let initial_origin = url.origin();
    for redirects in 0..=3 {
        let client = pinned_client(&url, scope, resolver).await?;
        let mut response = transport.send(&client, &url).await?;
        if response.status().is_redirection() {
            if redirects == 3 || scope == TrustScope::ExplicitLan {
                return Err(GuideFetchError::RedirectDenied);
            }
            let location = response
                .headers()
                .get(LOCATION)
                .ok_or(GuideFetchError::RedirectDenied)?
                .to_str()
                .map_err(|_| GuideFetchError::RedirectDenied)?;
            let next = validate_redirect_location(&url, location)?;
            if url.scheme() == "https" && next.scheme() != "https" {
                return Err(GuideFetchError::RedirectDenied);
            }
            // Public redirects cannot acquire LAN consent, even when the
            // subsequent hostname resolves to a local or mixed address set.
            url = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(GuideFetchError::HttpStatus);
        }
        let encoding = response
            .headers()
            .get(CONTENT_ENCODING)
            .map(|v| v.to_str().map_err(|_| GuideFetchError::Encoding))
            .transpose()?
            .unwrap_or("identity")
            .to_owned();
        if !matches!(encoding.as_str(), "identity" | "gzip") {
            return Err(GuideFetchError::Encoding);
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|n| n > MAX_WIRE as u64)
        {
            return Err(GuideFetchError::Oversize);
        }
        let mut body = Vec::new();
        loop {
            let chunk = tokio::time::timeout(Duration::from_secs(30), response.chunk())
                .await
                .map_err(|_| GuideFetchError::Timeout)?
                .map_err(|_| GuideFetchError::Network)?;
            let Some(chunk) = chunk else {
                break;
            };
            if chunk.len() > MAX_WIRE.saturating_sub(body.len()) {
                return Err(GuideFetchError::Oversize);
            }
            body.extend_from_slice(&chunk);
        }
        if body.is_empty() {
            return Err(GuideFetchError::Encoding);
        }
        let magic = body.starts_with(&[0x1f, 0x8b]);
        let gzip = encoding == "gzip" || magic;
        if encoding == "gzip" && !magic {
            return Err(GuideFetchError::Encoding);
        }
        return Ok(GuidePayload { bytes: body, gzip });
    }
    let _ = initial_origin;
    Err(GuideFetchError::RedirectDenied)
}

// Separate compressed/plain bounds for streaming transport. Production reads must
// never buffer a multi-gigabyte XMLTV payload in a Vec.
const CANDIDATE_COMPRESSED: u64 = 256 * 1024 * 1024;
const CANDIDATE_PLAIN: u64 = 2 * 1024 * 1024 * 1024;

/// The caller owns a private staging path and removes it on every outcome.
/// Reuses the exact peer pinning, redirect and explicit-LAN consent rules.
pub async fn fetch_guide_spooled(
    origin: &str,
    scope: TrustScope,
    cancellation: &GuideCancellation,
    path: &std::path::Path,
) -> Result<bool, GuideFetchError> {
    let url = validate_remote_url(origin)?;
    if !download_space_allowed(path).await {
        return Err(GuideFetchError::DiskFull);
    }
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(GuideFetchError::Cancelled),
        r = tokio::time::timeout(
            Duration::from_secs(90 * 60),
            fetch_spooled_hops(url, scope, &SystemGuideResolver, &ReqwestGuideTransport, path),
        ) => r.map_err(|_| GuideFetchError::Timeout)?,
    }
}

async fn fetch_spooled_hops<R: GuideResolver, T: GuideTransport>(
    mut url: Url,
    scope: TrustScope,
    resolver: &R,
    transport: &T,
    path: &std::path::Path,
) -> Result<bool, GuideFetchError> {
    use tokio::io::AsyncWriteExt;
    for redirects in 0..=3 {
        let client = pinned_client(&url, scope, resolver).await?;
        let mut response = transport.send(&client, &url).await?;
        if response.status().is_redirection() {
            if redirects == 3 || scope == TrustScope::ExplicitLan {
                return Err(GuideFetchError::RedirectDenied);
            }
            let location = response
                .headers()
                .get(LOCATION)
                .ok_or(GuideFetchError::RedirectDenied)?
                .to_str()
                .map_err(|_| GuideFetchError::RedirectDenied)?;
            let next = validate_redirect_location(&url, location)?;
            if url.scheme() == "https" && next.scheme() != "https" {
                return Err(GuideFetchError::RedirectDenied);
            }
            url = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(GuideFetchError::HttpStatus);
        }
        let encoding = response
            .headers()
            .get(CONTENT_ENCODING)
            .map(|v| v.to_str().map_err(|_| GuideFetchError::Encoding))
            .transpose()?
            .unwrap_or("identity")
            .to_owned();
        if !matches!(encoding.as_str(), "identity" | "gzip") {
            return Err(GuideFetchError::Encoding);
        }
        let declared = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let preliminary_cap = if encoding == "gzip" {
            CANDIDATE_COMPRESSED
        } else {
            CANDIDATE_PLAIN
        };
        if declared.is_some_and(|n| n > preliminary_cap) {
            return Err(GuideFetchError::WireLimit);
        }
        // Never overwrite an existing source file or follow symlinks. The
        // caller supplies a random path in its private SQLite data directory.
        if !download_space_allowed(path).await {
            return Err(GuideFetchError::DiskFull);
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(spool_io_error)?;
        let mut file = tokio::fs::File::from_std(file);
        let mut total = 0_u64;
        let mut space_checks = 0_u64;
        let mut header = [0_u8; 2];
        let mut header_bytes = 0_usize;
        let mut gzip = encoding == "gzip";
        loop {
            let chunk = tokio::time::timeout(Duration::from_secs(30), response.chunk())
                .await
                .map_err(|_| GuideFetchError::Timeout)?
                .map_err(|_| GuideFetchError::Network)?;
            let Some(chunk) = chunk else {
                break;
            };
            for &b in chunk.iter().take(2_usize.saturating_sub(header_bytes)) {
                header[header_bytes] = b;
                header_bytes += 1;
            }
            if header_bytes == 2 && header == [0x1f, 0x8b] {
                gzip = true;
            }
            let cap = if gzip {
                CANDIDATE_COMPRESSED
            } else {
                CANDIDATE_PLAIN
            };
            total = total
                .checked_add(chunk.len() as u64)
                .ok_or(GuideFetchError::WireLimit)?;
            if total > cap {
                return Err(GuideFetchError::WireLimit);
            }
            file.write_all(&chunk).await.map_err(spool_io_error)?;
            while completed_download_intervals(total) > space_checks {
                // Enforce each additional completed 16 MiB interval, without
                // trusting server Content-Length or needing a background timer.
                if !download_space_allowed(path).await {
                    return Err(GuideFetchError::DiskFull);
                }
                space_checks += 1;
            }
        }
        if total == 0 || (encoding == "gzip" && header != [0x1f, 0x8b]) {
            return Err(GuideFetchError::Encoding);
        }
        file.flush().await.map_err(spool_io_error)?;
        return Ok(gzip);
    }
    Err(GuideFetchError::RedirectDenied)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn addr(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 443)
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn spool_write_maps_real_kernel_enospc_without_filling_disk() {
        // /dev/full is a bounded Linux kernel fault device, not a filesystem
        // pressure test. Exercise the same asynchronous spool-write primitive.
        use tokio::io::AsyncWriteExt;
        let device = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("Linux provides the non-destructive /dev/full device");
        let mut spool = tokio::fs::File::from_std(device);
        // Tokio may buffer a small successful write until flush; production
        // maps errors at both write_all and flush.
        let failure = match spool.write_all(b"<tv/>").await {
            Err(error) => error,
            Ok(()) => spool
                .flush()
                .await
                .expect_err("the kernel must reject a flush to /dev/full"),
        };
        assert_eq!(failure.kind(), std::io::ErrorKind::StorageFull);
        assert_eq!(spool_io_error(failure), GuideFetchError::DiskFull);
    }

    #[test]
    fn spool_errors_distinguish_storage_full_from_other_io() {
        assert_eq!(
            spool_io_error(std::io::Error::from(std::io::ErrorKind::StorageFull)),
            GuideFetchError::DiskFull
        );
        assert_eq!(
            spool_io_error(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            GuideFetchError::StoragePermission
        );
        assert_eq!(
            spool_io_error(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            GuideFetchError::Storage
        );
    }
    #[test]
    fn remote_download_cadence_counts_exact_16_mib_boundaries() {
        let interval = headroom::DOWNLOAD_CHECK_BYTES;
        assert_eq!(completed_download_intervals(interval - 1), 0);
        assert_eq!(completed_download_intervals(interval), 1);
        assert_eq!(completed_download_intervals(interval + 1), 1);
        assert_eq!(completed_download_intervals(2 * interval - 1), 1);
        assert_eq!(completed_download_intervals(2 * interval), 2);
    }

    #[test]
    fn denies_unsafe_and_repaired_authorities() {
        for origin in [
            "http://127.1/epg",
            "http://0x7f000001/",
            "http://user:pw@example.com/",
            "file:///tmp/tv.xml",
            "https://example.com\\\\@127.0.0.1/",
            "https://user%40example.com/",
            "https://[fe80::1%25eth0]/",
            "https://example.com/#fragment",
        ] {
            assert!(validate_remote_url(origin).is_err());
        }
        assert!(validate_remote_url("https://example.com/epg.xml.gz?secret=opaque").is_ok());
    }
    #[test]
    fn raw_redirect_authorities_are_not_canonicalized_into_trust() {
        let base = Url::parse("https://example.com/a/b").unwrap();
        for location in [
            "http://127.1/feed",
            "http://0177.0.0.1/feed",
            "http://0x7f000001/feed",
            "http://2130706433/feed",
            "//127.1/secret",
            "//user%40example.com/x",
            "https://user:secret@example.com/",
            "//evil%2eexample.com/feed",
            "https://example.com\\@127.0.0.1/",
            "///127.0.0.1/",
            "javascript:alert(1)",
            "https://example.com/%0a#frag",
        ] {
            assert!(
                validate_redirect_location(&base, location).is_err(),
                "{location}"
            );
        }
        assert_eq!(
            validate_redirect_location(&base, "../guide.xml")
                .unwrap()
                .as_str(),
            "https://example.com/guide.xml"
        );
        assert_eq!(
            validate_redirect_location(&base, "//other.example/guide")
                .unwrap()
                .host_str(),
            Some("other.example")
        );
    }

    #[test]
    fn all_answers_must_satisfy_scope() {
        assert!(
            authorize_addresses(&[addr("1.1.1.1"), addr("10.0.0.1")], TrustScope::Public).is_err()
        );
        assert!(authorize_addresses(&[addr("169.254.169.254")], TrustScope::ExplicitLan).is_err());
        assert!(authorize_addresses(
            &[addr("10.0.0.1"), addr("127.0.0.1")],
            TrustScope::ExplicitLan
        )
        .is_ok());
        assert!(authorize_addresses(&[addr("2001:db8::1")], TrustScope::Public).is_err());
        assert!(authorize_addresses(&[addr("1.1.1.1")], TrustScope::Public).is_ok());
    }
    #[test]
    fn errors_never_embed_private_locator() {
        let secret = "https://example.com/feed?token=my-private-token";
        let error = validate_remote_url(&format!("{secret}#broken")).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("my-private-token"));
    }
}

// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Lazy, best-effort raster logos for visible channel rows. Never log a
//! playlist-supplied URL: its path, userinfo or query may contain credentials.
//! This module runs on the Tokio worker and never blocks the GTK main loop.
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use adw::glib;
use reqwest::{
    dns::{Addrs, Name, Resolve, Resolving},
    redirect::Policy,
    Client, Url,
};
use ring::digest::{digest, SHA256};
use tokio::sync::Semaphore;

const MAX_BYTES: usize = 2 * 1024 * 1024;
// GDK currently decodes on GTK's main thread, so refuse full-HD
// broadcaster artwork rather than risk blocking remote input. Keep the
// raster work at <= 1 MiB decoded per channel until a worker-side
// downscale path exists. A missing logo is preferable to UI stalls.
const MAX_EDGE: u32 = 512;
const MAX_PIXELS: u64 = 512 * 512;
// Hundreds of channels are normal for one IPTV source. Keep disk capacity
// independently generous; small bounded decoded textures still protect GTK.
const MAX_DISK_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DISK_ENTRIES: usize = 2048;

fn client() -> Option<&'static Client> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                // Imported tvg-logo targets are not user-authorized network
                // origins. Proxies resolve the final host outside this client's
                // control, so only direct logo connections can be enforced.
                // User-configured playlist fetching uses a separate client.
                .no_proxy()
                .dns_resolver(PublicLogoDns)
                .redirect(Policy::custom(|attempt| {
                    // Many legitimate logo hosts use HTTP 301/302 or a CDN.
                    // Follow at most three safe redirects; never follow to
                    // local files, embedded credentials or an HTTPS downgrade.
                    if attempt.previous().len() >= 3
                        || !allowed_url(attempt.url().as_str())
                        || attempt
                            .previous()
                            .last()
                            .is_some_and(|previous| !safe_redirect(previous, attempt.url()))
                    {
                        attempt.stop()
                    } else {
                        attempt.follow()
                    }
                }))
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(12))
                .build()
                .ok()
        })
        .as_ref()
}

fn semaphore() -> &'static Semaphore {
    static LIMIT: OnceLock<Semaphore> = OnceLock::new();
    LIMIT.get_or_init(|| Semaphore::new(4))
}

// Reject special-purpose/non-global IP allocations conservatively. A
// public-looking DNS name is checked again by PublicLogoDns for EVERY socket
// resolution. Literal IP hosts are handled here, as hyper need not call DNS.
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_ipv4(ip),
        IpAddr::V6(ip) => public_ipv6(ip),
    }
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !matches!(
        (a, b, c),
        (0, _, _)                  // this network / unspecified
            | (10, _, _)            // RFC1918
            | (100, 64..=127, _)    // shared address space
            | (127, _, _)           // loopback
            | (169, 254, _)         // link-local
            | (172, 16..=31, _)     // RFC1918
            | (192, 0, 0)           // IETF protocol assignments (conservative)
            | (192, 0, 2)           // documentation
            | (192, 88, 99)         // deprecated 6to4 relay
            | (192, 168, _)         // RFC1918
            | (198, 18..=19, _)     // benchmark networks
            | (198, 51, 100)        // documentation
            | (203, 0, 113)         // documentation
            | (224..=255, _, _)     // multicast / reserved / broadcast
    )
}

fn public_ipv6(ip: Ipv6Addr) -> bool {
    // IPv4-mapped IPv6 and other transition/translation addresses must not
    // evade IPv4 or global-unicast restrictions. Be deliberately conservative:
    // unallocated and special-use prefixes simply fall back to generic icons.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_ipv4(v4);
    }
    let segments = ip.segments();
    let first = segments[0];
    first & 0xe000 == 0x2000 // currently allocated global unicast 2000::/3
        && !(first == 0x2001 && segments[1] < 0x0200) // IETF special-use /23
        && !(first == 0x2001 && segments[1] == 0x0db8) // documentation /32
        && first != 0x2002 // 6to4 tunneling /16
        && !(first == 0x3fff && segments[1] & 0xf000 == 0) // documentation /20
}

// Fail closed for a mixed public/private DNS answer, even if the socket
// connector would otherwise try only a public candidate first. All returned
// SocketAddrs are those handed directly to reqwest's dial path: no separate,
// TOCTOU-prone hostname preflight or subsequent unchecked DNS resolution.
fn public_addresses(addrs: impl IntoIterator<Item = SocketAddr>) -> Option<Vec<SocketAddr>> {
    let addresses: Vec<_> = addrs.into_iter().collect();
    (!addresses.is_empty() && addresses.iter().all(|addr| public_ip(addr.ip())))
        .then_some(addresses)
}

struct PublicLogoDns;

impl Resolve for PublicLogoDns {
    fn resolve(&self, name: Name) -> Resolving {
        let hostname = name.as_str().to_owned();
        Box::pin(async move {
            let answers = tokio::net::lookup_host((hostname.as_str(), 0)).await?;
            let permitted = public_addresses(answers).ok_or_else(|| {
                io::Error::new(io::ErrorKind::PermissionDenied, "logo destination blocked")
            })?;
            Ok(Box::new(permitted.into_iter()) as Addrs)
        })
    }
}

/// URL checks are deliberately separate from stream and playlist retrieval.
pub(super) fn allowed_url(raw: &str) -> bool {
    if raw.len() > 8192 {
        return false;
    }
    Url::parse(raw).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            // URL parsing canonicalizes unusual IPv4 forms (e.g. integer or
            // hexadecimal hosts). Also handle IPv6 bracket spelling.
            && url.host_str().is_some_and(|host| {
                let literal = host.trim_start_matches('[').trim_end_matches(']');
                literal.parse::<IpAddr>().map_or(true, public_ip)
            })
    })
}

fn safe_redirect(previous: &Url, next: &Url) -> bool {
    allowed_url(next.as_str()) && !(previous.scheme() == "https" && next.scheme() != "https")
}

fn allowed_dimensions(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_EDGE
        && height <= MAX_EDGE
        && u64::from(width) * u64::from(height) <= MAX_PIXELS
}

fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 24 || !data.starts_with(b"\x89PNG\r\n\x1a\n") || &data[12..16] != b"IHDR" {
        return None;
    }
    Some((
        u32::from_be_bytes(data[16..20].try_into().ok()?),
        u32::from_be_bytes(data[20..24].try_into().ok()?),
    ))
}

fn jpeg_size(data: &[u8]) -> Option<(u32, u32)> {
    if !data.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut cursor = 2usize;
    while cursor + 4 <= data.len() {
        if data[cursor] != 0xff {
            return None;
        }
        while cursor < data.len() && data[cursor] == 0xff {
            cursor += 1;
        }
        let marker = *data.get(cursor)?;
        cursor += 1;
        if marker == 0xda || marker == 0xd9 {
            return None;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = usize::from(u16::from_be_bytes(
            data.get(cursor..cursor + 2)?.try_into().ok()?,
        ));
        if length < 2 || cursor.checked_add(length)? > data.len() {
            return None;
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            if length < 7 {
                return None;
            }
            return Some((
                u32::from(u16::from_be_bytes([data[cursor + 5], data[cursor + 6]])),
                u32::from(u16::from_be_bytes([data[cursor + 3], data[cursor + 4]])),
            ));
        }
        cursor += length;
    }
    None
}

fn webp_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 30 || &data[..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return None;
    }
    match &data[12..16] {
        b"VP8X" => {
            // Animated logos are not needed for a TV channel list.
            if data[20] & 0x02 != 0 {
                return None;
            }
            Some((
                1 + u32::from(data[24]) + (u32::from(data[25]) << 8) + (u32::from(data[26]) << 16),
                1 + u32::from(data[27]) + (u32::from(data[28]) << 8) + (u32::from(data[29]) << 16),
            ))
        }
        b"VP8 " => {
            if data.len() < 30 || data[23..26] != [0x9d, 0x01, 0x2a] {
                return None;
            }
            Some((
                u32::from(u16::from_le_bytes([data[26], data[27]]) & 0x3fff),
                u32::from(u16::from_le_bytes([data[28], data[29]]) & 0x3fff),
            ))
        }
        b"VP8L" => {
            if data.len() < 25 || data[20] != 0x2f {
                return None;
            }
            Some((
                1 + u32::from(data[21]) + (u32::from(data[22] & 0x3f) << 8),
                1 + (u32::from(data[22]) >> 6)
                    + (u32::from(data[23]) << 2)
                    + (u32::from(data[24] & 0x0f) << 10),
            ))
        }
        _ => None,
    }
}

/// Check the actual raster signature AND dimensions before asking GDK to
/// allocate a decoded texture. SVG, HTML, GIF and huge raster images fail.
pub(super) fn valid_raster(data: &[u8]) -> bool {
    if data.is_empty() || data.len() > MAX_BYTES {
        return false;
    }
    png_size(data)
        .or_else(|| jpeg_size(data))
        .or_else(|| webp_size(data))
        .is_some_and(|(w, h)| allowed_dimensions(w, h))
}

fn cache_dir() -> Option<PathBuf> {
    let path = glib::user_cache_dir().join("broadcaster").join("logos");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(&path).ok()?;
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || (metadata.permissions().mode() & 0o077 != 0)
            || metadata.uid() != unsafe { geteuid() }
        {
            return None;
        }
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(&path).ok()?;
        if !fs::symlink_metadata(&path).ok()?.is_dir() {
            return None;
        }
    }
    Some(path)
}

#[cfg(unix)]
unsafe extern "C" {
    fn geteuid() -> u32;
}

/// Stable, URL-derived filename; neither a channel name nor a credentialed
/// playlist URL is ever used verbatim as a filesystem component.
pub(super) fn cache_name(url: &str) -> String {
    let key = digest(&SHA256, url.as_bytes());
    let mut name = String::with_capacity(68);
    for byte in key.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(&mut name, "{byte:02x}");
    }
    name.push_str(".img");
    name
}

fn path_for(dir: &Path, url: &str) -> PathBuf {
    dir.join(cache_name(url))
}

fn read_cached(file: &Path) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(file).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_BYTES as u64 {
        return None;
    }
    let bytes = fs::read(file).ok()?;
    valid_raster(&bytes).then_some(bytes)
}

fn write_cached(file: &Path, data: &[u8]) {
    let temp = file.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(&temp)?;
        output.write_all(data)?;
        output.sync_all()?;
        fs::rename(&temp, file)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
}

/// Cache maintenance only examines the private, fixed directory. No URL,
/// token, hostname or channel name is used as a filesystem component.
fn evict(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == "img") {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        total = total.saturating_add(meta.len());
        files.push((path, meta.len(), meta.modified().ok()));
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    let mut remaining = files.len();
    for (path, size, _) in files {
        if remaining <= MAX_DISK_ENTRIES && total <= MAX_DISK_BYTES {
            break;
        }
        if fs::remove_file(path).is_ok() {
            remaining -= 1;
            total = total.saturating_sub(size);
        }
    }
}

/// Best-effort private-cache garbage collection, only after an authoritative
/// successful source mutation. Referenced names cover ALL present imported
/// channels across sources, including sources the user temporarily disabled.
/// A failed database query MUST NOT trigger deletion.
pub(super) fn prune_unused(referenced: &HashSet<String>) {
    if let Some(dir) = cache_dir() {
        prune_unused_in(&dir, referenced);
    }
}

fn prune_unused_in(dir: &Path, referenced: &HashSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let filename = entry.file_name();
        let Some(name) = filename.to_str() else {
            continue;
        };
        // Refuse to touch anything except our own 64-hex SHA-256 files.
        let Some(hash) = name.strip_suffix(".img") else {
            continue;
        };
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        if referenced.contains(name) {
            continue;
        }
        let path = entry.path();
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_file()) {
            let _ = fs::remove_file(path);
        }
    }
}

async fn download(url: &str) -> Option<Vec<u8>> {
    if !allowed_url(url) {
        return None;
    }
    let client = client()?;
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|bytes| bytes > MAX_BYTES as u64)
    {
        return None;
    }
    let mut response = response;
    let mut result = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if chunk.len() > MAX_BYTES.saturating_sub(result.len()) {
            return None;
        }
        result.extend_from_slice(&chunk);
    }
    valid_raster(&result).then_some(result)
}

/// Worker-only call. Client and semaphore are shared across all visible rows.
pub(super) async fn load(url: String) -> Option<Vec<u8>> {
    if !allowed_url(&url) {
        return None;
    }
    let directory = cache_dir();
    let path = directory.as_ref().map(|dir| path_for(dir, &url));
    if let Some(bytes) = path.as_deref().and_then(read_cached) {
        return Some(bytes);
    }
    let _permit = semaphore().acquire().await.ok()?;
    // A second waiter might have populated the cache while this request queued.
    if let Some(bytes) = path.as_deref().and_then(read_cached) {
        return Some(bytes);
    }
    // One deadline covers every redirect plus the complete decoded body.
    let bytes = tokio::time::timeout(Duration::from_secs(12), download(&url))
        .await
        .ok()??;
    if let Some(path) = &path {
        write_cached(path, &bytes);
        if let Some(dir) = directory.as_deref() {
            evict(dir);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_urls_and_large_or_active_content() {
        for url in [
            "file:///etc/passwd",
            "data:image/svg+xml,%3Csvg%3E",
            "https://user:SECRET-CANARY@example.invalid/logo.png",
            "javascript:alert(1)",
        ] {
            assert!(!allowed_url(url));
        }
        assert!(allowed_url("https://example.invalid/logo.png"));
        for input in [
            &b"<svg xmlns='http://www.w3.org/2000/svg'/>"[..],
            &b"<html>SECRET-CANARY</html>"[..],
            &b"GIF89a"[..],
        ] {
            assert!(!valid_raster(input));
        }
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&2048u32.to_be_bytes());
        png.extend_from_slice(&2048u32.to_be_bytes());
        assert!(!valid_raster(&png));
        png[16..20].copy_from_slice(&1920u32.to_be_bytes());
        png[20..24].copy_from_slice(&1080u32.to_be_bytes());
        assert!(!valid_raster(&png)); // No full-HD decode in GTK thread.
        png[16..20].copy_from_slice(&512u32.to_be_bytes());
        png[20..24].copy_from_slice(&512u32.to_be_bytes());
        assert!(valid_raster(&png));
        png[16..20].copy_from_slice(&32u32.to_be_bytes());
        png[20..24].copy_from_slice(&16u32.to_be_bytes());
        assert!(valid_raster(&png));
    }

    #[test]
    fn redirect_urls_never_gain_authority_or_downgrade_https() {
        let secure = Url::parse("https://example.invalid/a.png").unwrap();
        let insecure = Url::parse("http://example.invalid/a.png").unwrap();
        let hosted = Url::parse("https://cdn.example.invalid/a.png").unwrap();
        let credentialed =
            Url::parse("https://user:SECRET-CANARY@cdn.example.invalid/a.png").unwrap();
        let file = Url::parse("file:///etc/passwd").unwrap();
        assert!(safe_redirect(&secure, &hosted));
        assert!(safe_redirect(&insecure, &hosted));
        assert!(!safe_redirect(&secure, &insecure));
        assert!(!safe_redirect(&secure, &credentialed));
        assert!(!safe_redirect(&secure, &file));
    }

    #[test]
    fn rejects_local_literals_and_non_global_ipv4_ipv6() {
        for host in [
            "127.0.0.1",
            "127.25.1.2",
            "10.1.2.3",
            "172.16.1.1",
            "172.31.255.255",
            "192.168.50.2",
            "169.254.1.2",
            "100.64.2.3",
            "0.0.0.0",
            "192.0.2.2",
            "198.18.1.1",
            "198.51.100.10",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "[::]",
            "[::1]",
            "[fe80::1]",
            "[fc00::1]",
            "[fd42::1]",
            "[ff02::1]",
            "[2001:db8::42]",
            "[2002:c0a8:101::]",
            "[::ffff:127.0.0.1]",
            "[::ffff:192.168.1.1]",
            "[3fff::1]",
        ] {
            let url = format!("http://{host}/logo.png");
            assert!(!allowed_url(&url), "{host}");
        }
        // url::Url canonicalizes these alternate encodings before our check.
        for url in [
            "http://2130706433/logo.png",
            "http://0x7f000001/logo.png",
            "http://0177.0.0.1/logo.png",
        ] {
            assert!(!allowed_url(url));
        }
        for host in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.215.14",
            "[2606:4700:4700::1111]",
            "[2001:4860:4860::8888]",
        ] {
            assert!(allowed_url(&format!("https://{host}/logo.png")), "{host}");
        }
    }

    #[test]
    fn resolver_denies_private_answers_and_dns_rebinding() {
        let public_a = "8.8.8.8:80".parse::<SocketAddr>().unwrap();
        let public_aaaa = "[2606:4700:4700::1111]:80"
            .parse::<SocketAddr>()
            .unwrap();
        let private_a = "192.168.1.1:80".parse::<SocketAddr>().unwrap();
        let local_aaaa = "[::1]:80".parse::<SocketAddr>().unwrap();
        assert_eq!(public_addresses([public_a]), Some(vec![public_a]));
        assert_eq!(
            public_addresses([public_a, public_aaaa]),
            Some(vec![public_a, public_aaaa])
        );
        // A public-looking DNS name's resolved addresses are not trusted.
        assert!(public_addresses([private_a]).is_none());
        assert!(public_addresses([public_a, private_a]).is_none());
        assert!(public_addresses([local_aaaa, public_aaaa]).is_none());
        assert!(public_addresses(std::iter::empty()).is_none());
        // On each re-resolution the connector gets a fresh filtered set.
        assert!(public_addresses([public_a]).is_some());
        assert!(public_addresses([private_a]).is_none());
    }

    #[test]
    fn redirects_block_local_literals_and_keep_safe_cdn_routes() {
        let original = Url::parse("https://logos.example.invalid/image.png").unwrap();
        let cdn = Url::parse("https://cdn.example.invalid/image.png").unwrap();
        let private = Url::parse("https://[::1]/image.png").unwrap();
        let integer = Url::parse("http://2130706433/image.png").unwrap();
        assert!(safe_redirect(&original, &cdn));
        assert!(!safe_redirect(&original, &private));
        assert!(!safe_redirect(&original, &integer));
        assert!(!safe_redirect(&cdn, &Url::parse("http://cdn.example.invalid/x").unwrap()));
    }

    #[tokio::test]
    async fn blocked_local_logo_never_connects_to_fixture() {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/logo.png", listener.local_addr().unwrap());
        assert!(download(&url).await.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "rejected logo caused a local connection"
        );
    }

    #[test]
    fn bounded_jpeg_header_parser() {
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0, 0, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x10,
            0x00, 0x20, 0x03, 0x01, 0x11, 0, 0x02, 0x11, 0, 0x03, 0x11, 0,
        ];
        assert_eq!(jpeg_size(&jpeg), Some((32, 16)));
        assert!(valid_raster(&jpeg));
        assert!(!valid_raster(&jpeg[..8]));
    }
    #[test]
    fn disk_cache_keeps_more_than_a_country_playlist() {
        let dir =
            std::env::temp_dir().join(format!("broadcaster-logo-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        for i in 0..325 {
            let name = cache_name(&format!("https://example.invalid/logo/{i}.png"));
            fs::write(dir.join(name), [0u8; 16]).unwrap();
        }
        evict(&dir);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 325);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prunes_only_orphaned_private_cache_images() {
        let dir =
            std::env::temp_dir().join(format!("broadcaster-logo-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let shared = cache_name("https://example.invalid/shared.png");
        let removed = cache_name("https://example.invalid/removed.png");
        fs::write(dir.join(&shared), [1u8; 8]).unwrap();
        fs::write(dir.join(&removed), [2u8; 8]).unwrap();
        fs::write(dir.join("unrelated.img"), b"untouched").unwrap();
        fs::write(dir.join("pending.tmp"), b"untouched").unwrap();

        prune_unused_in(&dir, &HashSet::from([shared.clone()]));
        assert!(dir.join(shared).exists());
        assert!(!dir.join(removed).exists());
        assert!(dir.join("unrelated.img").exists());
        assert!(dir.join("pending.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}

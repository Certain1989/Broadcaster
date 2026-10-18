// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Lazy, best-effort raster logos for visible channel rows. Never log a
//! playlist-supplied URL: its path, userinfo or query may contain credentials.
//! This module runs on the Tokio worker and never blocks the GTK main loop.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use adw::glib;
use reqwest::{redirect::Policy, Client, Url};
use ring::digest::{digest, SHA256};
use tokio::sync::Semaphore;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_EDGE: u32 = 1024;
const MAX_PIXELS: u64 = 1024 * 1024;
const MAX_DISK_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DISK_ENTRIES: usize = 128;

fn client() -> Option<&'static Client> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .redirect(Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(8))
                .build()
                .ok()
        })
        .as_ref()
}

fn semaphore() -> &'static Semaphore {
    static LIMIT: OnceLock<Semaphore> = OnceLock::new();
    LIMIT.get_or_init(|| Semaphore::new(4))
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
    })
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

fn path_for(dir: &Path, url: &str) -> PathBuf {
    let key = digest(&SHA256, url.as_bytes());
    let mut name = String::with_capacity(68);
    for byte in key.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(&mut name, "{byte:02x}");
    }
    name.push_str(".img");
    dir.join(name)
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

async fn download(url: &str) -> Option<Vec<u8>> {
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
    let bytes = download(&url).await?;
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
        png[16..20].copy_from_slice(&32u32.to_be_bytes());
        png[20..24].copy_from_slice(&16u32.to_be_bytes());
        assert!(valid_raster(&png));
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
}

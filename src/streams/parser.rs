// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure, bounded M3U parsing. Never render untrusted playlist values in diagnostics.
use std::collections::BTreeSet;
use std::fmt;

use reqwest::Url;

use super::model::{normalize_facet, FacetDimension, IncomingChannel, Region};

pub(crate) const MAX_PLAYLIST_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_CANDIDATES: usize = 50_000;
pub(crate) const MAX_LINE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_ATTRIBUTE_BYTES: usize = 4 * 1024;
pub(crate) const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_WARNINGS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParseError {
    BodyTooLarge,
    InvalidUtf8,
    InvalidOrigin,
    TooManyCandidates,
    HlsManifest,
    NoPlayableChannels,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BodyTooLarge => "playlist body exceeds limit",
            Self::InvalidUtf8 => "playlist is not valid UTF-8",
            Self::InvalidOrigin => "playlist origin is not a valid HTTP(S) URL",
            Self::TooManyCandidates => "playlist contains too many candidate channels",
            Self::HlsManifest => "HLS manifest is not an IPTV channel catalogue",
            Self::NoPlayableChannels => "playlist contains no playable HTTP(S) channels",
        })
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WarningKind {
    InvalidAttribute,
    OversizedAttribute,
    InvalidMetadata,
    InvalidFacet,
    InvalidStreamUrl,
    InvalidLogoUrl,
    InvalidReferer,
    OversizedLine,
    IncompleteEntry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParseWarning {
    pub kind: WarningKind,
    pub count: usize,
}

/// Contains S1's canonical DTO; no second import model or identity algorithm.
pub(crate) struct ParsedPlaylist {
    pub channels: Vec<IncomingChannel>,
    pub candidates: usize,
    pub skipped: usize,
    pub source_label: Option<String>,
    /// Aggregated, bounded and deliberately free of user-controlled text.
    pub warnings: Vec<ParseWarning>,
}

#[derive(Default)]
struct Metadata {
    title: Option<String>,
    tvg_id: Option<String>,
    tvg_name: Option<String>,
    group_title: Option<String>,
    logo_url: Option<String>,
    channel_number: Option<String>,
    region_code: Option<String>,
    region_label: Option<String>,
    languages: BTreeSet<String>,
    countries: BTreeSet<String>,
    user_agent: Option<String>,
    referer: Option<String>,
}

#[derive(Default)]
struct Diagnostics {
    skipped: usize,
    warnings: Vec<ParseWarning>,
}

impl Diagnostics {
    fn warn(&mut self, kind: WarningKind) {
        if let Some(warning) = self.warnings.iter_mut().find(|w| w.kind == kind) {
            warning.count = warning.count.saturating_add(1);
        } else if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(ParseWarning { kind, count: 1 });
        }
    }

    fn skip(&mut self, kind: WarningKind) {
        self.skipped = self.skipped.saturating_add(1);
        self.warn(kind);
    }
}

/// `base_origin=None` means a local M3U file: only absolute HTTP(S) media
/// addresses are accepted. `Some(final_playlist_url)` allows HTTP(S)-relative
/// media and logo addresses. The caller must supply the final redirect target.
pub(crate) fn parse_playlist(
    bytes: &[u8],
    base_origin: Option<&str>,
) -> Result<ParsedPlaylist, ParseError> {
    if bytes.len() > MAX_PLAYLIST_BYTES {
        return Err(ParseError::BodyTooLarge);
    }
    let base = match base_origin {
        Some(origin) => Some(validate_base(origin)?),
        None => None,
    };
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidUtf8)?;

    let mut channels = Vec::new();
    let mut candidates = 0usize;
    let mut diagnostics = Diagnostics::default();
    let mut pending = Metadata::default();
    let mut has_extinf = false;
    let mut group_hint: Option<String> = None;
    let mut source_label = None;

    for raw_line in text.split(['\n', '\r']) {
        let line = raw_line.trim();
        if line.starts_with("#EXT-X-") {
            return Err(ParseError::HlsManifest);
        }
        if raw_line.len() > MAX_LINE_BYTES {
            // An overlong line is not retained or echoed, even in a warning.
            diagnostics.skip(WarningKind::OversizedLine);
            pending = Metadata::default();
            has_extinf = false;
            group_hint = None;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if let Some(body) = line.strip_prefix("#EXTINF:") {
            if has_extinf {
                diagnostics.skip(WarningKind::IncompleteEntry);
            }
            pending = Metadata::default();
            parse_extinf(body, &mut pending, &mut diagnostics);
            has_extinf = true;
            continue;
        }
        if let Some(group) = line.strip_prefix("#EXTGRP:") {
            group_hint = bounded_text(group, 256, &mut diagnostics);
            continue;
        }
        if let Some(value) = line.strip_prefix("#PLAYLIST:") {
            if source_label.is_none() {
                source_label = bounded_text(value, 128, &mut diagnostics);
            }
            continue;
        }
        if let Some(header) = line.strip_prefix("#EXTM3U") {
            if source_label.is_none() && !header.is_empty() {
                let mut label = Metadata::default();
                parse_attributes(header, &mut label, &mut diagnostics, true);
                source_label = label.title;
            }
            continue;
        }
        if let Some(option) = line.strip_prefix("#EXTVLCOPT:") {
            if let Some((key, value)) = option.split_once('=') {
                // Only recognized headers are retained as data. No mpv flag passthrough.
                if key.len() <= 64 && value.len() <= MAX_ATTRIBUTE_BYTES {
                    assign_attribute(key.trim(), value.trim(), &mut pending, &mut diagnostics);
                } else {
                    diagnostics.warn(WarningKind::OversizedAttribute);
                }
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }

        candidates += 1;
        if candidates > MAX_CANDIDATES {
            return Err(ParseError::TooManyCandidates);
        }
        let metadata = std::mem::take(&mut pending);
        has_extinf = false;
        let hint = group_hint.take();
        match build_channel(line, candidates as u32, metadata, hint, base.as_ref(), &mut diagnostics) {
            Some(channel) => channels.push(channel),
            None => diagnostics.skip(WarningKind::InvalidStreamUrl),
        }
    }
    if has_extinf {
        diagnostics.skip(WarningKind::IncompleteEntry);
    }
    if channels.is_empty() {
        return Err(ParseError::NoPlayableChannels);
    }
    Ok(ParsedPlaylist {
        channels,
        candidates,
        skipped: diagnostics.skipped,
        source_label,
        warnings: diagnostics.warnings,
    })
}

fn validate_base(raw: &str) -> Result<Url, ParseError> {
    if raw.len() > MAX_URL_BYTES || unsafe_url_text(raw) {
        return Err(ParseError::InvalidOrigin);
    }
    let url = Url::parse(raw).map_err(|_| ParseError::InvalidOrigin)?;
    if !allowed_url(&url) {
        return Err(ParseError::InvalidOrigin);
    }
    Ok(url)
}

fn unsafe_url_text(s: &str) -> bool {
    s.is_empty() || s.bytes().any(|b| b <= b' ' || b == b'\\' || b == 0x7f)
}

fn allowed_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host().is_some()
}

fn media_url(raw: &str, base: Option<&Url>) -> Option<String> {
    let raw = raw.trim();
    if raw.len() > MAX_URL_BYTES || unsafe_url_text(raw) {
        return None;
    }
    if let Ok(url) = Url::parse(raw) {
        // Keep the original absolute string, including every auth query token;
        // parsing/normalizing it for storage could alter S1's identity key.
        return allowed_url(&url).then(|| raw.to_owned());
    }
    let base = base?;
    let url = base.join(raw).ok()?;
    (allowed_url(&url) && url.as_str().len() <= MAX_URL_BYTES).then(|| url.into())
}

fn build_channel(
    raw_url: &str,
    order: u32,
    mut metadata: Metadata,
    group_hint: Option<String>,
    base: Option<&Url>,
    diagnostics: &mut Diagnostics,
) -> Option<IncomingChannel> {
    let stream_url = media_url(raw_url, base)?;
    let imported_name = metadata
        .title
        .take()
        .or_else(|| metadata.tvg_name.clone())
        .unwrap_or_else(|| format!("Channel {order}"));
    let logo_url = metadata.logo_url.and_then(|url| {
        let parsed = media_url(&url, base);
        if parsed.is_none() {
            diagnostics.warn(WarningKind::InvalidLogoUrl);
        }
        parsed
    });
    let referer = metadata.referer.and_then(|url| {
        let parsed = media_url(&url, base);
        if parsed.is_none() {
            diagnostics.warn(WarningKind::InvalidReferer);
        }
        parsed
    });
    let region = if metadata.region_code.is_some() || metadata.region_label.is_some() {
        Some(Region {
            code: metadata.region_code,
            label: metadata.region_label,
        })
    } else {
        None
    };
    let channel = IncomingChannel {
        order,
        imported_name,
        stream_url,
        tvg_id: metadata.tvg_id,
        tvg_name: metadata.tvg_name,
        group_title: metadata.group_title.or(group_hint),
        logo_url,
        channel_number: metadata.channel_number,
        region,
        languages: metadata.languages,
        countries: metadata.countries,
        user_agent: metadata.user_agent,
        referer,
    };
    if channel.validate().is_err() {
        diagnostics.warn(WarningKind::InvalidMetadata);
        return None;
    }
    Some(channel)
}

fn bounded_text(raw: &str, max: usize, diagnostics: &mut Diagnostics) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > max || trimmed.chars().any(char::is_control) {
        diagnostics.warn(WarningKind::InvalidMetadata);
        return None;
    }
    Some(trimmed.to_owned())
}

fn parse_extinf(raw: &str, metadata: &mut Metadata, diagnostics: &mut Diagnostics) {
    let mut in_quotes = false;
    let mut escaped = false;
    let mut comma = None;
    for (i, ch) in raw.char_indices() {
        if in_quotes && escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                comma = Some(i);
                break;
            }
            _ => {}
        }
    }
    if in_quotes {
        diagnostics.warn(WarningKind::InvalidAttribute);
    }
    let (header, title) = if let Some(at) = comma {
        (&raw[..at], Some(&raw[at + 1..]))
    } else {
        (raw, None)
    };
    if let Some(title) = title {
        metadata.title = bounded_text(title, 512, diagnostics);
    }
    let header = header.trim();
    if let Some(at) = header.find(char::is_whitespace) {
        parse_attributes(&header[at..], metadata, diagnostics, false);
    } else if header.contains('=') {
        parse_attributes(header, metadata, diagnostics, false);
    }
}

/// Tokenize key=value pairs with whitespace, quoted commas and escaped quotes.
/// The entire input is already line-bounded and each retained value is 4 KiB max.
fn parse_attributes(
    input: &str,
    metadata: &mut Metadata,
    diagnostics: &mut Diagnostics,
    playlist_header: bool,
) {
    let data = input.as_bytes();
    let mut pos = 0;
    while pos < data.len() {
        while pos < data.len() && data[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos == data.len() {
            break;
        }
        let key_start = pos;
        while pos < data.len() && !data[pos].is_ascii_whitespace() && data[pos] != b'=' {
            pos += 1;
        }
        let key_end = pos;
        while pos < data.len() && data[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos == data.len() || data[pos] != b'=' {
            // A lone word is not an attribute; resume at the next token.
            if pos == key_start {
                pos += 1;
            }
            diagnostics.warn(WarningKind::InvalidAttribute);
            continue;
        }
        pos += 1;
        while pos < data.len() && data[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let mut value = String::new();
        let mut oversized = false;
        let mut terminated = true;
        if pos < data.len() && data[pos] == b'"' {
            pos += 1;
            terminated = false;
            while pos < data.len() {
                if data[pos] == b'"' {
                    pos += 1;
                    terminated = true;
                    break;
                }
                let ch = if data[pos] == b'\\'
                    && pos + 1 < data.len()
                    && (data[pos + 1] == b'"' || data[pos + 1] == b'\\')
                {
                    pos += 1;
                    let ch = char::from(data[pos]);
                    pos += 1;
                    ch
                } else {
                    let ch = input[pos..].chars().next().expect("valid UTF-8");
                    pos += ch.len_utf8();
                    ch
                };
                if value.len() + ch.len_utf8() <= MAX_ATTRIBUTE_BYTES {
                    value.push(ch);
                } else {
                    oversized = true;
                }
            }
        } else {
            let start = pos;
            while pos < data.len() && !data[pos].is_ascii_whitespace() {
                pos += 1;
            }
            let span = &input[start..pos];
            if span.len() > MAX_ATTRIBUTE_BYTES {
                oversized = true;
            } else {
                value.push_str(span);
            }
        }
        if !terminated {
            diagnostics.warn(WarningKind::InvalidAttribute);
            break;
        }
        if oversized {
            diagnostics.warn(WarningKind::OversizedAttribute);
            continue;
        }
        let key = &input[key_start..key_end];
        if key.len() > 64 || !key.is_ascii() {
            diagnostics.warn(WarningKind::InvalidAttribute);
            continue;
        }
        if playlist_header {
            if key.eq_ignore_ascii_case("name") {
                metadata.title = bounded_text(&value, 128, diagnostics);
            }
        } else {
            assign_attribute(key, &value, metadata, diagnostics);
        }
    }
}

fn assign_attribute(
    key: &str,
    value: &str,
    metadata: &mut Metadata,
    diagnostics: &mut Diagnostics,
) {
    let key = key.to_ascii_lowercase();
    match key.as_str() {
        "tvg-id" => metadata.tvg_id = bounded_text(value, 512, diagnostics),
        "tvg-name" => metadata.tvg_name = bounded_text(value, 512, diagnostics),
        "tvg-logo" => metadata.logo_url = bounded_text(value, MAX_URL_BYTES, diagnostics),
        "group-title" => metadata.group_title = bounded_text(value, 256, diagnostics),
        "tvg-chno" => metadata.channel_number = bounded_text(value, 64, diagnostics),
        "tvg-region-code" | "region-code" => {
            metadata.region_code = bounded_text(value, 64, diagnostics);
        }
        "tvg-region" | "tvg-region-label" | "region-label" => {
            metadata.region_label = bounded_text(value, 128, diagnostics);
        }
        "tvg-language" => parse_facets(value, FacetDimension::Language, &mut metadata.languages, diagnostics),
        "tvg-country" => parse_facets(value, FacetDimension::Country, &mut metadata.countries, diagnostics),
        "user-agent" | "http-user-agent" => {
            metadata.user_agent = bounded_text(value, 1024, diagnostics);
        }
        "referer" | "referrer" | "http-referrer" | "http-referer" => {
            metadata.referer = bounded_text(value, MAX_URL_BYTES, diagnostics);
        }
        _ => {} // In particular, never interpret arbitrary EXTVLCOPT directives.
    }
}

fn parse_facets(
    raw: &str,
    dimension: FacetDimension,
    values: &mut BTreeSet<String>,
    diagnostics: &mut Diagnostics,
) {
    for token in raw.split(';') {
        match normalize_facet(token, dimension) {
            Ok(value) if values.len() < 64 || values.contains(&value) => {
                values.insert(value);
            }
            _ => diagnostics.warn(WarningKind::InvalidFacet),
        }
    }
}

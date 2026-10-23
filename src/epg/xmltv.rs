// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Portable, bounded XMLTV parsing. Input streams and trust decisions are
//! supplied by separate platform/network/file adapters. XML never performs I/O.
use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Cursor, Read},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use flate2::read::MultiGzDecoder;
use quick_xml::{events::{BytesStart, Event}, Reader};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use super::model::{bounded_text, EpgError, GuideChannel, GuideSnapshot, Programme};

const MAX_WIRE: usize = 16 * 1024 * 1024;
const MAX_EXPANDED: usize = 128 * 1024 * 1024;
const MAX_ELEMENT_TEXT: usize = 1024 * 1024;
const MAX_CHANNELS: usize = 10_000;
const MAX_PROGRAMMES: usize = 250_000;

#[derive(Clone, Copy, Debug)]
pub struct ParseLimits {
    pub max_wire_bytes: usize,
    pub max_expanded_bytes: usize,
    pub max_ratio: usize,
    pub max_depth: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_wire_bytes: MAX_WIRE,
            max_expanded_bytes: MAX_EXPANDED,
            max_ratio: 100,
            max_depth: 64,
        }
    }
}

/// The source adapter supplies only explicitly approved bytes or a bounded
/// stream, never XMLTV-provided URLs. A 16 MiB wire quota is enforced even if
/// the original source does not disclose Content-Length.
pub fn decode_xmltv<R: BufRead>(
    input: R, source_id: &str, retrieved_utc: i64, gzip: bool,
    limits: ParseLimits, cancelled: &AtomicBool,
) -> Result<GuideSnapshot, EpgError> {
    if !bounded_text(source_id, 64)
        || limits.max_wire_bytes == 0 || limits.max_wire_bytes > MAX_WIRE
        || limits.max_expanded_bytes == 0 || limits.max_expanded_bytes > MAX_EXPANDED
        || limits.max_ratio == 0 || limits.max_ratio > 100
        || limits.max_depth == 0 || limits.max_depth > 64
    {
        return Err(EpgError::InvalidInput);
    }
    let mut wire = Vec::new();
    let quota = (limits.max_wire_bytes as u64).saturating_add(1);
    input.take(quota).read_to_end(&mut wire)
        .map_err(|_| EpgError::InvalidInput)?;
    if wire.len() > limits.max_wire_bytes { return Err(EpgError::OverLimit); }
    if wire.is_empty() || cancelled.load(Ordering::Relaxed) {
        return Err(if wire.is_empty() { EpgError::InvalidInput } else { EpgError::Cancelled });
    }
    let decoder: Box<dyn Read> = if gzip {
        if !wire.starts_with(&[0x1f, 0x8b]) { return Err(EpgError::InvalidInput); }
        Box::new(MultiGzDecoder::new(Cursor::new(wire.clone())))
    } else {
        Box::new(Cursor::new(wire.clone()))
    };
    let expanded_cap = limits.max_expanded_bytes.min(
        wire.len().checked_mul(limits.max_ratio).ok_or(EpgError::OverLimit)?
    );
    let exceeded = Arc::new(AtomicBool::new(false));
    let limited = LimitedRead {
        inner: decoder,
        remaining: expanded_cap,
        exceeded: exceeded.clone(),
    };
    let result = parse_xml_stream(
        BufReader::new(limited), source_id, retrieved_utc, limits.max_depth,
        cancelled,
    );
    if exceeded.load(Ordering::Relaxed) { return Err(EpgError::OverLimit); }
    result
}

/// Detects over-cap data rather than synthesizing EOF at exactly the quota.
struct LimitedRead<R> {
    inner: R,
    remaining: usize,
    exceeded: Arc<AtomicBool>,
}
impl<R: Read> Read for LimitedRead<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() { return Ok(0); }
        if self.remaining == 0 {
            let mut probe = [0u8; 1];
            if self.inner.read(&mut probe)? == 0 { return Ok(0); }
            self.exceeded.store(true, Ordering::Relaxed);
            return Err(io::Error::new(io::ErrorKind::InvalidData, "EPG byte limit"));
        }
        let allowed = output.len().min(self.remaining);
        let n = self.inner.read(&mut output[..allowed])?;
        self.remaining -= n;
        Ok(n)
    }
}

#[derive(Default)]
struct PendingChannel {
    id: String,
    display_name: Option<String>,
}
#[derive(Default)]
struct PendingProgramme {
    channel: String,
    start_utc: Option<i64>,
    stop_utc: Option<i64>,
    title: Option<String>,
    subtitle: Option<String>,
    description: Option<String>,
}

#[derive(Default)]
struct ParseState {
    tags: Vec<String>,
    text_bytes: Vec<usize>,
    seen_root: bool,
    channels: Vec<GuideChannel>,
    programmes: Vec<Programme>,
    channel: Option<PendingChannel>,
    programme: Option<PendingProgramme>,
    programme_count: usize,
}

fn attributes(tag: &BytesStart<'_>) -> Result<BTreeMap<String, String>, EpgError> {
    let mut attributes = BTreeMap::new();
    for item in tag.attributes().with_checks(true) {
        let item = item.map_err(|_| EpgError::InvalidInput)?;
        if attributes.len() >= 32 { return Err(EpgError::OverLimit); }
        let key = std::str::from_utf8(item.key.as_ref()).map_err(|_| EpgError::InvalidInput)?;
        let value = item.unescape_value().map_err(|_| EpgError::InvalidInput)?;
        if key.starts_with("xmlns") && value.contains("www.w3.org/2001/XInclude") {
            return Err(EpgError::InvalidInput);
        }
        if key.len() > 512 || value.len() > MAX_ELEMENT_TEXT {
            return Err(EpgError::OverLimit);
        }
        attributes.insert(key.to_owned(), value.into_owned());
    }
    Ok(attributes)
}

impl ParseState {
    fn begin(&mut self, tag: &BytesStart<'_>, source_id: &str, max_depth: usize)
        -> Result<(), EpgError>
    {
        let name = std::str::from_utf8(tag.name().as_ref())
            .map_err(|_| EpgError::InvalidInput)?.to_owned();
        // Namespace/XInclude processing is deliberately not enabled. Do not
        // silently interpret or dereference namespaced "include" elements.
        if name.contains(':') { return Err(EpgError::InvalidInput); }
        let a = attributes(tag)?;
        if self.tags.is_empty() {
            if self.seen_root || name != "tv" { return Err(EpgError::InvalidInput); }
            self.seen_root = true;
        } else if self.tags.len() == 1 && self.tags[0] == "tv" {
            match name.as_str() {
                "channel" => {
                    if self.channels.len() >= MAX_CHANNELS { return Err(EpgError::OverLimit); }
                    let id = a.get("id").cloned().ok_or(EpgError::InvalidInput)?;
                    if !bounded_text(&id, 512) { return Err(EpgError::InvalidInput); }
                    self.channel = Some(PendingChannel { id: id.trim().into(), display_name: None });
                }
                "programme" => {
                    self.programme_count += 1;
                    if self.programme_count > MAX_PROGRAMMES { return Err(EpgError::OverLimit); }
                    self.programme = Some(PendingProgramme {
                        channel: a.get("channel").cloned().unwrap_or_default(),
                        start_utc: a.get("start").and_then(|s| parse_timestamp(s).ok()),
                        stop_utc: a.get("stop").and_then(|s| parse_timestamp(s).ok()),
                        ..Default::default()
                    });
                }
                _ => {}
            }
        } else if self.tags.len() == 2 && self.tags[0] == "tv" {
            if self.tags[1] == "channel" && name == "display-name" {
                if let Some(channel) = self.channel.as_mut() {
                    if channel.display_name.is_none() { channel.display_name = Some(String::new()); }
                }
            } else if self.tags[1] == "programme" {
                if let Some(p) = self.programme.as_mut() {
                    match name.as_str() {
                        "title" if p.title.is_none() => p.title = Some(String::new()),
                        "sub-title" if p.subtitle.is_none() => p.subtitle = Some(String::new()),
                        "desc" if p.description.is_none() => p.description = Some(String::new()),
                        _ => {},
                    }
                }
            }
        }
        self.tags.push(name);
        self.text_bytes.push(0);
        if self.tags.len() > max_depth { return Err(EpgError::OverLimit); }
        Ok(())
    }

    fn add_text(&mut self, text: &str) -> Result<(), EpgError> {
        if self.tags.is_empty() {
            if !text.trim().is_empty() { return Err(EpgError::InvalidInput); }
            return Ok(());
        }
        let used = self.text_bytes.last_mut().ok_or(EpgError::InvalidInput)?;
        *used = used.checked_add(text.len()).ok_or(EpgError::OverLimit)?;
        if *used > MAX_ELEMENT_TEXT { return Err(EpgError::OverLimit); }
        if self.tags.len() != 3 || self.tags[0] != "tv" { return Ok(()); }
        match (self.tags[1].as_str(), self.tags[2].as_str()) {
            ("channel", "display-name") => {
                if let Some(name) = self.channel.as_mut().and_then(|c| c.display_name.as_mut()) {
                    name.push_str(text);
                    if name.len() > 512 { return Err(EpgError::OverLimit); }
                }
            }
            ("programme", field) => {
                if let Some(p) = self.programme.as_mut() {
                    let (value, max): (Option<&mut String>, usize) = match field {
                        "title" => (p.title.as_mut(), 512),
                        "sub-title" => (p.subtitle.as_mut(), 512),
                        "desc" => (p.description.as_mut(), 2048),
                        _ => (None, 0),
                    };
                    if let Some(value) = value {
                        value.push_str(text);
                        if value.len() > max { return Err(EpgError::OverLimit); }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, tag: &str, source_id: &str) -> Result<(), EpgError> {
        let name = self.tags.pop().ok_or(EpgError::InvalidInput)?;
        self.text_bytes.pop();
        if name != tag { return Err(EpgError::InvalidInput); }
        if tag == "channel" && self.tags == ["tv"] {
            let c = self.channel.take().ok_or(EpgError::InvalidInput)?;
            let name = c.display_name.ok_or(EpgError::InvalidInput)?;
            if !bounded_text(&name, 512) { return Err(EpgError::InvalidInput); }
            let row = GuideChannel {
                source_id: source_id.to_owned(), channel_id: c.id, display_name: name.trim().into(),
            };
            if let Some(existing) = self.channels.iter().find(|a| a.channel_id == row.channel_id) {
                if existing != &row { return Err(EpgError::DuplicateChannel); }
            } else {
                self.channels.push(row);
            }
        }
        if tag == "programme" && self.tags == ["tv"] {
            let p = self.programme.take().ok_or(EpgError::InvalidInput)?;
            if let (Some(start), Some(stop), Some(title)) = (p.start_utc, p.stop_utc, p.title) {
                if start < stop && stop.saturating_sub(start) <= 86_400 && !p.channel.is_empty()
                    && bounded_text(&title, 512)
                {
                    self.programmes.push(Programme {
                        source_id: source_id.to_owned(),
                        channel_id: p.channel,
                        start_utc: start,
                        stop_utc: stop,
                        title: title.trim().into(),
                        subtitle: p.subtitle,
                        description: p.description,
                    });
                }
            }
        }
        Ok(())
    }
}

fn parse_xml_stream<R: BufRead>(
    input: R, source_id: &str, retrieved_utc: i64, max_depth: usize,
    cancel: &AtomicBool,
) -> Result<GuideSnapshot, EpgError> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().check_end_names = true;
    let mut b = Vec::new();
    let mut state = ParseState::default();
    loop {
        if cancel.load(Ordering::Relaxed) { return Err(EpgError::Cancelled); }
        let event = reader.read_event_into(&mut b).map_err(|_| EpgError::InvalidInput)?;
        match event {
            Event::Start(tag) => state.begin(&tag, source_id, max_depth)?,
            Event::Empty(tag) => {
                let name = std::str::from_utf8(tag.name().as_ref())
                    .map_err(|_| EpgError::InvalidInput)?.to_owned();
                state.begin(&tag, source_id, max_depth)?;
                state.end(&name, source_id)?;
            }
            Event::End(tag) => {
                let name = std::str::from_utf8(tag.name().as_ref())
                    .map_err(|_| EpgError::InvalidInput)?;
                state.end(name, source_id)?;
            }
            Event::Text(text) => {
                let decoded = text.decode().map_err(|_| EpgError::InvalidInput)?;
                state.add_text(&decoded)?;
            }
            Event::CData(text) => {
                let decoded = text.decode().map_err(|_| EpgError::InvalidInput)?;
                state.add_text(&decoded)?;
            }
            Event::GeneralRef(entity) => {
                let decoded = entity.decode().map_err(|_| EpgError::InvalidInput)?;
                let replacement = match decoded.as_ref() {
                    "amp" => '&', "lt" => '<', "gt" => '>',
                    "quot" => '"', "apos" => '\'',
                    _ => entity.resolve_char_ref()
                        .map_err(|_| EpgError::InvalidInput)?
                        .ok_or(EpgError::InvalidInput)?,
                };
                state.add_text(&replacement.to_string())?;
            }
            Event::Decl(decl) => {
                if state.seen_root { return Err(EpgError::InvalidInput); }
                if let Some(encoding) = decl.encoding() {
                    let encoding = encoding.map_err(|_| EpgError::InvalidInput)?;
                    if !encoding.eq_ignore_ascii_case(b"utf-8") {
                        return Err(EpgError::InvalidInput);
                    }
                }
            }
            Event::DocType(_) | Event::PI(_) => return Err(EpgError::InvalidInput),
            Event::Comment(_) => {},
            Event::Eof => break,
        }
        b.clear();
    }
    if !state.seen_root || !state.tags.is_empty() {
        return Err(EpgError::InvalidInput);
    }
    state.programmes.sort();
    state.programmes.dedup();
    if state.channels.is_empty() || state.programmes.is_empty()
        || !state.programmes.iter().any(|p| p.stop_utc > retrieved_utc)
    {
        return Err(EpgError::InvalidSchedule);
    }
    let known = state.channels.iter().map(|c| c.channel_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if state.programmes.iter().any(|p| !known.contains(p.channel_id.as_str())) {
        return Err(EpgError::MissingChannel);
    }
    Ok(GuideSnapshot {
        source_id: source_id.to_owned(),
        channels: state.channels,
        programmes: state.programmes,
        retrieved_utc,
    })
}

/// Full XMLTV programme schedule preserves numeric source offsets in UTC.
pub fn parse_timestamp(raw: &str) -> Result<i64, EpgError> {
    let raw = raw.trim();
    if raw.len() < 15 || !raw.is_ascii() { return Err(EpgError::InvalidInput); }
    let stamp = raw.get(..14).ok_or(EpgError::InvalidInput)?;
    if !stamp.bytes().all(|b| b.is_ascii_digit()) { return Err(EpgError::InvalidInput); }
    let number = |start, end| -> Result<u8, EpgError> {
        stamp[start..end].parse().map_err(|_| EpgError::InvalidInput)
    };
    let year: i32 = stamp[0..4].parse().map_err(|_| EpgError::InvalidInput)?;
    let month = Month::try_from(number(4, 6)?).map_err(|_| EpgError::InvalidInput)?;
    let date = Date::from_calendar_date(year, month, number(6, 8)?)
        .map_err(|_| EpgError::InvalidInput)?;
    let time = Time::from_hms(number(8, 10)?, number(10, 12)?, number(12, 14)?)
        .map_err(|_| EpgError::InvalidInput)?;
    let offset = raw[14..].trim();
    let utc_offset = if offset == "Z" { UtcOffset::UTC } else {
        let bytes = offset.as_bytes();
        if bytes.len() != 5 || !matches!(bytes[0], b'+' | b'-')
            || !bytes[1..].iter().all(|b| b.is_ascii_digit())
        {
            return Err(EpgError::InvalidInput);
        }
        let hour: i8 = offset[1..3].parse().map_err(|_| EpgError::InvalidInput)?;
        let minutes: i8 = offset[3..5].parse().map_err(|_| EpgError::InvalidInput)?;
        if hour > 14 || minutes > 59 || (hour == 14 && minutes != 0) {
            return Err(EpgError::InvalidInput);
        }
        let sign = if bytes[0] == b'+' { 1 } else { -1 };
        UtcOffset::from_hms(sign * hour, sign * minutes, 0)
            .map_err(|_| EpgError::InvalidInput)?
    };
    let epoch = PrimitiveDateTime::new(date, time)
        .assume_offset(utc_offset).unix_timestamp();
    let lower = Date::from_calendar_date(2000, Month::January, 1)
        .map_err(|_| EpgError::InvalidInput)?.midnight().assume_utc().unix_timestamp();
    let upper = Date::from_calendar_date(2100, Month::January, 1)
        .map_err(|_| EpgError::InvalidInput)?.midnight().assume_utc().unix_timestamp();
    if epoch < lower || epoch > upper { return Err(EpgError::InvalidInput); }
    Ok(epoch)
}

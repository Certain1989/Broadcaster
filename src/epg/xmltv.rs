// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Portable, bounded XMLTV parsing. Input streams and trust decisions are
//! supplied by separate platform/network/file adapters. XML never performs I/O.
use super::model::{bounded_text, EpgError, GuideChannel, GuideSnapshot, LimitKind, Programme};
use flate2::read::MultiGzDecoder;
use quick_xml::{
    events::{BytesStart, Event},
    Reader,
};
use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Cursor, Read},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

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
    input: R,
    source_id: &str,
    retrieved_utc: i64,
    gzip: bool,
    limits: ParseLimits,
    cancelled: &AtomicBool,
) -> Result<GuideSnapshot, EpgError> {
    if !bounded_text(source_id, 64)
        || limits.max_wire_bytes == 0
        || limits.max_wire_bytes > MAX_WIRE
        || limits.max_expanded_bytes == 0
        || limits.max_expanded_bytes > MAX_EXPANDED
        || limits.max_ratio == 0
        || limits.max_ratio > 100
        || limits.max_depth == 0
        || limits.max_depth > 64
    {
        return Err(EpgError::InvalidInput);
    }
    // Cancellation is observed between bounded reads, not during a blocking
    // adapter read; transport timeouts/abort are the source adapter's duty.
    let mut input = input;
    let mut wire = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(EpgError::Cancelled);
        }
        let remaining = limits.max_wire_bytes - wire.len();
        let requested = chunk.len().min(remaining + 1);
        let bytes = input
            .read(&mut chunk[..requested])
            .map_err(|_| EpgError::InvalidInput)?;
        if cancelled.load(Ordering::Relaxed) {
            return Err(EpgError::Cancelled);
        }
        if bytes == 0 {
            break;
        }
        if bytes > remaining {
            return Err(EpgError::OverLimit);
        }
        wire.extend_from_slice(&chunk[..bytes]);
    }
    if wire.is_empty() {
        return Err(EpgError::InvalidInput);
    }
    let decoder: Box<dyn Read + '_> = if gzip {
        if !wire.starts_with(&[0x1f, 0x8b]) {
            return Err(EpgError::InvalidInput);
        }
        Box::new(MultiGzDecoder::new(Cursor::new(wire.as_slice())))
    } else {
        Box::new(Cursor::new(wire.as_slice()))
    };
    let expanded_cap = limits.max_expanded_bytes.min(
        wire.len()
            .checked_mul(limits.max_ratio)
            .ok_or(EpgError::OverLimit)?,
    );
    let exceeded = Arc::new(AtomicBool::new(false));
    let limited = LimitedRead {
        inner: decoder,
        remaining: expanded_cap,
        exceeded: exceeded.clone(),
    };
    let result = parse_xml_stream(
        BufReader::new(limited),
        source_id,
        retrieved_utc,
        limits.max_depth,
        cancelled,
    );
    if exceeded.load(Ordering::Relaxed) {
        return Err(EpgError::OverLimit);
    }
    result.map_err(|error| match error {
        EpgError::Capacity(_) => EpgError::OverLimit,
        other => other,
    })
}

/// Detects over-cap data rather than synthesizing EOF at exactly the quota.
struct LimitedRead<R> {
    inner: R,
    remaining: usize,
    exceeded: Arc<AtomicBool>,
}
impl<R: Read> Read for LimitedRead<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut probe = [0u8; 1];
            if self.inner.read(&mut probe)? == 0 {
                return Ok(0);
            }
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
    description_truncated: bool,
}

#[derive(Default)]
struct ParseState {
    tags: Vec<String>,
    text_bytes: Vec<usize>,
    seen_root: bool,
    channels: Vec<GuideChannel>,
    channel_lookup: BTreeMap<String, String>,
    channel_declarations: usize,
    programmes: Vec<Programme>,
    channel: Option<PendingChannel>,
    programme: Option<PendingProgramme>,
    programme_count: usize,
    truncated_descriptions: usize,
    allow_description_truncation: bool,
    max_channels: usize,
    max_programmes: usize,
    capturing: Option<String>,
}

fn attributes(tag: &BytesStart<'_>) -> Result<BTreeMap<String, String>, EpgError> {
    let mut attributes = BTreeMap::new();
    for item in tag.attributes().with_checks(true) {
        let item = item.map_err(|_| EpgError::InvalidInput)?;
        if attributes.len() >= 32 {
            return Err(EpgError::Capacity(LimitKind::Attributes));
        }
        let key = std::str::from_utf8(item.key.as_ref()).map_err(|_| EpgError::InvalidInput)?;
        let value = item.unescape_value().map_err(|_| EpgError::InvalidInput)?;
        if key.starts_with("xmlns") && value.contains("www.w3.org/2001/XInclude") {
            return Err(EpgError::InvalidInput);
        }
        if key.len() > 512 || value.len() > MAX_ELEMENT_TEXT {
            return Err(EpgError::Capacity(LimitKind::Attributes));
        }
        attributes.insert(key.to_owned(), value.into_owned());
    }
    Ok(attributes)
}

impl ParseState {
    fn begin(&mut self, tag: &BytesStart<'_>, max_depth: usize) -> Result<(), EpgError> {
        let name = std::str::from_utf8(tag.name().as_ref())
            .map_err(|_| EpgError::InvalidInput)?
            .to_owned();
        // Namespace/XInclude processing is deliberately not enabled. Do not
        // silently interpret or dereference namespaced "include" elements.
        if name.contains(':') {
            return Err(EpgError::InvalidInput);
        }
        let a = attributes(tag)?;
        if self.tags.is_empty() {
            if self.seen_root || name != "tv" {
                return Err(EpgError::InvalidInput);
            }
            self.seen_root = true;
        } else if self.tags.len() == 1 && self.tags[0] == "tv" {
            match name.as_str() {
                "channel" => {
                    if self.channel_declarations >= self.max_channels {
                        return Err(EpgError::Capacity(LimitKind::Channels));
                    }
                    self.channel_declarations += 1;
                    let id = a.get("id").cloned().ok_or(EpgError::InvalidInput)?;
                    if !bounded_text(&id, 512) {
                        return Err(EpgError::InvalidInput);
                    }
                    self.channel = Some(PendingChannel {
                        id: id.trim().into(),
                        display_name: None,
                    });
                }
                "programme" => {
                    self.programme_count += 1;
                    if self.programme_count > self.max_programmes {
                        return Err(EpgError::Capacity(LimitKind::Programmes));
                    }
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
                    if channel.display_name.is_none() {
                        channel.display_name = Some(String::new());
                        self.capturing = Some(name.clone());
                    }
                }
            } else if self.tags[1] == "programme" {
                if let Some(p) = self.programme.as_mut() {
                    match name.as_str() {
                        "title" if p.title.is_none() => {
                            p.title = Some(String::new());
                            self.capturing = Some(name.clone());
                        }
                        "sub-title" if p.subtitle.is_none() => {
                            p.subtitle = Some(String::new());
                            self.capturing = Some(name.clone());
                        }
                        "desc" if p.description.is_none() => {
                            p.description = Some(String::new());
                            self.capturing = Some(name.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        self.tags.push(name);
        self.text_bytes.push(0);
        if self.tags.len() > max_depth {
            return Err(EpgError::Capacity(LimitKind::Depth));
        }
        Ok(())
    }

    fn add_text(&mut self, text: &str) -> Result<(), EpgError> {
        if text.chars().any(|c| {
            let u = c as u32;
            (u < 0x20 && c != '\n' && c != '\t' && c != '\r') || (0x7f..=0x9f).contains(&u)
        }) {
            return Err(EpgError::InvalidInput);
        }
        if self.tags.is_empty() {
            if !text.trim().is_empty() {
                return Err(EpgError::InvalidInput);
            }
            return Ok(());
        }
        // XMLTV permits formatting whitespace between its many records.
        // The root and the channel/programme containers can each contain
        // millions of individually small separators. Their *aggregate*
        // whitespace is not the text of one XML element. The expanded-byte
        // quota still bounds the total input, while leaf text stays limited.
        if self.tags.len() <= 2 && text.trim().is_empty() {
            return Ok(());
        }
        let used = self.text_bytes.last_mut().ok_or(EpgError::InvalidInput)?;
        *used = used
            .checked_add(text.len())
            .ok_or(EpgError::Capacity(LimitKind::XmlText))?;
        if *used > MAX_ELEMENT_TEXT {
            return Err(EpgError::Capacity(LimitKind::XmlText));
        }
        if self.tags.len() != 3
            || self.tags[0] != "tv"
            || self.capturing.as_deref() != self.tags.get(2).map(String::as_str)
        {
            return Ok(());
        }
        match (self.tags[1].as_str(), self.tags[2].as_str()) {
            ("channel", "display-name") => {
                if let Some(name) = self.channel.as_mut().and_then(|c| c.display_name.as_mut()) {
                    name.push_str(text);
                    if name.len() > 512 {
                        return Err(EpgError::Capacity(LimitKind::ChannelName));
                    }
                }
            }
            ("programme", field) => {
                if let Some(p) = self.programme.as_mut() {
                    if field == "desc" && self.allow_description_truncation {
                        if let Some(value) = p.description.as_mut() {
                            if !p.description_truncated {
                                let remaining = 2048_usize.saturating_sub(value.len());
                                if text.len() > remaining {
                                    // Preserve a complete UTF-8 prefix and never
                                    // append later CDATA/text/reference fragments.
                                    let mut end = remaining;
                                    while !text.is_char_boundary(end) {
                                        end -= 1;
                                    }
                                    value.push_str(&text[..end]);
                                    p.description_truncated = true;
                                } else {
                                    value.push_str(text);
                                }
                            }
                        }
                    } else {
                        let (value, max): (Option<&mut String>, usize) = match field {
                            "title" => (p.title.as_mut(), 512),
                            "sub-title" => (p.subtitle.as_mut(), 512),
                            "desc" => (p.description.as_mut(), 2048),
                            _ => (None, 0),
                        };
                        if let Some(value) = value {
                            value.push_str(text);
                            if value.len() > max {
                                let reason = match field {
                                    "title" => LimitKind::Title,
                                    "sub-title" => LimitKind::Subtitle,
                                    "desc" => LimitKind::Description,
                                    _ => LimitKind::XmlText,
                                };
                                return Err(EpgError::Capacity(reason));
                            }
                        }
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
        if name != tag {
            return Err(EpgError::InvalidInput);
        }
        if self.capturing.as_deref() == Some(tag) {
            self.capturing = None;
        }
        if tag == "channel" && self.tags == ["tv"] {
            let c = self.channel.take().ok_or(EpgError::InvalidInput)?;
            let name = c.display_name.ok_or(EpgError::InvalidInput)?;
            if !bounded_text(&name, 512) {
                return Err(EpgError::InvalidInput);
            }
            let row = GuideChannel {
                source_id: source_id.to_owned(),
                channel_id: c.id,
                display_name: name.trim().into(),
            };
            if let Some(existing_name) = self.channel_lookup.get(&row.channel_id) {
                if existing_name != &row.display_name {
                    return Err(EpgError::DuplicateChannel);
                }
            } else {
                self.channel_lookup
                    .insert(row.channel_id.clone(), row.display_name.clone());
                self.channels.push(row);
            }
        }
        if tag == "programme" && self.tags == ["tv"] {
            let p = self.programme.take().ok_or(EpgError::InvalidInput)?;
            if let (Some(start), Some(stop), Some(title)) = (p.start_utc, p.stop_utc, p.title) {
                if start < stop
                    && stop.saturating_sub(start) <= 86_400
                    && !p.channel.is_empty()
                    && bounded_text(&title, 512)
                {
                    if p.description_truncated {
                        self.truncated_descriptions += 1;
                    }
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
    input: R,
    source_id: &str,
    retrieved_utc: i64,
    max_depth: usize,
    cancel: &AtomicBool,
) -> Result<GuideSnapshot, EpgError> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().check_end_names = true;
    let mut b = Vec::new();
    let mut state = ParseState {
        max_channels: MAX_CHANNELS,
        max_programmes: MAX_PROGRAMMES,
        ..ParseState::default()
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(EpgError::Cancelled);
        }
        let event = reader
            .read_event_into(&mut b)
            .map_err(|_| EpgError::InvalidInput)?;
        match event {
            Event::Start(tag) => state.begin(&tag, max_depth)?,
            Event::Empty(tag) => {
                let name = std::str::from_utf8(tag.name().as_ref())
                    .map_err(|_| EpgError::InvalidInput)?
                    .to_owned();
                state.begin(&tag, max_depth)?;
                state.end(&name, source_id)?;
            }
            Event::End(tag) => {
                let tag_name = tag.name();
                let name =
                    std::str::from_utf8(tag_name.as_ref()).map_err(|_| EpgError::InvalidInput)?;
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
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => entity
                        .resolve_char_ref()
                        .map_err(|_| EpgError::InvalidInput)?
                        .ok_or(EpgError::InvalidInput)?,
                };
                state.add_text(&replacement.to_string())?;
            }
            Event::Decl(decl) => {
                if state.seen_root {
                    return Err(EpgError::InvalidInput);
                }
                if let Some(encoding) = decl.encoding() {
                    let encoding = encoding.map_err(|_| EpgError::InvalidInput)?;
                    if !encoding.eq_ignore_ascii_case(b"utf-8") {
                        return Err(EpgError::InvalidInput);
                    }
                }
            }
            Event::DocType(_) | Event::PI(_) => return Err(EpgError::InvalidInput),
            Event::Comment(_) => {}
            Event::Eof => break,
        }
        b.clear();
    }
    if !state.seen_root || !state.tags.is_empty() {
        return Err(EpgError::InvalidInput);
    }
    state.programmes.sort();
    state.programmes.dedup();
    if state.channels.is_empty()
        || state.programmes.is_empty()
        || !state.programmes.iter().any(|p| p.stop_utc > retrieved_utc)
    {
        return Err(EpgError::InvalidSchedule);
    }
    let known = state
        .channels
        .iter()
        .map(|c| c.channel_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if state
        .programmes
        .iter()
        .any(|p| !known.contains(p.channel_id.as_str()))
    {
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
    if raw.len() < 15 || !raw.is_ascii() {
        return Err(EpgError::InvalidInput);
    }
    let stamp = raw.get(..14).ok_or(EpgError::InvalidInput)?;
    if !stamp.bytes().all(|b| b.is_ascii_digit()) {
        return Err(EpgError::InvalidInput);
    }
    let number = |start, end| -> Result<u8, EpgError> {
        stamp[start..end]
            .parse()
            .map_err(|_| EpgError::InvalidInput)
    };
    let year: i32 = stamp[0..4].parse().map_err(|_| EpgError::InvalidInput)?;
    let month = Month::try_from(number(4, 6)?).map_err(|_| EpgError::InvalidInput)?;
    let date =
        Date::from_calendar_date(year, month, number(6, 8)?).map_err(|_| EpgError::InvalidInput)?;
    let time = Time::from_hms(number(8, 10)?, number(10, 12)?, number(12, 14)?)
        .map_err(|_| EpgError::InvalidInput)?;
    let offset = raw[14..].trim();
    let utc_offset = if offset == "Z" {
        UtcOffset::UTC
    } else {
        let bytes = offset.as_bytes();
        if bytes.len() != 5
            || !matches!(bytes[0], b'+' | b'-')
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
        UtcOffset::from_hms(sign * hour, sign * minutes, 0).map_err(|_| EpgError::InvalidInput)?
    };
    let epoch = PrimitiveDateTime::new(date, time)
        .assume_offset(utc_offset)
        .unix_timestamp();
    let lower = Date::from_calendar_date(2000, Month::January, 1)
        .map_err(|_| EpgError::InvalidInput)?
        .midnight()
        .assume_utc()
        .unix_timestamp();
    let upper = Date::from_calendar_date(2100, Month::January, 1)
        .map_err(|_| EpgError::InvalidInput)?
        .midnight()
        .assume_utc()
        .unix_timestamp();
    if epoch < lower || epoch > upper {
        return Err(EpgError::InvalidInput);
    }
    Ok(epoch)
}

/// Bounded row-streaming limits, with independent input and expanded XML caps.
#[derive(Clone, Copy, Debug)]
pub struct StreamingLimits {
    pub compressed_bytes: usize,
    pub expanded_bytes: usize,
    pub ratio: usize,
    pub channels: usize,
    pub programmes: usize,
}
impl StreamingLimits {
    pub fn candidate() -> Self {
        Self {
            compressed_bytes: 256 * 1024 * 1024,
            expanded_bytes: 2 * 1024 * 1024 * 1024,
            ratio: 200,
            channels: 100_000,
            programmes: 5_000_000,
        }
    }
}
/// Count completed validated rows (not a claim about unique SQLite PKs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamedRows {
    pub channels: usize,
    pub programmes: usize,
    /// Accepted programmes with a shortened synopsis.
    pub truncated_descriptions: usize,
}

struct MeasuredRead<R> {
    input: R,
    seen: std::rc::Rc<std::cell::Cell<usize>>,
    exceeded: std::rc::Rc<std::cell::Cell<Option<LimitKind>>>,
    cap_kind: LimitKind,
    cap: usize,
    ratio: Option<(std::rc::Rc<std::cell::Cell<usize>>, usize)>,
}
impl<R: Read> Read for MeasuredRead<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let used = self.seen.get();
        let available = self.cap.saturating_sub(used);
        if available == 0 {
            let mut probe = [0_u8; 1];
            if self.input.read(&mut probe)? == 0 {
                return Ok(0);
            }
            self.exceeded.set(Some(self.cap_kind));
            return Err(io::Error::new(io::ErrorKind::InvalidData, "XMLTV capacity"));
        }
        let requested = buffer.len().min(available);
        let n = self.input.read(&mut buffer[..requested])?;
        let total = used.saturating_add(n);
        self.seen.set(total);
        if let Some((wire, max_ratio)) = &self.ratio {
            // Check on every decoder read, not merely at EOF. Decoded bytes
            // cannot outrun inspected compressed bytes by an unbounded factor.
            if total > wire.get().saturating_mul(*max_ratio) {
                self.exceeded.set(Some(LimitKind::Ratio));
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "XMLTV expansion",
                ));
            }
        }
        Ok(n)
    }
}

/// Unlike the legacy snapshot API, this never constructs a whole-guide Vec.
/// The caller commits rows transactionally, and must verify referential
/// integrity before publication. An error must discard its transaction.
#[expect(
    clippy::too_many_arguments,
    reason = "parser limits, cancellation and two independent sinks form one bounded streaming operation"
)]
pub fn decode_xmltv_rows<R: BufRead>(
    input: R,
    source_id: &str,
    retrieved_utc: i64,
    gzip: bool,
    limits: StreamingLimits,
    cancelled: &AtomicBool,
    on_channel: impl FnMut(GuideChannel) -> Result<(), EpgError>,
    on_programme: impl FnMut(Programme) -> Result<(), EpgError>,
) -> Result<StreamedRows, EpgError> {
    use std::{cell::Cell, rc::Rc};
    if !bounded_text(source_id, 64)
        || limits.compressed_bytes == 0
        || limits.compressed_bytes > 256 * 1024 * 1024
        || limits.expanded_bytes == 0
        || limits.expanded_bytes > 2 * 1024 * 1024 * 1024
        || limits.ratio == 0
        || limits.ratio > 200
        || limits.channels == 0
        || limits.channels > 100_000
        || limits.programmes == 0
        || limits.programmes > 5_000_000
    {
        return Err(EpgError::InvalidInput);
    }
    let wire = Rc::new(Cell::new(0_usize));
    let expanded = Rc::new(Cell::new(0_usize));
    let over = Rc::new(Cell::new(None::<LimitKind>));
    let input = MeasuredRead {
        input,
        seen: wire.clone(),
        exceeded: over.clone(),
        // Plain XML is *not* subject to the compressed-size limit.
        cap: if gzip {
            limits.compressed_bytes
        } else {
            limits.expanded_bytes
        },
        ratio: None,
        cap_kind: if gzip {
            LimitKind::Wire
        } else {
            LimitKind::Expanded
        },
    };
    let decoded: Box<dyn Read + '_> = if gzip {
        Box::new(MultiGzDecoder::new(input))
    } else {
        Box::new(input)
    };
    let expanded_reader = MeasuredRead {
        input: decoded,
        seen: expanded.clone(),
        exceeded: over.clone(),
        cap: limits.expanded_bytes,
        cap_kind: LimitKind::Expanded,
        ratio: if gzip {
            Some((wire.clone(), limits.ratio))
        } else {
            None
        },
    };
    let result = parse_xml_stream_rows(
        BufReader::new(expanded_reader),
        source_id,
        retrieved_utc,
        limits.channels,
        limits.programmes,
        cancelled,
        on_channel,
        on_programme,
    );
    if let Some(reason) = over.get() {
        Err(EpgError::Capacity(reason))
    } else {
        result
    }
}

fn emit_stream_row<C, P>(
    state: &mut ParseState,
    tag: &str,
    retrieved_utc: i64,
    counts: &mut StreamedRows,
    future: &mut bool,
    on_channel: &mut C,
    on_programme: &mut P,
) -> Result<(), EpgError>
where
    C: FnMut(GuideChannel) -> Result<(), EpgError>,
    P: FnMut(Programme) -> Result<(), EpgError>,
{
    match tag {
        "channel" if state.tags == ["tv"] => {
            if let Some(row) = state.channels.pop() {
                counts.channels += 1;
                on_channel(row)?;
            }
        }
        "programme" if state.tags == ["tv"] => {
            if let Some(row) = state.programmes.pop() {
                counts.programmes += 1;
                *future |= row.stop_utc > retrieved_utc;
                on_programme(row)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// Keep the exact same quick-xml event, timestamp, text and DTD semantics as
// the snapshot parser. Only the validated-row destination differs.
#[expect(
    clippy::too_many_arguments,
    reason = "streaming XMLTV event parser carries validation context and two row sinks"
)]
fn parse_xml_stream_rows<R, C, P>(
    input: R,
    source_id: &str,
    retrieved_utc: i64,
    max_channels: usize,
    max_programmes: usize,
    cancel: &AtomicBool,
    mut on_channel: C,
    mut on_programme: P,
) -> Result<StreamedRows, EpgError>
where
    R: BufRead,
    C: FnMut(GuideChannel) -> Result<(), EpgError>,
    P: FnMut(Programme) -> Result<(), EpgError>,
{
    let mut reader = Reader::from_reader(input);
    reader.config_mut().check_end_names = true;
    let mut b = Vec::new();
    let mut state = ParseState {
        max_channels,
        max_programmes,
        allow_description_truncation: true,
        ..ParseState::default()
    };
    let mut rows = StreamedRows {
        channels: 0,
        programmes: 0,
        truncated_descriptions: 0,
    };
    let mut has_future = false;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(EpgError::Cancelled);
        }
        let event = reader
            .read_event_into(&mut b)
            .map_err(|_| EpgError::InvalidInput)?;
        match event {
            Event::Start(tag) => state.begin(&tag, 64)?,
            Event::Empty(tag) => {
                let name = std::str::from_utf8(tag.name().as_ref())
                    .map_err(|_| EpgError::InvalidInput)?
                    .to_owned();
                state.begin(&tag, 64)?;
                state.end(&name, source_id)?;
                emit_stream_row(
                    &mut state,
                    &name,
                    retrieved_utc,
                    &mut rows,
                    &mut has_future,
                    &mut on_channel,
                    &mut on_programme,
                )?;
            }
            Event::End(tag) => {
                let name = std::str::from_utf8(tag.name().as_ref())
                    .map_err(|_| EpgError::InvalidInput)?
                    .to_owned();
                state.end(&name, source_id)?;
                emit_stream_row(
                    &mut state,
                    &name,
                    retrieved_utc,
                    &mut rows,
                    &mut has_future,
                    &mut on_channel,
                    &mut on_programme,
                )?;
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
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => entity
                        .resolve_char_ref()
                        .map_err(|_| EpgError::InvalidInput)?
                        .ok_or(EpgError::InvalidInput)?,
                };
                state.add_text(&replacement.to_string())?;
            }
            Event::Decl(decl) => {
                if state.seen_root {
                    return Err(EpgError::InvalidInput);
                }
                if let Some(encoding) = decl.encoding() {
                    let encoding = encoding.map_err(|_| EpgError::InvalidInput)?;
                    if !encoding.eq_ignore_ascii_case(b"utf-8") {
                        return Err(EpgError::InvalidInput);
                    }
                }
            }
            Event::DocType(_) | Event::PI(_) => return Err(EpgError::InvalidInput),
            Event::Comment(_) => {}
            Event::Eof => break,
        }
        b.clear();
    }
    if !state.seen_root
        || !state.tags.is_empty()
        || rows.channels == 0
        || rows.programmes == 0
        || !has_future
    {
        return Err(EpgError::InvalidSchedule);
    }
    rows.truncated_descriptions = state.truncated_descriptions;
    Ok(rows)
}

#[cfg(test)]
mod formatted_xmltv_regression {
    use super::{decode_xmltv_rows, EpgError, LimitKind, StreamingLimits};
    use std::{fmt::Write as _, io::Cursor, sync::atomic::AtomicBool};

    /// Mirrors ordinary indented XMLTV with roughly 75,000 programmes.
    /// Previously the spaces between <programme> rows exhausted the 1 MiB
    /// text cap on the enclosing <tv> despite all fields being small.
    #[test]
    fn indented_germany_sized_guide_uses_per_field_not_root_text_budget() {
        const PROGRAMMES: usize = 75_000;
        let mut xml = String::with_capacity(18_000_000);
        xml.push_str(
            "<tv>\n                <channel id='de'><display-name>DE</display-name></channel>\n",
        );
        for i in 0..PROGRAMMES {
            writeln!(
                &mut xml,
                "                <programme channel='de' start='20300101000000 +0000' \
                 stop='20300101010000 +0000'><title>Show {i}</title></programme>"
            )
            .unwrap();
        }
        xml.push_str("</tv>");
        let cancelled = AtomicBool::new(false);
        let mut channels = 0_usize;
        let mut programmes = 0_usize;
        let stats = decode_xmltv_rows(
            Cursor::new(xml.as_bytes()),
            "synthetic",
            0,
            false,
            StreamingLimits::candidate(),
            &cancelled,
            |_| {
                channels += 1;
                Ok(())
            },
            |_| {
                programmes += 1;
                Ok(())
            },
        )
        .expect("normal XMLTV formatting whitespace must not trip element quota");
        assert_eq!(stats.channels, 1);
        assert_eq!(stats.programmes, PROGRAMMES);
        assert_eq!(stats.truncated_descriptions, 0);
        assert_eq!((channels, programmes), (1, PROGRAMMES));
    }

    #[test]
    fn oversized_description_retains_complete_schedule_and_tracks_warning() {
        let desc = format!("{}é", "x".repeat(2047));
        let xml = format!(
            "<tv><channel id='c'><display-name>Channel</display-name></channel>\
            <programme channel='c' start='20300101000000 +0000'\
            stop='20300101010000 +0000'><title>First</title>\
            <desc>{desc}<![CDATA[EXTRA]]>&amp;more</desc></programme>\
            <programme channel='c' start='20300101010000 +0000'\
            stop='20300101020000 +0000'><title>Second</title></programme></tv>"
        );
        let cancelled = AtomicBool::new(false);
        let mut descriptions = Vec::new();
        let result = decode_xmltv_rows(
            Cursor::new(xml.as_bytes()),
            "synthetic",
            0,
            false,
            StreamingLimits::candidate(),
            &cancelled,
            |_| Ok(()),
            |p| { descriptions.push(p.description); Ok(()) },
        ).unwrap();
        assert_eq!(result.programmes, 2);
        assert_eq!(result.truncated_descriptions, 1);
        assert_eq!(descriptions[0].as_deref(), Some("x".repeat(2047).as_str()));
        assert_eq!(descriptions[1], None);
    }

    #[test]
    fn exact_utf8_description_limit_keeps_all_bytes() {
        let xml = format!(
            "<tv><channel id='c'><display-name>Channel</display-name></channel>\
            <programme channel='c' start='20300101000000 +0000'\
            stop='20300101010000 +0000'><title>Show</title><desc>{}</desc></programme></tv>",
            "é".repeat(1024),
        );
        let cancelled = AtomicBool::new(false);
        let mut descriptions = Vec::new();
        let rows = decode_xmltv_rows(
            Cursor::new(xml.as_bytes()), "synthetic", 0, false,
            StreamingLimits::candidate(), &cancelled, |_| Ok(()),
            |p| { descriptions.push(p.description); Ok(()) },
        ).unwrap();
        assert_eq!(rows.truncated_descriptions, 0);
        assert_eq!(descriptions[0].as_ref().unwrap().len(), 2048);
    }

    #[test]
    fn huge_description_remains_rejected_by_aggregate_text_limit() {
        let xml = format!(
            "<tv><channel id='c'><display-name>Channel</display-name></channel>\
            <programme channel='c' start='20300101000000 +0000'\
            stop='20300101010000 +0000'><title>Show</title><desc>{}</desc></programme></tv>",
            "x".repeat(1_048_577),
        );
        let cancelled = AtomicBool::new(false);
        let error = decode_xmltv_rows(
            Cursor::new(xml.as_bytes()), "synthetic", 0, false,
            StreamingLimits::candidate(), &cancelled, |_| Ok(()), |_| Ok(()),
        ).unwrap_err();
        assert_eq!(error, EpgError::Capacity(LimitKind::XmlText));
    }

    #[test]
    fn oversized_actual_leaf_text_is_still_rejected() {
        let mut xml = String::from("<tv><channel id='de'><display-name>");
        xml.push_str(&"X".repeat(1_048_577));
        xml.push_str("</display-name></channel></tv>");
        let cancelled = AtomicBool::new(false);
        let outcome = decode_xmltv_rows(
            Cursor::new(xml.as_bytes()),
            "synthetic",
            0,
            false,
            StreamingLimits::candidate(),
            &cancelled,
            |_| Ok(()),
            |_| Ok(()),
        );
        assert_eq!(outcome.unwrap_err(), EpgError::Capacity(LimitKind::XmlText));
    }
}

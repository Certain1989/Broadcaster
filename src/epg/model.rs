// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeSet;

/// Equal bare IDs from Live and IPTV never imply equal channel identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LinearChannelRef {
    Live { provider_channel_id: String },
    Iptv { iptv_channel_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuideChannel {
    pub source_id: String,
    pub channel_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Programme {
    pub source_id: String,
    pub channel_id: String,
    pub start_utc: i64,
    pub stop_utc: i64,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GuideSnapshot {
    pub source_id: String,
    pub channels: Vec<GuideChannel>,
    pub programmes: Vec<Programme>,
    pub retrieved_utc: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certainty {
    Exact,
    Missing,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NowNext {
    pub current: Option<Programme>,
    pub next: Option<Programme>,
    pub certainty: Certainty,
    pub next_ambiguous: bool,
    pub progress_fraction: Option<f64>,
}

/// Static user-safe capacity reason; never put source or XML content here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Wire,
    Expanded,
    Ratio,
    Attributes,
    Depth,
    Channels,
    Programmes,
    XmlText,
    ChannelName,
    Title,
    Subtitle,
    Description,
    Database,
    Duration,
}
impl LimitKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Wire => "oversize_wire",
            Self::Expanded => "oversize_expanded",
            Self::Ratio => "oversize_ratio",
            Self::Attributes => "oversize_attributes",
            Self::Depth => "oversize_depth",
            Self::Channels => "oversize_channels",
            Self::Programmes => "oversize_programmes",
            Self::XmlText => "oversize_xml_text",
            Self::ChannelName => "oversize_channel_name",
            Self::Title => "oversize_title",
            Self::Subtitle => "oversize_subtitle",
            Self::Description => "oversize_description",
            Self::Database => "oversize_database",
            Self::Duration => "oversize_duration",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpgError {
    InvalidInput,
    OverLimit,
    Capacity(LimitKind),
    DuplicateChannel,
    MissingChannel,
    InvalidSchedule,
    Cancelled,
    Unavailable,
    Storage,
    DiskFull,
}

impl std::fmt::Display for EpgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Do not include private guide URLs, programme text, or SQL errors.
        f.write_str(match self {
            Self::InvalidInput => "Invalid EPG input",
            Self::OverLimit | Self::Capacity(_) => "EPG input exceeded a safety limit",
            Self::DuplicateChannel => "Inconsistent guide channel declaration",
            Self::MissingChannel => "Programme channel is not declared in its guide",
            Self::InvalidSchedule => "The guide has no valid programme schedule",
            Self::Cancelled => "EPG operation cancelled",
            Self::Unavailable => "EPG source is unavailable",
            Self::Storage => "EPG storage operation failed",
            Self::DiskFull => "Not enough disk space to store the guide",
        })
    }
}
impl std::error::Error for EpgError {}

pub fn bounded_text(s: &str, max: usize) -> bool {
    !s.trim().is_empty()
        && s.len() <= max
        && !s.chars().any(|c| {
            let u = c as u32;
            (u < 0x20 && c != '\n' && c != '\t' && c != '\r') || (0x7f..=0x9f).contains(&u)
        })
}

/// Snapshot validation is entirely independent of a guide's network/file origin.
/// Validated intervals are retained in their entirety; none are trimmed to now/next.
pub fn validate_snapshot(snapshot: &GuideSnapshot) -> Result<(), EpgError> {
    if snapshot.source_id.is_empty()
        || snapshot.source_id.len() > 64
        || snapshot.channels.is_empty()
        || snapshot.channels.len() > 10_000
        || snapshot.programmes.is_empty()
        || snapshot.programmes.len() > 250_000
    {
        return Err(EpgError::InvalidSchedule);
    }
    let mut channels = BTreeSet::new();
    for channel in &snapshot.channels {
        if channel.source_id != snapshot.source_id
            || !bounded_text(&channel.channel_id, 512)
            || !bounded_text(&channel.display_name, 512)
            || !channels.insert(channel.channel_id.as_str())
        {
            return Err(EpgError::DuplicateChannel);
        }
    }
    for programme in &snapshot.programmes {
        if programme.source_id != snapshot.source_id
            || !channels.contains(programme.channel_id.as_str())
        {
            return Err(EpgError::MissingChannel);
        }
        if !bounded_text(&programme.title, 512)
            || programme.subtitle.as_deref().is_some_and(|s| s.len() > 512)
            || programme
                .description
                .as_deref()
                .is_some_and(|s| s.len() > 2048)
            || programme.stop_utc <= programme.start_utc
            || programme
                .stop_utc
                .checked_sub(programme.start_utc)
                .is_none_or(|s| s > 86_400)
        {
            return Err(EpgError::InvalidSchedule);
        }
    }
    if !snapshot
        .programmes
        .iter()
        .any(|programme| programme.stop_utc > snapshot.retrieved_utc)
    {
        return Err(EpgError::InvalidSchedule);
    }
    Ok(())
}

/// Half-open intervals. A competing current record suppresses misleading progress.
/// Following entries remain available even when the current interval is unknown.
pub fn select_programmes(
    programmes: &[Programme],
    guide_channel_id: &str,
    now_utc: i64,
) -> NowNext {
    let mut current: Vec<&Programme> = programmes
        .iter()
        .filter(|p| {
            p.channel_id == guide_channel_id && p.start_utc <= now_utc && now_utc < p.stop_utc
        })
        .collect();
    current.sort_by(|a, b| {
        (a.start_utc, a.stop_utc, &a.title).cmp(&(b.start_utc, b.stop_utc, &b.title))
    });
    current.dedup();
    let certainty = match current.len() {
        0 => Certainty::Missing,
        1 => Certainty::Exact,
        _ => Certainty::Ambiguous,
    };
    let chosen = if certainty == Certainty::Exact {
        Some((*current[0]).clone())
    } else {
        None
    };
    let progress = chosen
        .as_ref()
        .map(|p| (now_utc - p.start_utc) as f64 / (p.stop_utc - p.start_utc) as f64);
    let mut future: Vec<&Programme> = programmes
        .iter()
        .filter(|p| p.channel_id == guide_channel_id && p.start_utc > now_utc)
        .collect();
    future.sort_by(|a, b| {
        (a.start_utc, a.stop_utc, &a.title).cmp(&(b.start_utc, b.stop_utc, &b.title))
    });
    future.dedup();
    let next_ambiguous = future.first().is_some_and(|first| {
        future
            .iter()
            .filter(|p| p.start_utc == first.start_utc)
            .count()
            > 1
    });
    NowNext {
        current: chosen,
        next: if next_ambiguous {
            None
        } else {
            future.first().map(|p| (**p).clone())
        },
        certainty,
        next_ambiguous,
        progress_fraction: progress,
    }
}

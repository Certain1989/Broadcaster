// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Index-backed, review-only M3U/XMLTV candidate discovery.
//! All IDs remain byte-for-byte intact. Neither construction nor lookup binds
//! an IPTV stream to a guide, reads a video URL, or touches the network.
use super::{
    matching::{normalized_name, IptvRow},
    model::GuideChannel,
};
use std::collections::{HashMap, HashSet};

/// Bump when the feed classifier changes: previews produced by an older
/// version must never be accepted as proof of current matching rules.
pub const RULE_PROFILE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedClass {
    Quality,
    Region,
    Language,
    TimeShift,
    Provider,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateReason {
    /// Only a user-confirmed exact original-ID alias can activate the link.
    Feed(FeedClass),
    /// Never included in the default batch; display name is not identity.
    NameOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub original_tvg_id: String,
    pub full_xmltv_id: String,
    pub guide_source_id: String,
    pub reason: CandidateReason,
    /// Default *review selection*, never an automatic binding.
    pub default_review_eligible: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Discovery {
    NoImportedId,
    AlreadyBound,
    ExactExists,
    None,
    Ambiguous { count: usize },
    Unique(Candidate),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchCandidate {
    pub candidate: Candidate,
    /// All currently selected-guide IPTV rows bearing the literal imported ID.
    pub affected_rows: usize,
    /// Rows with an existing explicit mapping, excluded from automatic effects.
    pub already_bound_rows: usize,
}

/// Only one single `@` separator is interpreted. Multiple separators or an
/// empty stem/suffix are deliberately treated as opaque unknown identities.
fn split_feed(id: &str) -> Option<(&str, &str)> {
    let (stem, suffix) = id.split_once('@')?;
    (!stem.is_empty() && !suffix.is_empty() && !suffix.contains('@')).then_some((stem, suffix))
}

fn feed_class(suffix: &str) -> FeedClass {
    let suffix = suffix.to_ascii_lowercase();
    match suffix.as_str() {
        "sd" | "hd" | "fhd" | "fullhd" | "uhd" | "4k" | "8k" | "720p" | "1080p" | "2160p"
        | "hevc" | "h264" | "h265" => FeedClass::Quality,
        "nord" | "sud" | "süd" | "north" | "south" | "east" | "west" | "ost" | "westen"
        | "regional" => FeedClass::Region,
        "de" | "deu" | "ger" | "german" | "deutsch" | "en" | "eng" | "english" | "es" | "esp"
        | "spanish" | "espanol" | "español" | "fr" | "fra" | "french" | "francais" | "français"
        | "it" | "ita" | "italian" | "pt" | "por" | "portuguese" => FeedClass::Language,
        "plus1" | "+1" | "plus2" | "+2" | "timeshift" | "shift1" | "shift2" => FeedClass::TimeShift,
        "sat" | "cable" | "dvb" | "terrestrial" | "iptv" | "ott" | "web" | "stream" => {
            FeedClass::Provider
        }
        _ => FeedClass::Unknown,
    }
}

/// Exact IDs and stem/name buckets are constructed once per selected catalogue
/// revision. Hash lookups are bounded by a bucket rather than by the guide size.
/// Callers own invalidation on guide, playlist and mapping revisions.
pub struct CandidateIndex<'a> {
    guide_source_id: &'a str,
    exact: HashSet<&'a str>,
    stems: HashMap<&'a str, Vec<&'a str>>,
    names: HashMap<String, Vec<&'a str>>,
}

impl<'a> CandidateIndex<'a> {
    pub fn build(guide_source_id: &'a str, channels: &'a [GuideChannel]) -> Self {
        let mut exact = HashSet::new();
        let mut stems: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut names: HashMap<String, Vec<&str>> = HashMap::new();
        for channel in channels.iter().filter(|c| c.source_id == guide_source_id) {
            let id = channel.channel_id.as_str();
            exact.insert(id);
            let stem = split_feed(id).map_or(id, |(base, _)| base);
            stems.entry(stem).or_default().push(id);
            let name = normalized_name(&channel.display_name);
            if !name.is_empty() {
                names.entry(name).or_default().push(id);
            }
        }
        Self {
            guide_source_id,
            exact,
            stems,
            names,
        }
    }

    /// `already_bound` means the caller has resolved manual override or a
    /// confirmed guide-scoped alias *before* candidate discovery. Unknown
    /// source availability must also be filtered by the caller.
    pub fn discover(&self, row: &IptvRow, already_bound: bool) -> Discovery {
        if already_bound {
            return Discovery::AlreadyBound;
        }
        let Some(original) = row.tvg_id.as_deref().filter(|s| !s.trim().is_empty()) else {
            return Discovery::NoImportedId;
        };
        if self.exact.contains(original) {
            return Discovery::ExactExists;
        }
        let stem = split_feed(original).map_or(original, |(base, _)| base);
        if let Some(bucket) = self.stems.get(stem) {
            if bucket.len() > 1 {
                // A duplicate declaration is ambiguous even if the IDs agree:
                // never turn a non-unique feed into a bulk default.
                return Discovery::Ambiguous {
                    count: bucket.len(),
                };
            }
            if let Some(&target) = bucket.first() {
                let source_class = split_feed(original).map(|(_, s)| feed_class(s));
                let target_class = split_feed(target).map(|(_, s)| feed_class(s));
                let class = [source_class, target_class]
                    .into_iter()
                    .flatten()
                    .find(|c| *c != FeedClass::Quality)
                    .unwrap_or(FeedClass::Quality);
                return Discovery::Unique(Candidate {
                    original_tvg_id: original.to_owned(),
                    full_xmltv_id: target.to_owned(),
                    guide_source_id: self.guide_source_id.to_owned(),
                    reason: CandidateReason::Feed(class),
                    default_review_eligible: class == FeedClass::Quality,
                });
            }
        }
        // A whole-guide name index is a review-only fallback. Blank imported
        // IDs never create future-effect aliases even if names are identical.
        self.discover_name_only(row)
    }

    /// Separate presentation-only lookup: blank original IDs may produce a
    /// single-stream suggestion, but never a binding or an alias candidate.
    pub fn discover_name_only(&self, row: &IptvRow) -> Discovery {
        let mut ids = HashSet::new();
        for name in [row.tvg_name.as_deref(), Some(row.effective_name.as_str())]
            .into_iter()
            .flatten()
        {
            let key = normalized_name(name);
            if let Some(matches) = self.names.get(&key) {
                ids.extend(matches.iter().copied());
            }
        }
        match ids.len() {
            0 => Discovery::None,
            1 => Discovery::Unique(Candidate {
                original_tvg_id: row.tvg_id.clone().unwrap_or_default(),
                full_xmltv_id: (*ids.iter().next().expect("one candidate")).to_owned(),
                guide_source_id: self.guide_source_id.to_owned(),
                reason: CandidateReason::NameOnly,
                default_review_eligible: false,
            }),
            count => Discovery::Ambiguous { count },
        }
    }

    /// A linear pass over existing selected-guide rows groups distinct literal
    /// IDs. It reports manual exclusions and vetoes incompatible candidates;
    /// only feed-based candidates enter bulk preview. Name-only evidence cannot
    /// authorize a global alias, even after an explicit row-specific review.
    pub fn preview_batch<'r>(
        &self,
        rows: &'r [IptvRow],
        already_bound_rows: &HashSet<&'r str>,
    ) -> Vec<BatchCandidate> {
        let mut effects: HashMap<&str, (usize, usize)> = HashMap::new();
        let mut groups: HashMap<&str, (Candidate, bool)> = HashMap::new();
        let mut invalid: HashSet<&str> = HashSet::new();
        for row in rows {
            let Some(original) = row.tvg_id.as_deref().filter(|s| !s.trim().is_empty()) else {
                continue;
            };
            let bound = already_bound_rows.contains(row.id.as_str());
            let effect = effects.entry(original).or_default();
            effect.0 += 1;
            if bound {
                effect.1 += 1;
                continue;
            }
            match self.discover(row, false) {
                Discovery::Unique(candidate)
                    if matches!(candidate.reason, CandidateReason::Feed(_)) =>
                {
                    groups
                        .entry(original)
                        .and_modify(|(first, conflicted)| {
                            *conflicted |= first.full_xmltv_id != candidate.full_xmltv_id
                                || first.reason != candidate.reason;
                        })
                        .or_insert((candidate, false));
                }
                Discovery::ExactExists | Discovery::AlreadyBound => {}
                _ => {
                    // A conflicting or name-only result earlier in the batch
                    // must veto a later unique result for this original ID.
                    invalid.insert(original);
                }
            }
        }
        let mut batch = groups
            .into_iter()
            .filter_map(|(original, (candidate, conflicted))| {
                let (affected_rows, already_bound_rows) = effects[original];
                (!conflicted && !invalid.contains(original)).then_some(BatchCandidate {
                    candidate,
                    affected_rows,
                    already_bound_rows,
                })
            })
            .collect::<Vec<_>>();
        batch.sort_by(|a, b| {
            a.candidate
                .original_tvg_id
                .cmp(&b.candidate.original_tvg_id)
        });
        batch
    }
}

/// A bounded, selected-guide snapshot for a human review screen. Unlike the
/// XMLTV importer, this is a UI work budget, not an EPG feed rejection limit.
/// Rebuild this snapshot before preparing a SQLite bulk confirmation token;
/// caller-owned cancellation also invalidates its GTK delivery.
const MAX_REVIEW_GUIDE_ROWS: usize = 100_000;
const MAX_REVIEW_IPTV_ROWS: usize = 50_000;
const MAX_REVIEW_SNAPSHOT_BYTES: usize = 48 * 1024 * 1024;
// Inspection is deliberately read-only and bounded separately from selectable
// feed proposals. The total count remains truthful if the display cap is hit.
const MAX_REVIEW_NAME_HINTS: usize = 8_192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewLoadError {
    Unavailable,
    Cancelled,
    TooLarge,
    Storage,
}

#[derive(Default, Clone, Debug)]
pub struct ReviewCounts {
    pub playlist_rows: usize,
    pub manual_or_alias: usize,
    pub exact_ids: usize,
    pub no_imported_id: usize,
    pub ambiguous: usize,
    pub unmatched: usize,
    pub name_only: usize,
    pub feed_review: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameOnlyHint {
    /// Stable per-stream identity, never a global original-ID alias key.
    pub row_id: String,
    pub playlist_source_id: String,
    pub original_tvg_id: Option<String>,
    pub imported_name: String,
    pub full_xmltv_id: String,
    pub guide_display_name: String,
}

#[derive(Debug)]
pub struct GuideReview {
    pub guide_source_id: String,
    pub last_success_at: i64,
    pub counts: ReviewCounts,
    pub proposals: Vec<BatchCandidate>,
    /// Informational only. Never passed to preview_batch or alias confirmation.
    pub name_only_hints: Vec<NameOnlyHint>,
    pub name_only_truncated: bool,
}

/// Read exclusively safe matching metadata. Never fetch stream URLs, request
/// headers, programme descriptions, guide origin locators or provider XML.
/// A single SQLite read transaction prevents mixed guide / playlist revisions.
/// Both source counts and candidate groups are computed on a blocking worker.
pub(crate) fn review_in_transaction(
    tx: &rusqlite::Connection,
    guide: &str,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<GuideReview, ReviewLoadError> {
    use rusqlite::{params, OptionalExtension};
    use std::sync::atomic::Ordering;

    if guide.is_empty() || guide.len() > 64 || cancelled.load(Ordering::Acquire) {
        return Err(ReviewLoadError::Cancelled);
    }
    let last_success_at: Option<i64> = tx
        .query_row(
            "SELECT last_success_at FROM epg_sources WHERE id=?1 AND enabled=1",
            [guide],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| ReviewLoadError::Storage)?
        .flatten();
    let last_success_at = last_success_at.ok_or(ReviewLoadError::Unavailable)?;
    let mut byte_budget = 0usize;
    let mut channels = Vec::new();
    {
        let mut stmt = tx
            .prepare("SELECT channel_id,display_name FROM epg_channels WHERE source_id=?1 ORDER BY channel_id")
            .map_err(|_| ReviewLoadError::Storage)?;
        let mut records = stmt.query([guide]).map_err(|_| ReviewLoadError::Storage)?;
        while let Some(record) = records.next().map_err(|_| ReviewLoadError::Storage)? {
            if cancelled.load(Ordering::Relaxed) {
                return Err(ReviewLoadError::Cancelled);
            }
            if channels.len() >= MAX_REVIEW_GUIDE_ROWS {
                return Err(ReviewLoadError::TooLarge);
            }
            let id: String = record.get(0).map_err(|_| ReviewLoadError::Storage)?;
            let display: String = record.get(1).map_err(|_| ReviewLoadError::Storage)?;
            byte_budget = byte_budget
                .saturating_add(id.len())
                .saturating_add(display.len());
            if byte_budget > MAX_REVIEW_SNAPSHOT_BYTES {
                return Err(ReviewLoadError::TooLarge);
            }
            channels.push(GuideChannel {
                source_id: guide.to_owned(),
                channel_id: id,
                display_name: display,
            });
        }
    }
    let index = CandidateIndex::build(guide, &channels);
    let guide_names: HashMap<&str, &str> = channels
        .iter()
        .map(|channel| (channel.channel_id.as_str(), channel.display_name.as_str()))
        .collect();
    let mut name_only_hints = Vec::new();
    let mut name_only_truncated = false;
    let mut rows = Vec::new();
    let mut already_bound_ids = HashSet::new();
    let mut counts = ReviewCounts::default();
    {
        let mut stmt = tx
            .prepare(
                "SELECT c.id,c.source_id,c.tvg_id,c.tvg_name,
                        COALESCE(co.display_name_override,c.imported_name),
                        (eo.iptv_channel_id IS NOT NULL OR a.iptv_tvg_id IS NOT NULL)
                 FROM iptv_channels c
                 JOIN iptv_sources s ON s.id=c.source_id
                 JOIN epg_settings settings ON settings.singleton=1
                 LEFT JOIN epg_playlist_links l ON l.playlist_source_id=c.source_id
                 LEFT JOIN epg_channel_overrides eo ON eo.iptv_channel_id=c.id
                 LEFT JOIN epg_global_id_aliases a ON a.guide_source_id=?1 AND a.iptv_tvg_id=c.tvg_id
                 LEFT JOIN iptv_channel_overrides co ON co.channel_id=c.id
                 WHERE c.is_present=1 AND s.enabled=1
                   AND COALESCE(eo.guide_source_id,l.guide_source_id,settings.shared_guide_source_id)=?1
                 ORDER BY c.source_id,c.id",
            )
            .map_err(|_| ReviewLoadError::Storage)?;
        let mut records = stmt
            .query(params![guide])
            .map_err(|_| ReviewLoadError::Storage)?;
        while let Some(record) = records.next().map_err(|_| ReviewLoadError::Storage)? {
            if cancelled.load(Ordering::Relaxed) {
                return Err(ReviewLoadError::Cancelled);
            }
            if rows.len() >= MAX_REVIEW_IPTV_ROWS {
                return Err(ReviewLoadError::TooLarge);
            }
            let row = IptvRow {
                id: record.get(0).map_err(|_| ReviewLoadError::Storage)?,
                playlist_source_id: record.get(1).map_err(|_| ReviewLoadError::Storage)?,
                tvg_id: record.get(2).map_err(|_| ReviewLoadError::Storage)?,
                tvg_name: record.get(3).map_err(|_| ReviewLoadError::Storage)?,
                effective_name: record.get(4).map_err(|_| ReviewLoadError::Storage)?,
            };
            let bound: bool = record.get(5).map_err(|_| ReviewLoadError::Storage)?;
            byte_budget = byte_budget
                .saturating_add(row.id.len())
                .saturating_add(row.playlist_source_id.len())
                .saturating_add(row.tvg_id.as_deref().map_or(0, str::len))
                .saturating_add(row.tvg_name.as_deref().map_or(0, str::len))
                .saturating_add(row.effective_name.len());
            if byte_budget > MAX_REVIEW_SNAPSHOT_BYTES {
                return Err(ReviewLoadError::TooLarge);
            }
            if bound {
                already_bound_ids.insert(row.id.clone());
            }
            // A blank original tvg-id can still offer a *per-row*, human-
            // confirmed exact-target shortcut based on a unique name clue.
            // It is never fed into bulk alias preview or future-ID linking.
            let name_hint = match index.discover(&row, bound) {
                Discovery::NoImportedId => {
                    counts.no_imported_id += 1;
                    match index.discover_name_only(&row) {
                        Discovery::Unique(candidate) => Some(candidate),
                        _ => None,
                    }
                }
                Discovery::AlreadyBound => {
                    counts.manual_or_alias += 1;
                    None
                }
                Discovery::ExactExists => {
                    counts.exact_ids += 1;
                    None
                }
                Discovery::Ambiguous { .. } => {
                    counts.ambiguous += 1;
                    None
                }
                Discovery::None => {
                    counts.unmatched += 1;
                    None
                }
                Discovery::Unique(candidate) => match candidate.reason {
                    CandidateReason::NameOnly => Some(candidate),
                    CandidateReason::Feed(_) => {
                        counts.feed_review += 1;
                        None
                    }
                },
            };
            if let Some(candidate) = name_hint {
                counts.name_only += 1;
                if name_only_hints.len() < MAX_REVIEW_NAME_HINTS {
                    name_only_hints.push(NameOnlyHint {
                        row_id: row.id.clone(),
                        playlist_source_id: row.playlist_source_id.clone(),
                        original_tvg_id: row.tvg_id.clone(),
                        imported_name: row.effective_name.clone(),
                        guide_display_name: guide_names
                            .get(candidate.full_xmltv_id.as_str())
                            .copied()
                            .unwrap_or("")
                            .to_owned(),
                        full_xmltv_id: candidate.full_xmltv_id,
                    });
                } else {
                    name_only_truncated = true;
                }
            }
            rows.push(row);
        }
    }
    let bound = already_bound_ids.iter().map(String::as_str).collect();
    let proposals = index.preview_batch(&rows, &bound);
    counts.playlist_rows = rows.len();
    Ok(GuideReview {
        guide_source_id: guide.to_owned(),
        last_success_at,
        counts,
        proposals,
        name_only_hints,
        name_only_truncated,
    })
}

/// A complete single-revision read, safe to call off GTK's main loop.
pub fn load_guide_review(
    conn: &mut rusqlite::Connection,
    guide: &str,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<GuideReview, ReviewLoadError> {
    let tx = conn.transaction().map_err(|_| ReviewLoadError::Storage)?;
    let review = review_in_transaction(&tx, guide, cancelled)?;
    tx.commit().map_err(|_| ReviewLoadError::Storage)?;
    Ok(review)
}

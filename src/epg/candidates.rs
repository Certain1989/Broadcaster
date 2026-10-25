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
                original_tvg_id: original.to_owned(),
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

// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Deterministic, source-scoped, UI-independent XMLTV matching.
//! Matching never changes a stored binding. A name match is ONLY a suggestion.
use super::model::GuideChannel;
use unicode_casefold::UnicodeCaseFold;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IptvRow {
    /// Stable per-playlist playable row ID; NOT a TVG ID or stream URL.
    pub id: String,
    pub playlist_source_id: String,
    pub tvg_id: Option<String>,
    pub tvg_name: Option<String>,
    pub effective_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualTarget {
    pub guide_source_id: String,
    pub guide_channel_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePriority {
    RowOverride,
    PlaylistException,
    SharedDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedSource {
    pub guide_source_id: String,
    pub priority: SourcePriority,
}

pub fn choose_source(
    override_target: Option<&ManualTarget>,
    playlist_exception: Option<&str>,
    shared_default: Option<&str>,
) -> Option<SelectedSource> {
    if let Some(target) = override_target {
        return Some(SelectedSource {
            guide_source_id: target.guide_source_id.clone(),
            priority: SourcePriority::RowOverride,
        });
    }
    if let Some(source) = playlist_exception {
        return Some(SelectedSource {
            guide_source_id: source.to_owned(),
            priority: SourcePriority::PlaylistException,
        });
    }
    shared_default.map(|source| SelectedSource {
        guide_source_id: source.to_owned(),
        priority: SourcePriority::SharedDefault,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideAvailability {
    /// Source exists but has been disabled by the owner.
    Disabled,
    /// Source exists but has no successful cached channel catalogue.
    NotFetched,
    Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMethod {
    RowOverride,
    ConfirmedGlobalAlias,
    ExactTvgId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchDecision {
    Unconfigured,
    Disabled {
        source_id: String,
    },
    Unavailable {
        source_id: String,
    },
    Unmatched {
        source_id: String,
    },
    Ambiguous {
        source_id: String,
    },
    /// Name hints were not evaluated because the catalogue exceeds the bounded scan.
    /// Never interpret this as an unmatched or unique suggestion.
    SuggestionsUnavailable {
        source_id: String,
    },
    MissingTarget {
        source_id: String,
        channel_id: String,
    },
    /// MUST be confirmed before creating either a per-row override or a global alias.
    Suggested {
        source_id: String,
        channel_id: String,
    },
    Matched {
        source_id: String,
        channel_id: String,
        method: MatchMethod,
    },
}

impl MatchDecision {
    pub fn confirmed_target(&self) -> Option<(&str, &str)> {
        match self {
            Self::Matched {
                source_id,
                channel_id,
                ..
            } => Some((source_id, channel_id)),
            _ => None,
        }
    }
}

/// Full non-Turkic Unicode default case folding, then Unicode whitespace collapse.
/// Deliberately no accent removal, region/HD removal, URL matching or fuzzy edit distance.
pub fn normalized_name(text: &str) -> String {
    text.split_whitespace()
        .map(|part| part.case_fold().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Alias is already filtered by its exact source+case-sensitive tvg-id in SQLite.
/// An absent explicitly bound target is unresolved: never fall through to another row.
pub fn match_in_source(
    row: &IptvRow,
    chosen: Option<&SelectedSource>,
    availability: GuideAvailability,
    override_target: Option<&ManualTarget>,
    confirmed_alias_target: Option<&str>,
    channels: &[GuideChannel],
) -> MatchDecision {
    let Some(source) = chosen else {
        return MatchDecision::Unconfigured;
    };
    let id = &source.guide_source_id;
    if availability == GuideAvailability::Disabled {
        return MatchDecision::Disabled {
            source_id: id.clone(),
        };
    }
    if availability == GuideAvailability::NotFetched {
        return MatchDecision::Unavailable {
            source_id: id.clone(),
        };
    }
    let contains = |target: &str| {
        channels
            .iter()
            .any(|c| c.source_id == *id && c.channel_id == target)
    };
    if let Some(target) = override_target {
        if target.guide_source_id != *id {
            return MatchDecision::Unavailable {
                source_id: id.clone(),
            };
        }
        return if contains(&target.guide_channel_id) {
            MatchDecision::Matched {
                source_id: id.clone(),
                channel_id: target.guide_channel_id.clone(),
                method: MatchMethod::RowOverride,
            }
        } else {
            MatchDecision::MissingTarget {
                source_id: id.clone(),
                channel_id: target.guide_channel_id.clone(),
            }
        };
    }
    if let Some(target) = confirmed_alias_target {
        return if contains(target) {
            MatchDecision::Matched {
                source_id: id.clone(),
                channel_id: target.to_owned(),
                method: MatchMethod::ConfirmedGlobalAlias,
            }
        } else {
            MatchDecision::MissingTarget {
                source_id: id.clone(),
                channel_id: target.to_owned(),
            }
        };
    }
    if let Some(tvg_id) = row.tvg_id.as_deref().filter(|s| !s.trim().is_empty()) {
        if contains(tvg_id) {
            return MatchDecision::Matched {
                source_id: id.clone(),
                channel_id: tvg_id.to_owned(),
                method: MatchMethod::ExactTvgId,
            };
        }
    }

    // One unique target across BOTH name hints. Multiple distinct candidates are ambiguous.
    let mut wanted = Vec::new();
    if let Some(tvg_name) = row.tvg_name.as_deref() {
        let normalized = normalized_name(tvg_name);
        if !normalized.is_empty() {
            wanted.push(normalized);
        }
    }
    let display_name = normalized_name(&row.effective_name);
    if !display_name.is_empty() && !wanted.contains(&display_name) {
        wanted.push(display_name);
    }
    // Fold each catalogue entry once, regardless of the number of name hints.
    let mut candidates = channels
        .iter()
        .filter(|c| c.source_id == *id && wanted.contains(&normalized_name(&c.display_name)))
        .map(|c| c.channel_id.as_str())
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.dedup();
    match candidates.as_slice() {
        [] => MatchDecision::Unmatched {
            source_id: id.clone(),
        },
        [only] => MatchDecision::Suggested {
            source_id: id.clone(),
            channel_id: (*only).to_owned(),
        },
        _ => MatchDecision::Ambiguous {
            source_id: id.clone(),
        },
    }
}

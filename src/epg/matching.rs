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
    /// Read-only, collision-free identity reconstruction; never a saved alias.
    AutomaticIdentity,
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

/// Only identity-preserving conversions produce an automatic programme target.
/// Names alone NEVER establish identity; the guide's exact ID and its own
/// display label must both corroborate region or station identity. Candidates
/// are evaluated against the complete selected guide, not another guide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoIdentityKind {
    Quality,
    Region,
    UsStation,
}

/// The candidate set is intentionally short and versioned by this code; adding
/// a region/provider heuristic requires a contract and negative-case tests.
pub fn automatic_identity_candidates(original: &str) -> Vec<(String, AutoIdentityKind)> {
    let Some((stem, suffix)) = original.split_once('@') else {
        return Vec::new();
    };
    if suffix.contains('@') || stem.is_empty() || suffix.is_empty() {
        return Vec::new();
    }
    if matches!(
        suffix.to_ascii_lowercase().as_str(),
        "sd" | "hd"
            | "fhd"
            | "fullhd"
            | "uhd"
            | "4k"
            | "8k"
            | "720p"
            | "1080p"
            | "2160p"
            | "hevc"
            | "h264"
            | "h265"
    ) {
        return vec![(stem.to_owned(), AutoIdentityKind::Quality)];
    }
    // A narrowly defined German regional identity: preserve the *entire*
    // named affiliate; never map an NDR region to generic NDR.de.
    if stem == "NDRFernsehen.de" {
        let variant = match suffix {
            "Niedersachsen" => Some("NDRFernsehenNiedersachsen.de"),
            "Hamburg" => Some("NDRHamburg.de"),
            "MecklenburgVorpommern" => Some("NDRMecklenburg-Vorpommern.de"),
            "SchleswigHolstein" => Some("NDRSchleswig-Holstein.de"),
            _ => None,
        };
        if let Some(target) = variant {
            return vec![(target.to_owned(), AutoIdentityKind::Region)];
        }
    }
    // A US network + specific K/W broadcast station (the network alone is not
    // enough). The TV/DT/CD/LD/LP *designation* is an explicit suffix; an
    // unspecified station such as ABC.us@KAAL remains a manual case.
    let Some(network) = stem.strip_suffix(".us") else {
        return Vec::new();
    };
    if !(2..=12).contains(&network.len()) || !network.bytes().all(|b| b.is_ascii_uppercase()) {
        return Vec::new();
    }
    for designation in ["TV", "DT", "CD", "LD", "LP"] {
        if let Some(call) = suffix.strip_suffix(designation) {
            if (3..=5).contains(&call.len())
                && (call.starts_with('K') || call.starts_with('W'))
                && call.bytes().all(|b| b.is_ascii_uppercase())
            {
                return vec![(format!("{network}{call}.us"), AutoIdentityKind::UsStation)];
            }
        }
    }
    Vec::new()
}

/// Conservative quality-only corroboration. An XMLTV identifier cannot prove
/// that a differently named broadcaster broadcasts the same schedule. Permit
/// only the complete broadcaster stem, optional matching country decoration,
/// and known presentation-only quality labels. Unknown naming is manual-only.
fn corroborates_quality_label(original: &str, display_name: &str) -> bool {
    let Some((full_id, _)) = original.split_once('@') else {
        return false;
    };
    let Some((broadcaster, country)) = full_id.rsplit_once('.') else {
        return false;
    };
    let expected: String = broadcaster
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    // Very short/generic IDs cannot establish station identity from a label.
    if expected.len() < 3 {
        return false;
    }
    // Inferred-quality evidence must not discard unknown qualifiers. Flags,
    // symbols, combining marks, format characters and non-ASCII letters are
    // meaningful metadata; ASCII controls are not identity evidence either.
    // Check before trimming, country-prefix handling or punctuation filtering.
    if display_name.chars().any(|c| !c.is_ascii() || c.is_ascii_control()) {
        return false;
    }
    let label = display_name.trim().to_ascii_lowercase();
    let country_prefix = format!("{} - ", country.to_ascii_lowercase());
    let label = label.strip_prefix(&country_prefix).unwrap_or(&label);
    let label_key: String = label
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    matches!(
        label_key.strip_prefix(&expected),
        Some(
            "" | "sd"
                | "hd"
                | "fhd"
                | "fullhd"
                | "uhd"
                | "4k"
                | "8k"
                | "720p"
                | "1080p"
                | "2160p"
                | "hevc"
                | "h264"
                | "h265"
        )
    )
}

/// A guide label is corroboration, not a replacement for the structural ID.
/// All checks use plain ASCII components found in exact candidate identities;
/// provider prefixes and case-insensitive full-name similarity alone cannot
/// produce a target.
pub fn corroborated_auto_identity(
    original: &str,
    kind: AutoIdentityKind,
    channel: &GuideChannel,
) -> bool {
    let lowered = channel.display_name.to_ascii_lowercase();
    match kind {
        AutoIdentityKind::Quality => corroborates_quality_label(original, &channel.display_name),
        AutoIdentityKind::Region => {
            let Some((_, region)) = original.split_once('@') else {
                return false;
            };
            let region = region.to_ascii_lowercase();
            let label: String = lowered
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect();
            label.contains("ndr") && label.contains(&region)
        }
        AutoIdentityKind::UsStation => {
            let Some((stem, suffix)) = original.split_once('@') else {
                return false;
            };
            let Some(network) = stem.strip_suffix(".us") else {
                return false;
            };
            let call = ["TV", "DT", "CD", "LD", "LP"]
                .iter()
                .find_map(|suffix_designator| suffix.strip_suffix(suffix_designator));
            let Some(call) = call else { return false };
            let words = lowered
                .split(|c: char| !c.is_ascii_alphanumeric())
                .collect::<Vec<_>>();
            let network_key = network.to_ascii_lowercase();
            let call_key = call.to_ascii_lowercase();
            words.contains(&network_key.as_str()) && words.contains(&call_key.as_str())
        }
    }
}

/// Full selected-guide scan only. A candidate without exactly one unique
/// corroborated guide identity is not a binding. Region and US identity can
/// never be inferred from a display name by itself.
pub fn automatic_identity_target(
    original: &str,
    selected_guide: &str,
    channels: &[GuideChannel],
) -> Option<String> {
    let candidates = automatic_identity_candidates(original);
    let mut results = Vec::new();
    for (id, kind) in candidates {
        let matches = channels.iter().filter(|channel| {
            channel.source_id == selected_guide
                && channel.channel_id == id
                && corroborated_auto_identity(original, kind, channel)
        });
        for _ in matches {
            results.push(id.clone());
        }
    }
    (results.len() == 1).then(|| results.remove(0))
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

    if let Some(original) = row.tvg_id.as_deref() {
        if let Some(target) = automatic_identity_target(original, id, channels) {
            return MatchDecision::Matched {
                source_id: id.clone(),
                channel_id: target,
                method: MatchMethod::AutomaticIdentity,
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

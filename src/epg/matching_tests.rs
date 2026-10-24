// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later
use super::matching::*;
use super::model::{GuideChannel, LinearChannelRef};

fn row(id: &str, playlist: &str, tvg_id: Option<&str>, name: &str) -> IptvRow {
    IptvRow {
        id: id.into(), playlist_source_id: playlist.into(),
        tvg_id: tvg_id.map(str::to_owned), tvg_name: None,
        effective_name: name.into(),
    }
}
fn ch(source: &str, id: &str, name: &str) -> GuideChannel {
    GuideChannel {
        source_id: source.into(), channel_id: id.into(), display_name: name.into(),
    }
}
fn decision(r: &IptvRow, override_target: Option<&ManualTarget>, alias: Option<&str>, channels: &[GuideChannel]) -> MatchDecision {
    let chosen = choose_source(override_target, None, Some("guide")).unwrap();
    match_in_source(r, Some(&chosen), GuideAvailability::Available, override_target, alias, channels)
}

#[test]
fn exact_id_is_case_sensitive_and_never_crosses_guides() {
    let catalog = [ch("guide", "FOO", "Different"), ch("other", "Foo", "Cross guide")];
    let matched = decision(&row("r1", "p1", Some("FOO"), "Anything"), None, None, &catalog);
    assert_eq!(matched, MatchDecision::Matched {
        source_id: "guide".into(), channel_id: "FOO".into(), method: MatchMethod::ExactTvgId
    });
    let no = decision(&row("r2", "p2", Some("foo"), "Anything"), None, None, &catalog);
    assert!(matches!(no, MatchDecision::Unmatched { .. }));
    assert_ne!(
        LinearChannelRef::Iptv { iptv_channel_id: "same".into() },
        LinearChannelRef::Live { provider_channel_id: "same".into() }
    );
}
#[test]
fn source_precedence_never_falls_back_from_disabled_selection() {
    let fixed = ManualTarget { guide_source_id: "manual".into(), guide_channel_id: "region".into() };
    assert_eq!(choose_source(Some(&fixed), Some("other"), Some("shared")).unwrap().guide_source_id, "manual");
    assert_eq!(choose_source(None, Some("other"), Some("shared")).unwrap().guide_source_id, "other");
    assert_eq!(choose_source(None, None, Some("shared")).unwrap().guide_source_id, "shared");
    assert!(choose_source(None, None, None).is_none());
    assert!(matches!(match_in_source(
        &row("r", "p", Some("id"), "Name"),
        choose_source(None, Some("other"), Some("shared")).as_ref(),
        GuideAvailability::Disabled, None, None,
        &[ch("shared", "id", "Name")]
    ), MatchDecision::Disabled { source_id } if source_id == "other"));
}
#[test]
fn alias_and_row_override_have_priority_and_missing_target_never_rebinds() {
    let channels = [ch("guide", "auto", "Original"), ch("guide", "regional", "Regional")];
    let r = row("r1", "playlist-A", Some("auto"), "Original");
    let regional = ManualTarget { guide_source_id: "guide".into(), guide_channel_id: "regional".into() };
    assert!(matches!(decision(&r, Some(&regional), Some("auto"), &channels),
        MatchDecision::Matched { method: MatchMethod::RowOverride, channel_id, .. } if channel_id == "regional"));
    assert!(matches!(decision(&r, None, Some("absent"), &channels),
        MatchDecision::MissingTarget { channel_id, .. } if channel_id == "absent"));
    assert!(matches!(decision(&r, Some(&ManualTarget {
        guide_source_id: "guide".into(), guide_channel_id: "absent".into()
    }), None, &channels), MatchDecision::MissingTarget { .. }));
    for playlist in ["playlist-A", "playlist-B"] {
        let r = row("separate-stream", playlist, Some("auto"), "Whatever");
        assert!(matches!(decision(&r, None, Some("regional"), &channels),
            MatchDecision::Matched { method: MatchMethod::ConfirmedGlobalAlias, channel_id, .. } if channel_id == "regional"));
    }
}
#[test]
fn name_comparison_is_full_unicode_folded_but_only_a_suggestion() {
    assert_eq!(normalized_name("  STRAẞE\u{2003}  NEWS "), "strasse news");
    assert_eq!(normalized_name(" Straße\tNEWS "), "strasse news");
    let r = row("r", "p", None, "  STRAẞE   NEWS ");
    let channels = [ch("guide", "de.1", "straße news")];
    assert!(matches!(decision(&r, None, None, &channels),
        MatchDecision::Suggested { channel_id, .. } if channel_id == "de.1"));
    let ambiguous = [ch("guide", "one", "STRAẞE NEWS"), ch("guide", "two", "straße news")];
    assert!(matches!(decision(&r, None, None, &ambiguous), MatchDecision::Ambiguous { .. }));
    let variants = [ch("guide", "hd", "Station HD"), ch("guide", "region", "Station Berlin")];
    assert!(matches!(decision(&row("r", "p", None, "Station"), None, None, &variants),
        MatchDecision::Unmatched { .. }));
}
#[test]
fn imported_tvg_name_and_effective_name_suggest_but_do_not_bind() {
    let mut r = row("r", "p", Some("WrongCase"), "Visible name");
    r.tvg_name = Some("  CANAL\u{a0}PLUS ".into());
    let catalog = [ch("guide", "id", "canal plus")];
    let d = decision(&r, None, None, &catalog);
    assert_eq!(d, MatchDecision::Suggested { source_id: "guide".into(), channel_id: "id".into() });
    assert!(d.confirmed_target().is_none());
}

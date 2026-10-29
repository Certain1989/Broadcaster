// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later
use super::matching::*;
use super::model::{GuideChannel, LinearChannelRef};

fn row(id: &str, playlist: &str, tvg_id: Option<&str>, name: &str) -> IptvRow {
    IptvRow {
        id: id.into(),
        playlist_source_id: playlist.into(),
        tvg_id: tvg_id.map(str::to_owned),
        tvg_name: None,
        effective_name: name.into(),
    }
}
fn ch(source: &str, id: &str, name: &str) -> GuideChannel {
    GuideChannel {
        source_id: source.into(),
        channel_id: id.into(),
        display_name: name.into(),
    }
}
fn decision(
    r: &IptvRow,
    override_target: Option<&ManualTarget>,
    alias: Option<&str>,
    channels: &[GuideChannel],
) -> MatchDecision {
    let chosen = choose_source(override_target, None, Some("guide")).unwrap();
    match_in_source(
        r,
        Some(&chosen),
        GuideAvailability::Available,
        override_target,
        alias,
        channels,
    )
}

#[test]
fn exact_id_is_case_sensitive_and_never_crosses_guides() {
    let catalog = [
        ch("guide", "FOO", "Different"),
        ch("other", "Foo", "Cross guide"),
    ];
    let matched = decision(
        &row("r1", "p1", Some("FOO"), "Anything"),
        None,
        None,
        &catalog,
    );
    assert_eq!(
        matched,
        MatchDecision::Matched {
            source_id: "guide".into(),
            channel_id: "FOO".into(),
            method: MatchMethod::ExactTvgId
        }
    );
    let no = decision(
        &row("r2", "p2", Some("foo"), "Anything"),
        None,
        None,
        &catalog,
    );
    assert!(matches!(no, MatchDecision::Unmatched { .. }));
    assert_ne!(
        LinearChannelRef::Iptv {
            iptv_channel_id: "same".into()
        },
        LinearChannelRef::Live {
            provider_channel_id: "same".into()
        }
    );
}
#[test]
fn source_precedence_never_falls_back_from_disabled_selection() {
    let fixed = ManualTarget {
        guide_source_id: "manual".into(),
        guide_channel_id: "region".into(),
    };
    assert_eq!(
        choose_source(Some(&fixed), Some("other"), Some("shared"))
            .unwrap()
            .guide_source_id,
        "manual"
    );
    assert_eq!(
        choose_source(None, Some("other"), Some("shared"))
            .unwrap()
            .guide_source_id,
        "other"
    );
    assert_eq!(
        choose_source(None, None, Some("shared"))
            .unwrap()
            .guide_source_id,
        "shared"
    );
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
    let channels = [
        ch("guide", "auto", "Original"),
        ch("guide", "regional", "Regional"),
    ];
    let r = row("r1", "playlist-A", Some("auto"), "Original");
    let regional = ManualTarget {
        guide_source_id: "guide".into(),
        guide_channel_id: "regional".into(),
    };
    assert!(
        matches!(decision(&r, Some(&regional), Some("auto"), &channels),
        MatchDecision::Matched { method: MatchMethod::RowOverride, channel_id, .. } if channel_id == "regional")
    );
    assert!(matches!(decision(&r, None, Some("absent"), &channels),
        MatchDecision::MissingTarget { channel_id, .. } if channel_id == "absent"));
    assert!(matches!(
        decision(
            &r,
            Some(&ManualTarget {
                guide_source_id: "guide".into(),
                guide_channel_id: "absent".into()
            }),
            None,
            &channels
        ),
        MatchDecision::MissingTarget { .. }
    ));
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
    let ambiguous = [
        ch("guide", "one", "STRAẞE NEWS"),
        ch("guide", "two", "straße news"),
    ];
    assert!(matches!(
        decision(&r, None, None, &ambiguous),
        MatchDecision::Ambiguous { .. }
    ));
    let variants = [
        ch("guide", "hd", "Station HD"),
        ch("guide", "region", "Station Berlin"),
    ];
    assert!(matches!(
        decision(&row("r", "p", None, "Station"), None, None, &variants),
        MatchDecision::Unmatched { .. }
    ));
}
#[test]
fn imported_tvg_name_and_effective_name_suggest_but_do_not_bind() {
    let mut r = row("r", "p", Some("WrongCase"), "Visible name");
    r.tvg_name = Some("  CANAL\u{a0}PLUS ".into());
    let catalog = [ch("guide", "id", "canal plus")];
    let d = decision(&r, None, None, &catalog);
    assert_eq!(
        d,
        MatchDecision::Suggested {
            source_id: "guide".into(),
            channel_id: "id".into()
        }
    );
    assert!(d.confirmed_target().is_none());
}

#[test]
fn full_fold_handles_greek_sigma_and_expansions_without_stripping_variants() {
    // Final sigma folds to ordinary sigma; dotted-I has a multi-codepoint
    // default fold; ligature expansion is full folding, not simple lowercasing.
    assert_eq!(normalized_name("ΟΣ"), normalized_name("ος"));
    assert_eq!(normalized_name("İ"), "i\u{307}");
    assert_eq!(normalized_name("ﬃ"), "ffi");
    assert_eq!(normalized_name("  STRAẞE  \u{2003} NEWS  "), "strasse news");
    assert_ne!(normalized_name("Channel HD"), normalized_name("Channel"));
    assert_ne!(
        normalized_name("Station Berlin"),
        normalized_name("Station")
    );
}

#[test]
fn d35_quality_region_and_us_station_auto_are_restricted_to_proven_identity() {
    let cases = [
        ("3sat.de@SD", "3sat.de", "DE - 3sat"),
        (
            "NDRFernsehen.de@Niedersachsen",
            "NDRFernsehenNiedersachsen.de",
            "DE - NDR Fernsehen Niedersachsen",
        ),
        (
            "ABC.us@KEROTV",
            "ABCKERO.us",
            "US - ABC KERO Bakersfield CA",
        ),
    ];
    for (original, expected, guide_name) in cases {
        let r = row(
            "r",
            "playlist",
            Some(original),
            "An irrelevant changed card name",
        );
        let channels = [
            ch("guide", expected, guide_name),
            ch("other", expected, guide_name),
        ];
        assert!(
            matches!(decision(&r, None, None, &channels),
            MatchDecision::Matched { method: MatchMethod::AutomaticIdentity, channel_id, .. }
            if channel_id == expected),
            "failed {original}"
        );
        assert!(automatic_identity_target(original, "other-guide", &channels).is_none());
    }
    // Literal same-ID target and explicit overrides always outrank derived IDs.
    let r = row("r", "p", Some("3sat.de@SD"), "3sat");
    assert!(matches!(
        decision(
            &r,
            None,
            None,
            &[
                ch("guide", "3sat.de@SD", "3sat feed"),
                ch("guide", "3sat.de", "3sat")
            ]
        ),
        MatchDecision::Matched {
            method: MatchMethod::ExactTvgId,
            ..
        }
    ));
    let manual = ManualTarget {
        guide_source_id: "guide".into(),
        guide_channel_id: "owner-region".into(),
    };
    assert!(matches!(
        decision(
            &r,
            Some(&manual),
            None,
            &[
                ch("guide", "3sat.de", "3sat"),
                ch("guide", "owner-region", "owner")
            ]
        ),
        MatchDecision::Matched {
            method: MatchMethod::RowOverride,
            ..
        }
    ));
}

#[test]
fn d35_no_false_automatic_links_in_critical_negative_matrix() {
    let negatives = [
        ("ARTE.de@SD", "ZDF.de", "DE - ZDF"),
        ("CBN.us@HD", "CNN.us", "US - CNN"),
        ("BBC.us@HD", "MBCAmerica.us", "MBC America"),
        ("ABC.us@KEROTV", "ABCKERO.pluto", "PLUTO - ABC KERO"),
        ("ABCNewsLive.us@1", "ABCNewsLive.us", "ABC News Live"),
        ("AMC.us@East", "AMC.us", "AMC West"),
        (
            "NDRFernsehen.de@Niedersachsen",
            "NDRHamburg.de",
            "DE - NDR Hamburg",
        ),
        ("ABC.us@KAAL", "ABCKAAL.us", "US - ABC KAAL"),
        ("BabyTVLatinAmerica.us@HD", "BabyTV.us", "BabyTV"),
    ];
    for (imported, guide_id, guide_name) in negatives {
        let catalog = [ch("guide", guide_id, guide_name)];
        let d = decision(
            &row("r", "p", Some(imported), "UNKNOWN"),
            None,
            None,
            &catalog,
        );
        // A suffix alone cannot make a provider/network ID change valid;
        // US station labels do need exact corroboration of their broadcaster.
        assert!(
            !matches!(
                d,
                MatchDecision::Matched {
                    method: MatchMethod::AutomaticIdentity,
                    ..
                }
            ),
            "incorrect auto: {imported} to {guide_id}"
        );
    }
    assert!(automatic_identity_target(
        "ABC.us@KEROTV",
        "guide",
        &[ch("guide", "ABCKERO.us", "US - CBS KERO Bakersfield CA")]
    )
    .is_none());
}

#[test]
fn m1_019_de_ctrl_01_valid_quality_name_and_selected_guide() {
    let imported = row("R12", "synthetic", Some("3sat.de@HD"), "3sat");
    let guide_channels = [
        ch("guide", "3sat.de", "DE - 3sat"),
        ch("other", "3sat.de", "ZDF"),
    ];
    assert!(matches!(
        decision(&imported, None, None, &guide_channels),
        MatchDecision::Matched {
            source_id,
            channel_id,
            method: MatchMethod::AutomaticIdentity,
        } if source_id == "guide" && channel_id == "3sat.de"
    ));
}

#[test]
fn m1_019_name_01_exact_name_only_remains_nonbinding_suggestion() {
    // Corrected NAME-01 oracle: the original guide lacks a matching name and
    // therefore cannot test a genuine name-only suggestion.
    let mut imported = row(
        "R15",
        "synthetic",
        Some("OTHER.de"),
        "Card label irrelevant",
    );
    imported.tvg_name = Some("Name-only without exact ID".into());
    let guide_channels = [ch("guide", "name-only.test", "Name-only without exact ID")];
    let result = decision(&imported, None, None, &guide_channels);
    assert!(matches!(&result,
        MatchDecision::Suggested { source_id, channel_id }
        if source_id == "guide" && channel_id == "name-only.test"
    ));
    assert!(result.confirmed_target().is_none());
}

#[test]
fn m1_019_regional_language_and_multiple_suffixes_never_auto_bind() {
    let cases = [
        (
            "NDRFernsehen.de@Niedersachsen",
            "NDRHamburg.de",
            "NDR Hamburg",
        ),
        ("ARTE.de@FR", "ARTE.de", "ARTE"),
        ("ARTE.de@HD@FR", "ARTE.de", "ARTE"),
        ("ABC.us@KEROTV@HD", "ABCKERO.us", "ABC KERO"),
        ("AMC.us@East", "AMC.us", "AMC"),
    ];
    for (original, guide_id, guide_label) in cases {
        assert!(
            !matches!(
                decision(
                    &row("R", "synthetic", Some(original), "unrelated"),
                    None,
                    None,
                    &[ch("guide", guide_id, guide_label)]
                ),
                MatchDecision::Matched {
                    method: MatchMethod::AutomaticIdentity,
                    ..
                }
            ),
            "unexpected automatic identity for {original} to {guide_id}"
        );
    }
}

#[test]
fn m1_019_de_neg_01_poisoned_quality_should_abstain_policy_gate() {
    // An uncorroborated quality-ID target must never attach its programme.
    let imported = row("R11", "synthetic", Some("3sat.de@HD"), "3sat");
    let contradictory = [ch("guide", "3sat.de", "ZDF - deliberately inconsistent")];
    assert!(
        !matches!(
            decision(&imported, None, None, &contradictory),
            MatchDecision::Matched {
                method: MatchMethod::AutomaticIdentity,
                ..
            }
        ),
        "unsafe automatic 3sat -> incorrectly labelled ZDF guide target"
    );
}


#[test]
fn r02_quality_label_corroboration_is_conservative_and_guide_scoped() {
    for label in ["3sat", "DE - 3sat", "3sat HD", "DE - 3sat FHD"] {
        assert!(matches!(
            decision(&row("r", "p", Some("3sat.de@HD"), "3sat"), None, None,
                &[ch("guide", "3sat.de", label)]),
            MatchDecision::Matched { method: MatchMethod::AutomaticIdentity, .. }
        ), "valid quality display label: {label}");
    }
    for label in ["ZDF - deliberately inconsistent", "DE - ZDF", "ZDF - 3sat",
                  "3sat International", "DE - 3sat West", "3sat (another network)"] {
        assert!(!matches!(
            decision(&row("r", "p", Some("3sat.de@HD"), "irrelevant"), None, None,
                &[ch("guide", "3sat.de", label)]),
            MatchDecision::Matched { method: MatchMethod::AutomaticIdentity, .. }
        ), "ambiguous or contradictory label: {label}");
    }
    // A literal exact ID does not need inferential label corroboration.
    assert!(matches!(
        decision(&row("r", "p", Some("3sat.de"), "3sat"), None, None,
            &[ch("guide", "3sat.de", "ZDF - contradictory feed")]),
        MatchDecision::Matched { method: MatchMethod::ExactTvgId, .. }
    ));
}

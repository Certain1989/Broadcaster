// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::candidates::{
    load_guide_review, CandidateIndex, CandidateReason, Discovery, FeedClass, ReviewLoadError,
    RULE_PROFILE_VERSION,
};
use super::{matching::IptvRow, model::GuideChannel};
use std::collections::HashSet;

fn guide(source: &str, id: &str, name: &str) -> GuideChannel {
    GuideChannel {
        source_id: source.into(),
        channel_id: id.into(),
        display_name: name.into(),
    }
}
fn row(id: &str, tvg: Option<&str>, name: &str) -> IptvRow {
    IptvRow {
        id: id.into(),
        playlist_source_id: "playlist".into(),
        tvg_id: tvg.map(str::to_owned),
        tvg_name: None,
        effective_name: name.into(),
    }
}
fn expect_unique(decision: Discovery, target: &str, reason: CandidateReason, default: bool) {
    let Discovery::Unique(candidate) = decision else {
        panic!("not unique: {decision:?}")
    };
    assert_eq!(candidate.full_xmltv_id, target);
    assert_eq!(candidate.reason, reason);
    assert_eq!(candidate.default_review_eligible, default);
}

#[test]
fn reviewed_feed_hints_preserve_distinct_regional_language_affiliate_and_time_shift() {
    assert_eq!(RULE_PROFILE_VERSION, 1);
    let channels = vec![
        guide("de", "BRFernsehen.de@Nord", "BR Fernsehen"),
        guide("de", "EWTN.us@Spanish", "EWTN"),
        guide("de", "ABC.us@KAAL", "ABC"),
        guide("de", "Timeshift.us@Plus1", "Timeshift"),
        guide("de", "Quality.de@FHD", "Quality"),
        guide("other", "Other.de@HD", "Other"),
    ];
    let ix = CandidateIndex::build("de", &channels);
    expect_unique(
        ix.discover(&row("1", Some("BRFernsehen.de"), "none"), false),
        "BRFernsehen.de@Nord",
        CandidateReason::Feed(FeedClass::Region),
        false,
    );
    expect_unique(
        ix.discover(&row("2", Some("EWTN.us@English"), "none"), false),
        "EWTN.us@Spanish",
        CandidateReason::Feed(FeedClass::Language),
        false,
    );
    expect_unique(
        ix.discover(&row("3", Some("ABC.us"), "none"), false),
        "ABC.us@KAAL",
        CandidateReason::Feed(FeedClass::Unknown),
        false,
    );
    expect_unique(
        ix.discover(&row("4", Some("Timeshift.us"), "none"), false),
        "Timeshift.us@Plus1",
        CandidateReason::Feed(FeedClass::TimeShift),
        false,
    );
    expect_unique(
        ix.discover(&row("5", Some("Quality.de@HD"), "none"), false),
        "Quality.de@FHD",
        CandidateReason::Feed(FeedClass::Quality),
        true,
    );
    assert_eq!(
        ix.discover(&row("6", Some("Quality.de@FHD"), "none"), false),
        Discovery::ExactExists
    );
    assert_eq!(
        ix.discover(&row("7", Some("Other.de"), "none"), false),
        Discovery::None
    );
    assert_eq!(
        ix.discover(&row("8", Some("Quality.de"), "none"), true),
        Discovery::AlreadyBound
    );
}

#[test]
fn collisions_never_default_and_name_hints_are_review_only() {
    let catalogue = vec![
        guide("g", "ABC.us@KAAL", "ABC Local"),
        guide("g", "ABC.us@KSTP", "ABC Local"),
        guide("g", "BR.de@Nord", "BR"),
        guide("g", "BR.de@Sud", "BR"),
    ];
    let ix = CandidateIndex::build("g", &catalogue);
    assert_eq!(
        ix.discover(&row("a", Some("ABC.us"), "ABC Local"), false),
        Discovery::Ambiguous { count: 2 }
    );
    assert_eq!(
        ix.discover(&row("b", Some("BR.de"), "BR"), false),
        Discovery::Ambiguous { count: 2 }
    );
    assert_eq!(
        ix.discover(&row("c", None, "ABC Local"), false),
        Discovery::NoImportedId
    );
    let catalogue = vec![guide("g", "Unique@HD", "The Unique")];
    let ix = CandidateIndex::build("g", &catalogue);
    expect_unique(
        ix.discover(&row("d", Some("different"), "The Unique"), false),
        "Unique@HD",
        CandidateReason::NameOnly,
        false,
    );
    assert!(ix
        .preview_batch(
            &[row("d", Some("different"), "The Unique")],
            &HashSet::new()
        )
        .is_empty());
}

#[test]
fn selected_guide_only_and_duplicate_declaration_veto() {
    let catalogue = vec![
        guide("a", "One@HD", "One"),
        guide("a", "One@HD", "One"),
        guide("b", "Two@HD", "Two"),
    ];
    let ix = CandidateIndex::build("a", &catalogue);
    assert_eq!(
        ix.discover(&row("1", Some("One"), "One"), false),
        Discovery::Ambiguous { count: 2 }
    );
    assert_eq!(
        ix.discover(&row("2", Some("Two"), "Two"), false),
        Discovery::None
    );
}

#[test]
fn batch_counts_exact_ids_across_playlists_and_manual_exclusion() {
    let catalogue = vec![guide("g", "Quality.de@HD", "Quality")];
    let ix = CandidateIndex::build("g", &catalogue);
    let mut rows = vec![
        row("1", Some("Quality.de"), "One"),
        row("2", Some("Quality.de"), "Two"),
        row("3", Some("Quality.de"), "Three"),
        row("4", None, "Quality"),
    ];
    rows[1].playlist_source_id = "playlist-2".into();
    let mapped = HashSet::from(["3"]);
    let batch = ix.preview_batch(&rows, &mapped);
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].affected_rows, 3);
    assert_eq!(batch[0].already_bound_rows, 1);
    assert!(batch[0].candidate.default_review_eligible);
}

#[test]
fn indexed_15k_catalogue_10k_playlist_reuses_full_guide_without_per_row_scan() {
    let channels: Vec<_> = (0..15_000)
        .map(|n| guide("guide", &format!("Channel{n}.us@HD"), "Station"))
        .collect();
    let start = std::time::Instant::now();
    let ix = CandidateIndex::build("guide", &channels);
    let rows: Vec<_> = (0..10_000)
        .map(|n| {
            row(
                &format!("row-{n}"),
                Some(&format!("Channel{n}.us")),
                "Unknown",
            )
        })
        .collect();
    let batch = ix.preview_batch(&rows, &HashSet::new());
    assert_eq!(batch.len(), 10_000);
    assert!(batch.iter().all(|s| s.candidate.default_review_eligible));
    eprintln!(
        "M1 indexed 15000x10000 synthetic candidate build+preview: {:?}",
        start.elapsed()
    );
}

#[test]
fn indexed_20k_repeated_iptv_rows_are_grouped_once_per_original_id() {
    let channels: Vec<_> = (0..15_000)
        .map(|n| guide("guide", &format!("Channel{n}.us@FHD"), "Station"))
        .collect();
    let ix = CandidateIndex::build("guide", &channels);
    let rows: Vec<_> = (0..20_000)
        .map(|n| {
            row(
                &format!("row-{n}"),
                Some(&format!("Channel{}.us", n % 15_000)),
                "Unknown",
            )
        })
        .collect();
    let start = std::time::Instant::now();
    let batch = ix.preview_batch(&rows, &HashSet::new());
    assert_eq!(batch.len(), 15_000);
    assert_eq!(batch.iter().map(|c| c.affected_rows).sum::<usize>(), 20_000);
    eprintln!(
        "M1 indexed 15000-channel x 20000-row batch lookup: {:?}",
        start.elapsed()
    );
}

#[test]
fn bounded_selected_guide_review_preserves_case_and_excludes_explicit_links() {
    use crate::streams::storage::{MIGRATION_001, MIGRATION_002};
    use rusqlite::{params, Connection};
    use std::sync::atomic::AtomicBool;
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    conn.execute_batch(MIGRATION_001).unwrap();
    conn.execute_batch(MIGRATION_002).unwrap();
    conn.execute(
        "INSERT INTO iptv_sources(id,name,origin_kind,origin_locator,created_at,updated_at)
         VALUES ('test','Demo','local_file','/fake',1,1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO epg_sources(id,name,origin_kind,origin_locator,enabled,created_at,updated_at,last_success_at)
         VALUES ('selected','Guide','local_file','/fake',1,1,1,99)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO epg_channels(source_id,channel_id,display_name) VALUES ('selected','ZDF','ZDF')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO epg_channels(source_id,channel_id,display_name) VALUES ('selected','Unique.xml@HD','Distinct Station')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE epg_settings SET shared_guide_source_id='selected' WHERE singleton=1",
        [],
    )
    .unwrap();
    for (id, tvg) in [
        ("one", "ZDF@HD"),
        ("two", "ZDF@HD"),
        ("three", "zdf@HD"),
        ("four", "ZDF"),
        ("five", ""),
        ("hint", "Case-Sensitive.Original"),
    ] {
        conn.execute(
            "INSERT INTO iptv_channels(id,source_id,playlist_order,imported_name,stream_url,tvg_id,created_at,updated_at)
             VALUES (?1,'test',0,'ZDF','https://invalid.example/never-opened',?2,1,1)",
            params![id, tvg],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE iptv_channels SET imported_name='Distinct Station' WHERE id='hint'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO epg_channel_overrides(iptv_channel_id,guide_source_id,guide_channel_id,updated_at)
         VALUES ('two','selected','ZDF',1)",
        [],
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let review = load_guide_review(&mut conn, "selected", &cancel).unwrap();
    assert_eq!(review.guide_source_id, "selected");
    assert_eq!(review.last_success_at, 99);
    assert_eq!(review.counts.playlist_rows, 6);
    // The pre-existing case-distinct zdf@HD row is also a name-only hint.
    assert_eq!(review.counts.name_only, 2);
    assert_eq!(review.name_only_hints.len(), 2);
    assert!(!review.name_only_truncated);
    let hint = review
        .name_only_hints
        .iter()
        .find(|candidate| candidate.original_tvg_id == "Case-Sensitive.Original")
        .expect("unlinked name hint must be separately inspectable");
    assert_eq!(hint.imported_name, "Distinct Station");
    assert_eq!(hint.full_xmltv_id, "Unique.xml@HD");
    assert_eq!(hint.guide_display_name, "Distinct Station");
    assert_eq!(review.counts.manual_or_alias, 1);
    assert_eq!(review.counts.exact_ids, 1);
    assert_eq!(review.counts.no_imported_id, 1);
    assert_eq!(review.proposals.len(), 1);
    assert_eq!(review.proposals[0].candidate.original_tvg_id, "ZDF@HD");
    assert_eq!(review.proposals[0].candidate.full_xmltv_id, "ZDF");
    assert_eq!(review.proposals[0].affected_rows, 2);
    assert_eq!(review.proposals[0].already_bound_rows, 1);
    assert!(review.proposals[0].candidate.default_review_eligible);

    cancel.store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        load_guide_review(&mut conn, "selected", &cancel),
        Err(ReviewLoadError::Cancelled)
    ));
    cancel.store(false, std::sync::atomic::Ordering::Release);
    conn.execute("UPDATE epg_sources SET enabled=0 WHERE id='selected'", [])
        .unwrap();
    assert!(matches!(
        load_guide_review(&mut conn, "selected", &cancel),
        Err(ReviewLoadError::Unavailable)
    ));
    conn.execute("UPDATE epg_sources SET enabled=1 WHERE id='selected'", [])
        .unwrap();
    conn.execute(
        "DELETE FROM epg_channels WHERE source_id='selected' AND channel_id='Unique.xml@HD'",
        [],
    )
    .unwrap();
    let after_deletion = load_guide_review(&mut conn, "selected", &cancel).unwrap();
    assert_eq!(after_deletion.counts.name_only, 1);
    assert!(after_deletion
        .name_only_hints
        .iter()
        .all(|candidate| candidate.original_tvg_id != "Case-Sensitive.Original"));
}

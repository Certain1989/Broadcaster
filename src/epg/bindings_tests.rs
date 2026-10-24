// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later
use super::{
    bindings::*,
    cache::{Coverage, Freshness},
    matching::{MatchDecision, MatchMethod},
    model::{Certainty, EpgError},
};
use crate::streams::storage::{MIGRATION_001, MIGRATION_002};
use rusqlite::{params, Connection};
use std::sync::atomic::AtomicBool;

fn setup() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    c.execute_batch(MIGRATION_001).unwrap();
    c.execute_batch(MIGRATION_002).unwrap();
    for playlist in ["playlist-a", "playlist-b"] {
        c.execute(
            "INSERT INTO iptv_sources (id,name,origin_kind,origin_locator,created_at,updated_at)
             VALUES (?1,?1,'local_file','/fabricated/source',1,1)",
            [playlist],
        )
        .unwrap();
    }
    for (id, playlist, order, tvg, name, url) in [
        (
            "a1",
            "playlist-a",
            0,
            Some("TVG.EXACT"),
            "Station",
            "https://invalid.example/one.m3u8",
        ),
        (
            "b1",
            "playlist-b",
            0,
            Some("TVG.EXACT"),
            "Station",
            "https://invalid.example/two.m3u8",
        ),
        (
            "a2",
            "playlist-a",
            1,
            Some("tvg.exact"),
            "Station HD",
            "https://invalid.example/one.m3u8",
        ),
        (
            "b2",
            "playlist-b",
            1,
            None,
            "Unique Non-ID",
            "https://invalid.example/one.m3u8",
        ),
    ] {
        c.execute(
            "INSERT INTO iptv_channels
             (id,source_id,playlist_order,imported_name,stream_url,tvg_id,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,1,1)",
            params![id, playlist, order, name, url, tvg],
        )
        .unwrap();
    }
    add_guide(&c, "g", true);
    c
}
fn add_guide(c: &Connection, id: &str, enabled: bool) {
    c.execute(
        "INSERT INTO epg_sources
         (id,name,origin_kind,origin_locator,enabled,created_at,updated_at,last_success_at)
         VALUES (?1,?1,'local_file','/fabricated/guide',?2,1,1,1000)",
        params![id, enabled as i64],
    )
    .unwrap();
}
fn add_channel(c: &Connection, source: &str, target: &str, name: &str) {
    c.execute(
        "INSERT INTO epg_channels (source_id,channel_id,display_name) VALUES (?1,?2,?3)",
        params![source, target, name],
    )
    .unwrap();
}
fn selected(c: &mut Connection, source: &str) {
    set_shared_guide(
        c,
        ConfirmedGuideSelection::after_user_confirmation(),
        Some(source),
    )
    .unwrap();
}
fn resolve(c: &Connection, row: &str) -> MatchDecision {
    resolve_row(c, row, &AtomicBool::new(false)).unwrap()
}
fn count(c: &Connection, table: &str) -> i64 {
    let allowed = [
        "epg_sources",
        "epg_channels",
        "epg_global_id_aliases",
        "epg_channel_overrides",
        "epg_playlist_links",
        "iptv_channels",
    ];
    assert!(allowed.contains(&table));
    c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn fk_ok(c: &Connection) {
    let mut s = c.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(!s.query([]).unwrap().next().unwrap().is_some());
}

#[test]
fn one_shared_guide_two_playlists_alias_confirmation_and_override() {
    let mut c = setup();
    add_channel(&c, "g", "actual", "Station");
    add_channel(&c, "g", "region", "Station Regional");
    assert!(matches!(resolve(&c, "a1"), MatchDecision::Unconfigured));
    selected(&mut c, "g");
    assert!(matches!(resolve(&c, "a1"), MatchDecision::Suggested { .. }));
    assert!(matches!(resolve(&c, "b1"), MatchDecision::Suggested { .. }));
    assert_eq!(
        set_global_alias(
            &mut c,
            ConfirmedGlobalMapping::after_user_confirmation_for_all_matching_streams(),
            "b2",
            "g",
            "actual",
            2
        ),
        Err(BindingError::InvalidInput)
    );
    set_global_alias(
        &mut c,
        ConfirmedGlobalMapping::after_user_confirmation_for_all_matching_streams(),
        "a1",
        "g",
        "actual",
        2,
    )
    .unwrap();
    for row in ["a1", "b1"] {
        assert!(matches!(resolve(&c, row),
            MatchDecision::Matched { method: MatchMethod::ConfirmedGlobalAlias, channel_id, .. }
            if channel_id == "actual"));
    }
    assert_eq!(count(&c, "epg_global_id_aliases"), 1);
    // An otherwise identical URL does not make another playable IPTV row the same identity.
    assert_eq!(count(&c, "iptv_channels"), 4);
    assert!(matches!(resolve(&c, "a2"), MatchDecision::Unmatched { .. }));

    set_row_mapping(
        &mut c,
        ConfirmedRowMapping::after_user_confirmation_for_this_stream(),
        "a1",
        "g",
        "region",
        3,
    )
    .unwrap();
    assert!(matches!(resolve(&c, "a1"),
        MatchDecision::Matched { method: MatchMethod::RowOverride, channel_id, .. }
        if channel_id == "region"));
    assert!(matches!(
        resolve(&c, "b1"),
        MatchDecision::Matched {
            method: MatchMethod::ConfirmedGlobalAlias,
            ..
        }
    ));
    fk_ok(&c);
}

#[test]
fn playlist_exception_is_explicit_and_not_shared_or_implicit() {
    let mut c = setup();
    add_guide(&c, "other", true);
    add_channel(&c, "g", "TVG.EXACT", "Exact");
    add_channel(&c, "other", "TVG.EXACT", "Another");
    selected(&mut c, "g");
    set_playlist_exception(
        &mut c,
        ConfirmedGuideSelection::after_user_confirmation(),
        "playlist-b",
        Some("other"),
        4,
    )
    .unwrap();
    assert!(matches!(resolve(&c, "a1"),
        MatchDecision::Matched { source_id, .. } if source_id == "g"));
    assert!(matches!(resolve(&c, "b1"),
        MatchDecision::Matched { source_id, .. } if source_id == "other"));
    c.execute("UPDATE epg_sources SET enabled=0 WHERE id='other'", [])
        .unwrap();
    assert!(matches!(resolve(&c, "b1"),
        MatchDecision::Disabled { source_id } if source_id == "other"));
    assert!(matches!(resolve(&c, "a1"), MatchDecision::Matched { .. }));
    set_playlist_exception(
        &mut c,
        ConfirmedGuideSelection::after_user_confirmation(),
        "playlist-b",
        None,
        5,
    )
    .unwrap();
    assert!(matches!(resolve(&c, "b1"),
        MatchDecision::Matched { source_id, .. } if source_id == "g"));
    assert_eq!(count(&c, "epg_playlist_links"), 0);
}

#[test]
fn channel_refresh_does_not_erase_manual_intent_and_return_restores_it() {
    let mut c = setup();
    add_channel(&c, "g", "manual", "Station");
    selected(&mut c, "g");
    set_row_mapping(
        &mut c,
        ConfirmedRowMapping::after_user_confirmation_for_this_stream(),
        "a1",
        "g",
        "manual",
        5,
    )
    .unwrap();
    c.execute(
        "DELETE FROM epg_channels WHERE source_id='g' AND channel_id='manual'",
        [],
    )
    .unwrap();
    assert!(matches!(resolve(&c, "a1"),
        MatchDecision::MissingTarget { channel_id, .. } if channel_id == "manual"));
    assert_eq!(count(&c, "epg_channel_overrides"), 1);
    add_channel(&c, "g", "manual", "Station");
    assert!(matches!(
        resolve(&c, "a1"),
        MatchDecision::Matched {
            method: MatchMethod::RowOverride,
            ..
        }
    ));
}
#[test]
fn transactional_validation_source_deletion_and_cascades() {
    let mut c = setup();
    add_channel(&c, "g", "a", "Channel");
    selected(&mut c, "g");
    let initial_url: String = c
        .query_row(
            "SELECT stream_url FROM iptv_channels WHERE id='a1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let rejected = set_row_mapping(
        &mut c,
        ConfirmedRowMapping::after_user_confirmation_for_this_stream(),
        "a1",
        "g",
        "missing",
        5,
    );
    assert_eq!(rejected, Err(BindingError::NotFound));
    assert_eq!(count(&c, "epg_channel_overrides"), 0);
    set_row_mapping(
        &mut c,
        ConfirmedRowMapping::after_user_confirmation_for_this_stream(),
        "a1",
        "g",
        "a",
        5,
    )
    .unwrap();
    set_global_alias(
        &mut c,
        ConfirmedGlobalMapping::after_user_confirmation_for_all_matching_streams(),
        "b1",
        "g",
        "a",
        5,
    )
    .unwrap();
    set_playlist_exception(
        &mut c,
        ConfirmedGuideSelection::after_user_confirmation(),
        "playlist-b",
        Some("g"),
        5,
    )
    .unwrap();
    let after_url: String = c
        .query_row(
            "SELECT stream_url FROM iptv_channels WHERE id='a1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(initial_url, after_url);
    c.execute("DELETE FROM epg_sources WHERE id='g'", [])
        .unwrap();
    assert_eq!(count(&c, "epg_channels"), 0);
    assert_eq!(count(&c, "epg_channel_overrides"), 0);
    assert_eq!(count(&c, "epg_global_id_aliases"), 0);
    assert_eq!(count(&c, "epg_playlist_links"), 0);
    let shared: Option<String> = c
        .query_row(
            "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(shared, None);
    assert_eq!(count(&c, "iptv_channels"), 4);
    assert!(matches!(resolve(&c, "a1"), MatchDecision::Unconfigured));
    fk_ok(&c);
}
#[test]
fn confirmation_inputs_require_real_rows_scope_and_foreign_keys() {
    let mut c = setup();
    add_guide(&c, "other", true);
    add_channel(&c, "g", "a", "A");
    selected(&mut c, "g");
    let wrong = set_global_alias(
        &mut c,
        ConfirmedGlobalMapping::after_user_confirmation_for_all_matching_streams(),
        "a1",
        "other",
        "a",
        3,
    );
    assert!(wrong.is_err());
    assert_eq!(count(&c, "epg_global_id_aliases"), 0);
    assert_eq!(
        set_playlist_exception(
            &mut c,
            ConfirmedGuideSelection::after_user_confirmation(),
            "nonexistent",
            Some("g"),
            3
        ),
        Err(BindingError::NotFound)
    );
    c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    assert_eq!(
        set_shared_guide(
            &mut c,
            ConfirmedGuideSelection::after_user_confirmation(),
            Some("g")
        ),
        Err(BindingError::Storage)
    );
}
#[test]
fn now_next_half_open_gap_overlap_stale_and_cancellation() {
    let mut c = setup();
    add_channel(&c, "g", "TVG.EXACT", "Station");
    selected(&mut c, "g");
    for (start, stop, title) in [
        (900, 1000, "Earlier"),
        (1000, 1200, "Now"),
        (1200, 1300, "Next"),
    ] {
        c.execute(
            "INSERT INTO epg_programmes (source_id,channel_id,start_utc,stop_utc,title)
             VALUES ('g','TVG.EXACT',?1,?2,?3)",
            params![start, stop, title],
        )
        .unwrap();
    }
    let zero = AtomicBool::new(false);
    let state = read_semantic_now_next(&c, "a1", 1000, &zero).unwrap();
    if let SemanticProgrammeState::Matched { now_next, page, .. } = state {
        let pair = now_next.expect("fresh complete result");
        assert_eq!(pair.current.unwrap().title, "Now");
        assert_eq!(pair.next.unwrap().title, "Next");
        assert_eq!(pair.certainty, Certainty::Exact);
        assert_eq!(pair.progress_fraction, Some(0.0));
        assert_eq!(page.freshness, Freshness::Fresh);
        assert!(matches!(page.coverage, Coverage::Partial { .. }));
    } else {
        panic!("expected match");
    }
    c.execute(
        "INSERT INTO epg_programmes (source_id,channel_id,start_utc,stop_utc,title)
         VALUES ('g','TVG.EXACT',1000,1200,'Conflicting current')",
        [],
    )
    .unwrap();
    if let SemanticProgrammeState::Matched { now_next, .. } =
        read_semantic_now_next(&c, "a1", 1000, &zero).unwrap()
    {
        let pair = now_next.unwrap();
        assert_eq!(pair.certainty, Certainty::Ambiguous);
        assert!(pair.current.is_none());
        assert!(pair.progress_fraction.is_none());
    } else {
        panic!("expected overlap");
    }
    c.execute(
        "UPDATE epg_sources SET last_success_at=1,last_error_code='timeout' WHERE id='g'",
        [],
    )
    .unwrap();
    if let SemanticProgrammeState::Matched {
        now_next,
        refresh_failed,
        page,
        ..
    } = read_semantic_now_next(&c, "a1", 1_000_000, &zero).unwrap()
    {
        assert_eq!(page.freshness, Freshness::Stale);
        assert!(now_next.is_none());
        assert!(refresh_failed);
    } else {
        panic!("expected stale");
    }
    assert!(matches!(
        read_semantic_now_next(&c, "a1", 1000, &AtomicBool::new(true)),
        Err(EpgError::Cancelled)
    ));
}

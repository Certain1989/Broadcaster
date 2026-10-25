// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::epg_projection::{
    next_projection_deadline, DeliveryKey, ProjectionOutcome, ProjectionService,
};
use std::sync::{atomic::AtomicBool, Arc};

#[test]
fn stale_view_and_recycled_row_results_are_rejected() {
    let a = DeliveryKey {
        row_id: "iptv-row-one".into(),
        view_generation: 12,
        binding_generation: 9,
        epg_generation: 4,
    };
    assert!(a.current("iptv-row-one", 12, 9, 4));
    assert!(!a.current("iptv-row-two", 12, 9, 4));
    assert!(!a.current("iptv-row-one", 13, 9, 4));
    assert!(!a.current("iptv-row-one", 12, 10, 4));
    assert!(!a.current("iptv-row-one", 12, 9, 5));
}

#[tokio::test]
async fn cancelled_requests_do_not_touch_the_database_or_leak_budget() {
    let service = ProjectionService::new();
    for n in 0..100 {
        let token = Arc::new(AtomicBool::new(true));
        let result = service.read(format!("row-{n}"), 1_800_000_000, token).await;
        assert!(matches!(result, ProjectionOutcome::Cancelled));
    }
    assert_eq!(service.pending(), 0);
}

#[test]
fn more_than_25_admissions_recover_after_release_without_budget_leak() {
    let service = ProjectionService::new();
    let admitted: Vec<_> = (0..24)
        .map(|_| service.admit().expect("up to the fixed 24-ticket ceiling"))
        .collect();
    assert_eq!(service.pending(), 24);
    for _ in 0..10 {
        assert!(service.admit().is_none(), "the 25th+ row is NOT queued");
    }
    // A visible bound row can retry admission after another bound row
    // finishes, without exceeding the old hard queue budget.
    drop(admitted);
    assert_eq!(service.pending(), 0);
    let resumed = service.admit().expect("stable visible row can retry");
    assert_eq!(service.pending(), 1);
    drop(resumed);
    assert_eq!(service.pending(), 0);
}

#[test]
fn cancellation_releases_all_waiting_tickets() {
    let service = ProjectionService::new();
    let mut waiting = (0..24)
        .map(|_| service.admit().unwrap())
        .collect::<Vec<_>>();
    assert!(service.admit().is_none());
    waiting.truncate(3); // rapid unbind of 21 recycled GTK row identities
    assert_eq!(service.pending(), 3);
    assert!(service.admit().is_some());
    assert_eq!(service.pending(), 3); // temporary admission dropped immediately
    waiting.clear();
    assert_eq!(service.pending(), 0);
}

#[test]
fn injected_time_crosses_current_stop_next_start_gap_and_overlap() {
    let at = 1_800_000_000;
    let within_current = next_projection_deadline(at, true, None, [(at - 600, at + 12)]);
    assert_eq!(
        within_current,
        at + 12,
        "half-open current must expire at stop"
    );

    let gap = next_projection_deadline(at, false, None, [(at - 600, at - 1), (at + 23, at + 80)]);
    assert_eq!(gap, at + 23, "a guide gap is unknown, not inferred off-air");

    let overlap =
        next_projection_deadline(at, true, None, [(at - 100, at + 45), (at + 20, at + 120)]);
    assert_eq!(
        overlap,
        at + 20,
        "new overlapping programme changes certainty"
    );

    let progress = next_projection_deadline(at, true, None, [(at - 300, at + 600)]);
    assert_eq!(
        progress,
        at + 60,
        "progress gets a bounded single-view update"
    );
}

#[test]
fn injected_36h_72h_freshness_boundaries_and_overflow() {
    let at = 1_800_000_000;
    assert_eq!(
        next_projection_deadline(at, false, Some(at - 129_600), []),
        at + 1,
        "36h Fresh to Aging transition"
    );
    assert_eq!(
        next_projection_deadline(at, false, Some(at - 259_200), []),
        at + 1,
        "72h Aging to Stale transition"
    );
    assert_eq!(
        next_projection_deadline(at, false, Some(at - 15_000), []),
        at + 1800
    );
    assert!(next_projection_deadline(i64::MAX - 3, true, Some(i64::MAX - 3), []) > 0);
}

#[test]
fn sqlite_data_version_detects_external_writes_not_own_connection() {
    // The revision observer MUST keep one connection open. A new connection
    // each poll would return incomparable PRAGMA data_version values.
    let path = std::env::temp_dir().join(format!(
        "broadcaster-epg-revision-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer
        .execute_batch("CREATE TABLE revision_sample (x INTEGER);")
        .unwrap();
    let observer = rusqlite::Connection::open(&path).unwrap();
    let version = || -> i64 {
        observer
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap()
    };
    let before = version();
    writer
        .execute("INSERT INTO revision_sample(x) VALUES (1)", [])
        .unwrap();
    assert_ne!(
        version(),
        before,
        "independent committed writer must invalidate"
    );
    let after = version();
    observer
        .execute("INSERT INTO revision_sample(x) VALUES (2)", [])
        .unwrap();
    assert_eq!(
        version(),
        after,
        "data_version ignores writes on observer connection"
    );
    drop(observer);
    drop(writer);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn cancelling_waiting_row_releases_ticket_without_a_worker_permit() {
    let service = ProjectionService::new();
    let held = service.hold_all_permits_for_test().await;
    let token = Arc::new(AtomicBool::new(false));
    let worker = service.clone();
    let cancel = token.clone();
    let pending = tokio::spawn(async move {
        worker
            .read("synthetic-row".into(), 1_800_000_000, cancel)
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(service.pending(), 1);
    token.store(true, std::sync::atomic::Ordering::Release);
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), pending)
        .await
        .expect("canceled wait must return without any permit release")
        .expect("worker join");
    assert!(matches!(outcome, ProjectionOutcome::Cancelled));
    assert_eq!(service.pending(), 0);
    drop(held);
}

fn alias_fixture() -> rusqlite::Connection {
    use crate::streams::storage::{MIGRATION_001, MIGRATION_002};
    use rusqlite::params;

    let c = rusqlite::Connection::open_in_memory().expect("fabricated sqlite");
    c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    c.execute_batch(MIGRATION_001).unwrap();
    c.execute_batch(MIGRATION_002).unwrap();
    for playlist in ["playlist-a", "playlist-b"] {
        c.execute(
            "INSERT INTO iptv_sources (id,name,origin_kind,origin_locator,created_at,updated_at)
             VALUES (?1,?1,'local_file','/fabricated/playlist',1,1)",
            [playlist],
        )
        .unwrap();
    }
    for (id, playlist, order, tvg) in [
        ("a1", "playlist-a", 0, "CHANNEL.ID"),
        ("b1", "playlist-b", 0, "CHANNEL.ID"),
        ("a2", "playlist-a", 1, "channel.id"),
    ] {
        c.execute(
            "INSERT INTO iptv_channels
             (id,source_id,playlist_order,imported_name,stream_url,tvg_id,created_at,updated_at)
             VALUES (?1,?2,?3,'Fabricated','https://example.invalid/stream',?4,1,1)",
            params![id, playlist, order, tvg],
        )
        .unwrap();
    }
    for guide in ["shared", "exception"] {
        c.execute(
            "INSERT INTO epg_sources
             (id,name,origin_kind,origin_locator,enabled,created_at,updated_at,last_success_at)
             VALUES (?1,?1,'local_file','/fabricated/guide',1,1,1,1000)",
            [guide],
        )
        .unwrap();
    }
    for (source, target) in [
        ("shared", "global-target"),
        ("shared", "manual-target"),
        ("exception", "CHANNEL.ID"),
    ] {
        c.execute(
            "INSERT INTO epg_channels (source_id,channel_id,display_name)
             VALUES (?1,?2,'Fabricated programme channel')",
            params![source, target],
        )
        .unwrap();
    }
    c
}

#[test]
fn exact_global_alias_rejects_stale_same_uuid_tvg_id_without_side_effects() {
    use crate::epg::bindings::{
        set_global_alias_if_imported_id_matches, set_shared_guide, BindingError,
        ConfirmedGlobalMapping, ConfirmedGuideSelection,
    };
    let mut c = alias_fixture();
    set_shared_guide(
        &mut c,
        ConfirmedGuideSelection::after_user_confirmation(),
        Some("shared"),
    )
    .unwrap();

    // The editor showed CHANNEL.ID, then a playlist refresh retained a1's
    // UUID and URL while changing its imported tvg-id to a different key.
    c.execute("UPDATE iptv_channels SET tvg_id='CHANGED.ID' WHERE id='a1'", [])
        .unwrap();
    let consent = || ConfirmedGlobalMapping::after_user_confirmation_for_all_matching_streams();
    assert_eq!(
        set_global_alias_if_imported_id_matches(
            &mut c,
            consent(),
            "a1",
            "CHANNEL.ID",
            "shared",
            "global-target",
            2,
        ),
        Err(BindingError::InvalidInput),
        "user consent for CHANNEL.ID must not create an alias for CHANGED.ID"
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM epg_global_id_aliases", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    // Reject a differently cased expectation even if it looks similar.
    assert_eq!(
        set_global_alias_if_imported_id_matches(
            &mut c,
            consent(),
            "b1",
            "channel.id",
            "shared",
            "global-target",
            3,
        ),
        Err(BindingError::InvalidInput)
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM epg_global_id_aliases", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn exact_global_alias_two_playlists_preserves_scope_case_and_override_order() {
    use crate::epg::{
        bindings::{
            resolve_row, set_global_alias_if_imported_id_matches, set_playlist_exception,
            set_row_mapping, set_shared_guide, ConfirmedGlobalMapping,
            ConfirmedGuideSelection, ConfirmedRowMapping,
        },
        matching::{MatchDecision, MatchMethod},
    };
    let mut c = alias_fixture();
    let cancel = AtomicBool::new(false);
    set_shared_guide(
        &mut c,
        ConfirmedGuideSelection::after_user_confirmation(),
        Some("shared"),
    )
    .unwrap();
    set_global_alias_if_imported_id_matches(
        &mut c,
        ConfirmedGlobalMapping::after_user_confirmation_for_all_matching_streams(),
        "a1",
        "CHANNEL.ID",
        "shared",
        "global-target",
        2,
    )
    .unwrap();

    for id in ["a1", "b1"] {
        assert!(matches!(
            resolve_row(&c, id, &cancel).unwrap(),
            MatchDecision::Matched {
                method: MatchMethod::ConfirmedGlobalAlias,
                source_id,
                channel_id,
            } if source_id == "shared" && channel_id == "global-target"
        ));
    }
    assert!(
        !matches!(
            resolve_row(&c, "a2", &cancel).unwrap(),
            MatchDecision::Matched { method: MatchMethod::ConfirmedGlobalAlias, .. }
        ),
        "lower-case imported tvg-id cannot inherit upper-case alias"
    );
    set_row_mapping(
        &mut c,
        ConfirmedRowMapping::after_user_confirmation_for_this_stream(),
        "a1",
        "shared",
        "manual-target",
        3,
    )
    .unwrap();
    assert!(matches!(
        resolve_row(&c, "a1", &cancel).unwrap(),
        MatchDecision::Matched {
            method: MatchMethod::RowOverride,
            channel_id,
            ..
        } if channel_id == "manual-target"
    ));
    set_playlist_exception(
        &mut c,
        ConfirmedGuideSelection::after_user_confirmation(),
        "playlist-b",
        Some("exception"),
        4,
    )
    .unwrap();
    assert!(matches!(
        resolve_row(&c, "b1", &cancel).unwrap(),
        MatchDecision::Matched {
            source_id,
            channel_id,
            ..
        } if source_id == "exception" && channel_id == "CHANNEL.ID"
    ));
    // An unrelated playlist/guide exception cannot override the manual row
    // or rewrite the shared guide's exact alias entry.
    assert!(matches!(
        resolve_row(&c, "a1", &cancel).unwrap(),
        MatchDecision::Matched {
            method: MatchMethod::RowOverride,
            ..
        }
    ));
    assert_eq!(
        c.query_row(
            "SELECT iptv_tvg_id FROM epg_global_id_aliases WHERE guide_source_id='shared'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
        "CHANNEL.ID"
    );
}

#[test]
fn exact_global_alias_validates_expected_id_and_selected_guide() {
    use crate::epg::bindings::{
        set_global_alias_if_imported_id_matches, set_shared_guide, BindingError,
        ConfirmedGlobalMapping, ConfirmedGuideSelection,
    };
    let mut c = alias_fixture();
    let consent = || ConfirmedGlobalMapping::after_user_confirmation_for_all_matching_streams();
    set_shared_guide(
        &mut c,
        ConfirmedGuideSelection::after_user_confirmation(),
        Some("shared"),
    )
    .unwrap();
    for invalid in ["", " CHANNEL.ID", "CHANNEL.ID ", "CHANNEL\n.ID"] {
        assert_eq!(
            set_global_alias_if_imported_id_matches(
                &mut c,
                consent(),
                "a1",
                invalid,
                "shared",
                "global-target",
                3
            ),
            Err(BindingError::InvalidInput)
        );
    }
    // The guide selection may have been changed after confirmation.
    set_shared_guide(
        &mut c,
        ConfirmedGuideSelection::after_user_confirmation(),
        Some("exception"),
    )
    .unwrap();
    assert_eq!(
        set_global_alias_if_imported_id_matches(
            &mut c,
            consent(),
            "a1",
            "CHANNEL.ID",
            "shared",
            "global-target",
            4,
        ),
        Err(BindingError::InvalidInput)
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM epg_global_id_aliases", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

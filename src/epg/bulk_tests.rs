// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later
use super::bulk::*;
use crate::streams::storage::{MIGRATION_001, MIGRATION_002};
use rusqlite::{params, Connection};
use std::sync::atomic::AtomicBool;

fn connection() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    c.execute_batch(MIGRATION_001).unwrap();
    c.execute_batch(MIGRATION_002).unwrap();
    for src in ["a", "b", "c"] {
        c.execute(
            "INSERT INTO iptv_sources(id,name,origin_kind,origin_locator,created_at,updated_at) VALUES (?1,?1,'local_file','/fabricated',1,1)",
            [src],
        ).unwrap();
    }
    for (id, source, tvg, number) in [
        ("r1", "a", "ZDF@HD", 0),
        ("r2", "b", "ZDF@HD", 0),
        ("r3", "b", "zdf@hd", 1),
        ("r4", "c", "ZDF@HD", 0),
    ] {
        c.execute(
            "INSERT INTO iptv_channels(id,source_id,playlist_order,imported_name,stream_url,tvg_id,created_at,updated_at)
             VALUES (?1,?2,?3,'Demo','https://invalid.example/unused',?4,1,1)",
            params![id, source, number, tvg],
        ).unwrap();
    }
    for guide in ["guide-a", "guide-b"] {
        c.execute("INSERT INTO epg_sources(id,name,origin_kind,origin_locator,enabled,created_at,updated_at,last_success_at) VALUES (?1,?1,'local_file','/fabricated',1,1,1,99)", [guide]).unwrap();
    }
    c.execute("INSERT INTO epg_channels(source_id,channel_id,display_name) VALUES ('guide-a','ZDF','ZDF')", []).unwrap();
    c.execute("INSERT INTO epg_channels(source_id,channel_id,display_name) VALUES ('guide-b','ZDF','ZDF')", []).unwrap();
    c.execute(
        "UPDATE epg_settings SET shared_guide_source_id='guide-a' WHERE singleton=1",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO epg_playlist_links(playlist_source_id,guide_source_id,updated_at) VALUES ('c','guide-b',1)", []).unwrap();
    c.execute("INSERT INTO epg_channel_overrides(iptv_channel_id,guide_source_id,guide_channel_id,updated_at) VALUES ('r2','guide-a','ZDF',1)", []).unwrap();
    c
}
fn choice() -> AliasChoice {
    AliasChoice {
        original_tvg_id: "ZDF@HD".into(),
        full_xmltv_id: "ZDF".into(),
    }
}
fn cancel() -> AtomicBool {
    AtomicBool::new(false)
}
fn alias_count(c: &Connection) -> i64 {
    c.query_row("SELECT count(*) FROM epg_global_id_aliases", [], |r| {
        r.get(0)
    })
    .unwrap()
}

#[test]
fn approved_keys_only_scoped_to_guide_and_conditional_undo() {
    let mut c = connection();
    let p = preview_bulk(&mut c, "guide-a", &[choice()], &cancel()).unwrap();
    assert_eq!(p.impact().len(), 1);
    assert_eq!(p.impact()[0].selected_guide_rows, 2);
    assert_eq!(p.impact()[0].manual_exclusions, 1);
    assert_eq!(p.impact()[0].other_guide_rows, 1);
    assert!(p.impact()[0].future_rows_also_affected);
    let undo = apply_bulk(
        &mut c,
        ConfirmedBulkReview::after_user_confirmation(),
        p,
        100,
        &cancel(),
    )
    .unwrap();
    assert_eq!(alias_count(&c), 1);
    assert_eq!(c.query_row("SELECT guide_channel_id FROM epg_global_id_aliases WHERE guide_source_id='guide-a' AND iptv_tvg_id='ZDF@HD'",[], |r|r.get::<_,String>(0)).unwrap(), "ZDF");
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM epg_global_id_aliases WHERE guide_source_id='guide-b'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    undo_same_session(&mut c, undo, &cancel()).unwrap();
    assert_eq!(alias_count(&c), 0);
}

#[test]
fn stale_by_playlist_row_revision_and_by_guide_programme_contents() {
    let mut c = connection();
    let p = preview_bulk(&mut c, "guide-a", &[choice()], &cancel()).unwrap();
    // Current playable imported ID changes; no alias should be created.
    c.execute(
        "UPDATE iptv_channels SET tvg_id='ZDF@FHD',last_seen_revision=2 WHERE id='r1'",
        [],
    )
    .unwrap();
    assert!(matches!(
        apply_bulk(
            &mut c,
            ConfirmedBulkReview::after_user_confirmation(),
            p,
            1,
            &cancel()
        ),
        Err(BulkError::Stale)
    ));
    assert_eq!(alias_count(&c), 0);
    let p = preview_bulk(&mut c, "guide-a", &[choice()], &cancel()).unwrap();
    c.execute("INSERT INTO epg_programmes (source_id,channel_id,start_utc,stop_utc,title) VALUES ('guide-a','ZDF',100,200,'New guide contents')", []).unwrap();
    assert!(matches!(
        apply_bulk(
            &mut c,
            ConfirmedBulkReview::after_user_confirmation(),
            p,
            2,
            &cancel()
        ),
        Err(BulkError::Stale)
    ));
    assert_eq!(alias_count(&c), 0);
}

#[test]
fn accepted_mapping_does_not_override_confirmed_alias_and_undo_never_destroys_newer_edit() {
    let mut c = connection();
    let p = preview_bulk(&mut c, "guide-a", &[choice()], &cancel()).unwrap();
    let undo = apply_bulk(
        &mut c,
        ConfirmedBulkReview::after_user_confirmation(),
        p,
        100,
        &cancel(),
    )
    .unwrap();
    assert!(matches!(
        preview_bulk(&mut c, "guide-a", &[choice()], &cancel()),
        Err(BulkError::ExistingAlias)
    ));
    c.execute(
        "UPDATE epg_global_id_aliases SET updated_at=101 WHERE guide_source_id='guide-a'",
        [],
    )
    .unwrap();
    assert!(matches!(
        undo_same_session(&mut c, undo, &cancel()),
        Err(BulkError::Stale)
    ));
    assert_eq!(alias_count(&c), 1);
}

#[test]
fn cancellation_and_wrong_selected_guide_leave_no_writes() {
    let mut c = connection();
    let p = preview_bulk(&mut c, "guide-a", &[choice()], &cancel()).unwrap();
    c.execute(
        "UPDATE epg_playlist_links SET guide_source_id='guide-a' WHERE playlist_source_id='c'",
        [],
    )
    .unwrap();
    assert!(matches!(
        apply_bulk(
            &mut c,
            ConfirmedBulkReview::after_user_confirmation(),
            p,
            10,
            &cancel()
        ),
        Err(BulkError::Stale)
    ));
    let p = preview_bulk(&mut c, "guide-a", &[choice()], &cancel()).unwrap();
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        apply_bulk(
            &mut c,
            ConfirmedBulkReview::after_user_confirmation(),
            p,
            10,
            &cancelled
        ),
        Err(BulkError::Cancelled)
    ));
    assert_eq!(alias_count(&c), 0);
}

#[test]
fn empty_case_differences_duplicate_and_nonexistent_targets_are_rejected() {
    let mut c = connection();
    assert!(matches!(
        preview_bulk(&mut c, "guide-a", &[], &cancel()),
        Err(BulkError::InvalidInput)
    ));
    assert!(matches!(
        preview_bulk(&mut c, "guide-a", &[choice(), choice()], &cancel()),
        Err(BulkError::InvalidInput)
    ));
    assert!(matches!(
        preview_bulk(
            &mut c,
            "guide-a",
            &[AliasChoice {
                original_tvg_id: " ".into(),
                full_xmltv_id: "ZDF".into()
            }],
            &cancel()
        ),
        Err(BulkError::InvalidInput)
    ));
    assert!(matches!(
        preview_bulk(
            &mut c,
            "guide-a",
            &[AliasChoice {
                original_tvg_id: "ZDF@HD".into(),
                full_xmltv_id: "Missing".into()
            }],
            &cancel()
        ),
        Err(BulkError::MissingGuideOrTarget)
    ));
    let p = preview_bulk(
        &mut c,
        "guide-a",
        &[AliasChoice {
            original_tvg_id: "zdf@hd".into(),
            full_xmltv_id: "ZDF".into(),
        }],
        &cancel(),
    )
    .unwrap();
    let _u = apply_bulk(
        &mut c,
        ConfirmedBulkReview::after_user_confirmation(),
        p,
        20,
        &cancel(),
    )
    .unwrap();
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM epg_global_id_aliases WHERE iptv_tvg_id='ZDF@HD'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn database_failure_during_second_insert_rolls_back_first_alias() {
    let mut c = connection();
    let second = AliasChoice {
        original_tvg_id: "ZDF@FHD".into(),
        full_xmltv_id: "ZDF".into(),
    };
    let p = preview_bulk(&mut c, "guide-a", &[choice(), second], &cancel()).unwrap();
    c.execute_batch(
        "CREATE TRIGGER fail_second_alias BEFORE INSERT ON epg_global_id_aliases
         WHEN NEW.iptv_tvg_id='ZDF@HD' BEGIN SELECT RAISE(ABORT,'injected'); END;",
    )
    .unwrap();
    assert!(matches!(
        apply_bulk(
            &mut c,
            ConfirmedBulkReview::after_user_confirmation(),
            p,
            2,
            &cancel()
        ),
        Err(BulkError::Storage)
    ));
    assert_eq!(alias_count(&c), 0);
}

#[test]
fn same_count_programme_revision_and_guide_disable_veto_approval() {
    let mut c = connection();
    c.execute(
        "INSERT INTO epg_programmes (source_id,channel_id,start_utc,stop_utc,title) VALUES ('guide-a','ZDF',100,200,'First')",
        [],
    ).unwrap();
    let p = preview_bulk(&mut c, "guide-a", &[choice()], &cancel()).unwrap();
    c.execute(
        "UPDATE epg_programmes SET title='Second' WHERE source_id='guide-a'",
        [],
    )
    .unwrap();
    assert!(matches!(
        apply_bulk(
            &mut c,
            ConfirmedBulkReview::after_user_confirmation(),
            p,
            3,
            &cancel()
        ),
        Err(BulkError::Stale)
    ));
    let p = preview_bulk(&mut c, "guide-a", &[choice()], &cancel()).unwrap();
    c.execute("UPDATE epg_sources SET enabled=0 WHERE id='guide-a'", [])
        .unwrap();
    assert!(matches!(
        apply_bulk(
            &mut c,
            ConfirmedBulkReview::after_user_confirmation(),
            p,
            3,
            &cancel()
        ),
        Err(BulkError::MissingGuideOrTarget)
    ));
    assert_eq!(alias_count(&c), 0);
}

#[test]
fn discovery_and_impact_share_one_read_revision_with_collision_veto() {
    let mut c = connection();
    let token = preview_discovered_bulk(&mut c, "guide-a", &[choice()], &cancel()).unwrap();
    assert_eq!(token.impact()[0].selected_guide_rows, 2);
    // Another feed variant makes the original stem ambiguous, although the
    // originally proposed full target still exists in this guide.
    c.execute(
        "INSERT INTO epg_channels(source_id,channel_id,display_name) VALUES ('guide-a','ZDF@SD','ZDF SD')",
        [],
    )
    .unwrap();
    assert!(matches!(
        preview_discovered_bulk(&mut c, "guide-a", &[choice()], &cancel()),
        Err(BulkError::Stale)
    ));
    assert!(matches!(
        apply_bulk(
            &mut c,
            ConfirmedBulkReview::after_user_confirmation(),
            token,
            102,
            &cancel()
        ),
        Err(BulkError::Stale)
    ));
    assert_eq!(alias_count(&c), 0);
}

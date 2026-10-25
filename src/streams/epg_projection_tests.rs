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

// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::epg_projection::{DeliveryKey, ProjectionOutcome, ProjectionService};
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

// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Cached EPG projections for hydrated My Channels rows only.
//! No GTK, network access, cache mutation, or playlist URL handling here.
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use tokio::sync::Semaphore;

use crate::{
    epg::{bindings::{read_semantic_now_next, SemanticProgrammeState}, model::EpgError},
    streams::storage::Storage,
};

const MAX_ACTIVE: usize = 2;
const MAX_PENDING: usize = 24;

/// Stable identity plus all UI generations required before painting a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeliveryKey {
    pub row_id: String,
    pub view_generation: u64,
    pub binding_generation: u64,
    pub epg_generation: u64,
}

impl DeliveryKey {
    pub fn current(&self, row_id: &str, view: u64, binding: u64, epg: u64) -> bool {
        self.row_id == row_id
            && self.view_generation == view
            && self.binding_generation == binding
            && self.epg_generation == epg
    }
}

struct Pending(Arc<AtomicUsize>);
impl Drop for Pending {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(super) enum ProjectionOutcome {
    Busy,
    Cancelled,
    Read(Result<SemanticProgrammeState, EpgError>),
}

/// Clones share one bounded work budget across all visible list rows.
#[derive(Clone, Debug)]
pub(super) struct ProjectionService {
    permits: Arc<Semaphore>,
    pending: Arc<AtomicUsize>,
}

impl ProjectionService {
    pub fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(MAX_ACTIVE)),
            pending: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Backlog is bounded even when a fast D-pad/scroll creates many bindings.
    /// The atomic cancellation token is set on unbind/reload/mapping changes.
    /// No IPC or GTK work occurs on the blocking thread.
    pub async fn read(
        &self,
        row_id: String,
        at_utc: i64,
        cancelled: Arc<AtomicBool>,
    ) -> ProjectionOutcome {
        if cancelled.load(Ordering::Acquire) {
            return ProjectionOutcome::Cancelled;
        }
        if self
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_PENDING).then_some(n + 1)
            })
            .is_err()
        {
            return ProjectionOutcome::Busy;
        }
        let ticket = Pending(self.pending.clone());
        let permits = self.permits.clone();
        crate::utils::tokio(async move {
            let _ticket = ticket;
            let Ok(_permit) = permits.acquire_owned().await else {
                return ProjectionOutcome::Cancelled;
            };
            if cancelled.load(Ordering::Acquire) {
                return ProjectionOutcome::Cancelled;
            }
            let worker_cancel = cancelled.clone();
            match tokio::task::spawn_blocking(move || {
                let connection = Storage::new().connect().map_err(|_| EpgError::Storage)?;
                read_semantic_now_next(&connection, &row_id, at_utc, &worker_cancel)
            })
            .await
            {
                Ok(result) if !cancelled.load(Ordering::Acquire) => {
                    ProjectionOutcome::Read(result)
                }
                _ => ProjectionOutcome::Cancelled,
            }
        })
        .await
    }

    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }
}

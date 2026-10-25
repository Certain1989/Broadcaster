// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Cached EPG projections for hydrated My Channels rows only.
//! A persistent read-only SQLite observer notices commits by independent guide
//! refresh, mapping and playlist connections through PRAGMA data_version.
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex,
};

use tokio::sync::Semaphore;

use crate::{
    epg::{
        bindings::{read_semantic_now_next, SemanticProgrammeState},
        model::EpgError,
    },
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

/// The one observer connection remains open: data_version values from separate
/// connections cannot be compared. This connection never writes to SQLite.
struct RevisionProbe {
    connection: rusqlite::Connection,
    version: i64,
}

pub(super) struct Pending(Arc<AtomicUsize>);
impl Drop for Pending {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(super) enum ProjectionOutcome {
    Busy,
    Cancelled,
    SourceChanged,
    Read(Result<SemanticProgrammeState, EpgError>),
}

/// Clones share the same hard admission budget and database change observer.
#[derive(Clone)]
pub(super) struct ProjectionService {
    permits: Arc<Semaphore>,
    pending: Arc<AtomicUsize>,
    probe: Arc<Mutex<Option<RevisionProbe>>>,
    revision: Arc<AtomicU64>,
}

impl std::fmt::Debug for ProjectionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectionService")
            .field("pending", &self.pending.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl ProjectionService {
    pub fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(MAX_ACTIVE)),
            pending: Arc::new(AtomicUsize::new(0)),
            probe: Arc::new(Mutex::new(None)),
            revision: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// One persistent observer, called on a blocking worker, detects EVERY
    /// committed change from other SQLite connections (including opted-in
    /// scheduled EPG refresh and independent playlist/mapping writes). Never
    /// query data_version on a new connection per poll: that is not a revision.
    pub async fn check_revision(&self) -> Result<bool, EpgError> {
        let probe = self.probe.clone();
        let revision = self.revision.clone();
        crate::utils::tokio(async move {
            tokio::task::spawn_blocking(move || {
                let mut guard = probe.lock().map_err(|_| EpgError::Storage)?;
                if guard.is_none() {
                    let connection = Storage::new().connect().map_err(|_| EpgError::Storage)?;
                    let version = connection
                        .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
                        .map_err(|_| EpgError::Storage)?;
                    *guard = Some(RevisionProbe { connection, version });
                    return Ok(false);
                }
                let observer = guard.as_mut().expect("observer initialized");
                let new_version = observer
                    .connection
                    .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
                    .map_err(|_| EpgError::Storage)?;
                if new_version != observer.version {
                    observer.version = new_version;
                    revision.fetch_add(1, Ordering::AcqRel);
                    Ok(true)
                } else {
                    Ok(false)
                }
            })
            .await
            .map_err(|_| EpgError::Storage)?
        })
        .await
    }

    /// Admission is real: Busy is not queued. Releasing this RAII ticket wakes
    /// no hidden tasks; the bound visible-row caller retries with cancellation.
    pub(super) fn admit(&self) -> Option<Pending> {
        self.pending
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_PENDING).then_some(n + 1)
            })
            .ok()
            .map(|_| Pending(self.pending.clone()))
    }

    pub async fn read(
        &self,
        row_id: String,
        at_utc: i64,
        cancelled: Arc<AtomicBool>,
    ) -> ProjectionOutcome {
        if cancelled.load(Ordering::Acquire) {
            return ProjectionOutcome::Cancelled;
        }
        let Some(ticket) = self.admit() else {
            return ProjectionOutcome::Busy;
        };
        let permits = self.permits.clone();
        let service = self.clone();
        crate::utils::tokio(async move {
            let _ticket = ticket;
            // The atomic UI cancellation token has no async notifier. Check
            // it while waiting so recycled rows release their admission slot
            // even when both blocking workers are occupied by slow lookups.
            let _permit = loop {
                tokio::select! {
                    acquired = permits.clone().acquire_owned() => {
                        match acquired {
                            Ok(permit) => break permit,
                            Err(_) => return ProjectionOutcome::Cancelled,
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                        if cancelled.load(Ordering::Acquire) {
                            return ProjectionOutcome::Cancelled;
                        }
                    }
                }
            };
            if cancelled.load(Ordering::Acquire) {
                return ProjectionOutcome::Cancelled;
            }
            match service.check_revision().await {
                Ok(true) => return ProjectionOutcome::SourceChanged,
                Ok(false) => {}
                Err(error) => return ProjectionOutcome::Read(Err(error)),
            }
            let before = service.revision();
            let worker_cancel = cancelled.clone();
            let result = tokio::task::spawn_blocking(move || {
                let connection = Storage::new().connect().map_err(|_| EpgError::Storage)?;
                read_semantic_now_next(&connection, &row_id, at_utc, &worker_cancel)
            })
            .await;
            if cancelled.load(Ordering::Acquire) {
                return ProjectionOutcome::Cancelled;
            }
            match service.check_revision().await {
                Ok(_) if before != service.revision() => ProjectionOutcome::SourceChanged,
                Ok(_) => match result {
                    Ok(result) => ProjectionOutcome::Read(result),
                    Err(_) => ProjectionOutcome::Cancelled,
                },
                Err(error) => ProjectionOutcome::Read(Err(error)),
            }
        })
        .await
    }

    #[cfg(test)]
    pub async fn hold_all_permits_for_test(&self) -> tokio::sync::OwnedSemaphorePermit {
        self.permits.clone().acquire_many_owned(MAX_ACTIVE as u32).await.unwrap()
    }

    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }
}

/// One monotonic UTC deadline per bound row; only the single view timer checks
/// them. Includes interval START and STOP (overlap and gaps), freshness category
/// boundaries at 36h/72h, and a bounded progress tick for current programmes.
pub(super) fn next_projection_deadline(
    at: i64,
    has_current: bool,
    last_success_at: Option<i64>,
    intervals: impl IntoIterator<Item = (i64, i64)>,
) -> i64 {
    let mut deadline = at.saturating_add(if has_current { 60 } else { 1800 });
    for (start, stop) in intervals {
        for boundary in [start, stop] {
            if boundary > at {
                deadline = deadline.min(boundary);
            }
        }
    }
    if let Some(last) = last_success_at {
        for boundary in [last.saturating_add(129_601), last.saturating_add(259_201)] {
            if boundary > at {
                deadline = deadline.min(boundary);
            }
        }
    }
    deadline.max(at.saturating_add(1))
}

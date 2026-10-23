// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Asynchronous, application-wide XMLTV coordinator. It never consults M3U
//! hints, stream headers, Live programmes or view repaint/focus events.
use super::{
    cache::replace_guide_snapshot,
    model::EpgError,
    network::{self, GuideCancellation, GuideFetchError, GuidePayload},
    sources::{
        ConfirmedGuideDeletion, GuideSource, GuideSourceRegistry, RegistryError,
        SourceOrigin,
    },
    xmltv::{decode_xmltv, ParseLimits},
};
use crate::streams::storage::Storage;
use adw::{gio, glib, prelude::*};
use std::{
    collections::HashMap,
    fmt,
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuideStage {
    Idle,
    Queued,
    Fetching,
    Decoding,
    Validating,
    Committing,
    Succeeded,
    Failed,
    Cancelled,
}
impl GuideStage {
    fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuideServiceError {
    Source(RegistryError),
    Fetch(GuideFetchError),
    InvalidXml,
    Storage,
    Disabled,
    AlreadyRunning,
    Cancelled,
    LocalAccess,
    WorkerFailed,
}
impl fmt::Display for GuideServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Source(error) => return write!(f, "{error}"),
            Self::Fetch(error) => return write!(f, "{error}"),
            Self::InvalidXml => "invalid_xml",
            Self::Storage => "storage_failed",
            Self::Disabled => "source_disabled",
            Self::AlreadyRunning => "refresh_in_progress",
            Self::Cancelled => "cancelled",
            Self::LocalAccess => "local_access",
            Self::WorkerFailed => "worker_failed",
        })
    }
}
impl std::error::Error for GuideServiceError {}
impl From<RegistryError> for GuideServiceError {
    fn from(value: RegistryError) -> Self {
        Self::Source(value)
    }
}
impl From<GuideFetchError> for GuideServiceError {
    fn from(value: GuideFetchError) -> Self {
        if value == GuideFetchError::Cancelled {
            Self::Cancelled
        } else {
            Self::Fetch(value)
        }
    }
}
fn clock() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs() as i64)
        .unwrap_or(0)
}
struct Running {
    transfer: GuideCancellation,
    parser: Arc<AtomicBool>,
    stage: GuideStage,
}
struct Inner {
    registry: GuideSourceRegistry,
    running: Mutex<HashMap<String, Running>>,
}
#[derive(Clone)]
pub struct GlobalGuideManager {
    inner: Arc<Inner>,
}
impl fmt::Debug for GlobalGuideManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GlobalGuideManager(<private>)")
    }
}
impl GlobalGuideManager {
    /// Shared service must be instantiated once by the application.
    pub async fn open() -> Result<Self, GuideServiceError> {
        Self::with_storage(Storage::new()).await
    }
    pub async fn with_storage(storage: Storage) -> Result<Self, GuideServiceError> {
        let init = storage.clone();
        crate::utils::tokio(async move {
            tokio::task::spawn_blocking(move || init.initialize())
                .await
                .map_err(|_| GuideServiceError::WorkerFailed)?
                .map_err(|_| GuideServiceError::Storage)
        })
        .await?;
        Ok(Self {
            inner: Arc::new(Inner {
                registry: GuideSourceRegistry::new(storage),
                running: Mutex::new(HashMap::new()),
            }),
        })
    }
    async fn db<T, F>(&self, task: F) -> Result<T, GuideServiceError>
    where
        T: Send + 'static,
        F: FnOnce(GuideSourceRegistry) -> Result<T, RegistryError> + Send + 'static,
    {
        let registry = self.inner.registry.clone();
        crate::utils::tokio(async move {
            tokio::task::spawn_blocking(move || task(registry))
                .await
                .map_err(|_| GuideServiceError::WorkerFailed)?
                .map_err(GuideServiceError::Source)
        })
        .await
    }
    pub async fn list(&self) -> Result<Vec<GuideSource>, GuideServiceError> {
        self.db(|registry| registry.list()).await
    }
    pub async fn add(
        &self,
        name: String,
        origin: SourceOrigin,
    ) -> Result<GuideSource, GuideServiceError> {
        self.db(move |registry| registry.add(&name, &origin)).await
    }
    pub async fn rename(&self, id: String, name: String) -> Result<(), GuideServiceError> {
        self.db(move |registry| registry.rename(&id, &name)).await
    }
    pub async fn set_enabled(&self, id: String, enabled: bool) -> Result<(), GuideServiceError> {
        if !enabled {
            self.cancel(&id).await;
        }
        self.db(move |registry| registry.set_enabled(&id, enabled))
            .await
    }
    pub async fn set_schedule(
        &self,
        id: String,
        enabled: bool,
        seconds: i64,
    ) -> Result<(), GuideServiceError> {
        self.db(move |registry| registry.set_schedule(&id, enabled, seconds))
            .await
    }
    pub async fn delete_confirmed(
        &self,
        approval: ConfirmedGuideDeletion,
    ) -> Result<(), GuideServiceError> {
        self.cancel(&approval.id()).await;
        self.db(move |registry| registry.delete_confirmed(&approval))
            .await
    }
    pub async fn due_ids(&self) -> Result<Vec<String>, GuideServiceError> {
        self.db(move |registry| registry.due_source_ids(clock()))
            .await
    }
    pub async fn stage(&self, id: &str) -> GuideStage {
        self.inner
            .running
            .lock()
            .await
            .get(id)
            .map(|running| running.stage)
            .unwrap_or(GuideStage::Idle)
    }
    pub async fn cancel(&self, id: &str) -> bool {
        let mut current = self.inner.running.lock().await;
        let Some(running) = current.get_mut(id) else {
            return false;
        };
        if running.stage.terminal() {
            return false;
        }
        running.parser.store(true, Ordering::Release);
        running.transfer.cancel();
        // Keep the in-flight slot occupied until the blocked operation stops.
        // Otherwise a second refresh could race the cancelled worker.
        true
    }
    async fn start(
        &self,
        id: &str,
    ) -> Result<(GuideCancellation, Arc<AtomicBool>), GuideServiceError> {
        let mut current = self.inner.running.lock().await;
        if current
            .get(id)
            .is_some_and(|running| !running.stage.terminal())
        {
            return Err(GuideServiceError::AlreadyRunning);
        }
        let transfer = GuideCancellation::new();
        let parser = Arc::new(AtomicBool::new(false));
        current.insert(
            id.to_owned(),
            Running {
                transfer: transfer.clone(),
                parser: parser.clone(),
                stage: GuideStage::Queued,
            },
        );
        Ok((transfer, parser))
    }
    async fn transition(&self, id: &str, next: GuideStage) -> Result<(), GuideServiceError> {
        let mut current = self.inner.running.lock().await;
        let running = current.get_mut(id).ok_or(GuideServiceError::Cancelled)?;
        if running.transfer.is_cancelled() || running.parser.load(Ordering::Acquire) {
            return Err(GuideServiceError::Cancelled);
        }
        running.stage = next;
        Ok(())
    }
    async fn terminal(&self, id: &str, outcome: &Result<(), GuideServiceError>) {
        let mut current = self.inner.running.lock().await;
        if let Some(running) = current.get_mut(id) {
            running.stage = match outcome {
                Ok(()) => GuideStage::Succeeded,
                Err(GuideServiceError::Cancelled) => GuideStage::Cancelled,
                Err(_) => GuideStage::Failed,
            }
        }
    }

    /// No request is made by adding or listing sources: call only from an
    /// explicit Refresh control or a separately opted-in schedule.
    pub async fn refresh(&self, id: String) -> Result<(), GuideServiceError> {
        let (transfer, parser_cancel) = self.start(&id).await?;
        let result = self.refresh_one(&id, &transfer, &parser_cancel).await;
        if let Err(error) = result {
            let error_code = match error {
                GuideServiceError::Fetch(error) => error.code(),
                GuideServiceError::InvalidXml => "invalid_xml",
                GuideServiceError::Storage => "storage_failed",
                GuideServiceError::LocalAccess => "local_access",
                GuideServiceError::Cancelled => "cancelled",
                GuideServiceError::Disabled => "source_disabled",
                _ => "network",
            };
            // Best effort only: never mask the original sanitized error.
            let source_id = id.clone();
            let _ = self
                .db(move |registry| registry.record_failure(&source_id, error_code))
                .await;
            self.terminal(&id, &Err(error)).await;
            Err(error)
        } else {
            self.terminal(&id, &Ok(())).await;
            Ok(())
        }
    }

    async fn refresh_one(
        &self,
        id: &str,
        transfer: &GuideCancellation,
        parser_cancel: &Arc<AtomicBool>,
    ) -> Result<(), GuideServiceError> {
        let source_id = id.to_owned();
        let source = self
            .db(move |registry| registry.get_private(&source_id))
            .await?;
        if !source.view.enabled {
            return Err(GuideServiceError::Disabled);
        }
        self.transition(id, GuideStage::Fetching).await?;
        let payload = match source.origin {
            SourceOrigin::RemoteUrl { locator, trust } => {
                let cancellation = transfer.clone();
                crate::utils::tokio(async move {
                    network::fetch_guide(&locator, trust, &cancellation).await
                })
                .await?
            }
            SourceOrigin::LocalFile(locator) => read_local(&locator, transfer).await?,
        };
        self.transition(id, GuideStage::Decoding).await?;
        // E1's strict monotonic success guard refuses equal-second retries.
        // A same-second refresh receives a unique future logical instant;
        // out-of-order refresh jobs are also excluded by source coalescing.
        let next = source
            .view
            .last_success_at
            .map(|value| value.checked_add(1).ok_or(GuideServiceError::Storage))
            .transpose()?
            .unwrap_or(0);
        let timestamp = clock().max(next);
        let source_id = id.to_owned();
        let cancelled = parser_cancel.clone();
        let snapshot = crate::utils::tokio(async move {
            tokio::task::spawn_blocking(move || {
                decode_xmltv(
                    Cursor::new(payload.bytes),
                    &source_id,
                    timestamp,
                    payload.gzip,
                    ParseLimits::default(),
                    &cancelled,
                )
            })
            .await
            .map_err(|_| GuideServiceError::WorkerFailed)?
            .map_err(|error| match error {
                EpgError::Cancelled => GuideServiceError::Cancelled,
                _ => GuideServiceError::InvalidXml,
            })
        })
        .await?;
        self.transition(id, GuideStage::Validating).await?;
        self.transition(id, GuideStage::Committing).await?;
        let store = self.inner.registry.clone();
        let cancelled = parser_cancel.clone();
        crate::utils::tokio(async move {
            tokio::task::spawn_blocking(move || {
                let mut conn = store
                    .connect_private()
                    .map_err(|_| GuideServiceError::Storage)?;
                replace_guide_snapshot(&mut conn, &snapshot, &cancelled).map_err(|error| {
                    if error == EpgError::Cancelled {
                        GuideServiceError::Cancelled
                    } else {
                        GuideServiceError::Storage
                    }
                })
            })
            .await
            .map_err(|_| GuideServiceError::WorkerFailed)?
        })
        .await
    }
}

/// GIO portal grants remain owned by the desktop adapter; never turn the URI
/// into a filesystem path, or reopen it during cached schedule queries.
async fn read_local(
    uri: &str,
    cancellation: &GuideCancellation,
) -> Result<GuidePayload, GuideServiceError> {
    let file = gio::File::for_uri(uri);
    if file.uri_scheme().as_deref() != Some("file") {
        return Err(GuideServiceError::LocalAccess);
    }
    let stream = file
        .read_future(glib::Priority::DEFAULT)
        .await
        .map_err(|_| GuideServiceError::LocalAccess)?;
    let mut bytes = Vec::new();
    loop {
        if cancellation.is_cancelled() {
            return Err(GuideServiceError::Cancelled);
        }
        let part = stream
            .read_bytes_future(64 * 1024, glib::Priority::DEFAULT)
            .await
            .map_err(|_| GuideServiceError::LocalAccess)?;
        if part.is_empty() {
            break;
        }
        if part.len() > 16 * 1024 * 1024usize - bytes.len() {
            return Err(GuideServiceError::Fetch(GuideFetchError::Oversize));
        }
        bytes.extend_from_slice(part.as_ref());
    }
    if cancellation.is_cancelled() {
        return Err(GuideServiceError::Cancelled);
    }
    if bytes.is_empty() {
        return Err(GuideServiceError::InvalidXml);
    }
    let gzip = bytes.starts_with(&[0x1f, 0x8b]);
    Ok(GuidePayload { bytes, gzip })
}

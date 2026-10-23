// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Asynchronous, application-wide XMLTV coordinator. It never consults M3U
//! hints, stream headers, Live programmes or view repaint/focus events.
use super::{
    ingest::import_xmltv,
    model::EpgError,
    network::{self, GuideCancellation, GuideFetchError},
    sources::{
        ConfirmedGuideDeletion, GuideSource, GuideSourceRegistry, PrivateGuideSource,
        RegistryError, SourceOrigin,
    },
};
use crate::streams::storage::Storage;
use adw::{gio, glib, prelude::*};
use std::{
    collections::HashMap,
    fmt,
    io::{BufReader, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, Semaphore};

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
    scheduled: bool,
}
struct Inner {
    registry: GuideSourceRegistry,
    running: Mutex<HashMap<String, Running>>,
    /// Serializes source authorization/revocation with request admission.
    admission: Mutex<()>,
    heavy_imports: Arc<Semaphore>,
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
        let service = Self {
            inner: Arc::new(Inner {
                registry: GuideSourceRegistry::new(storage),
                running: Mutex::new(HashMap::new()),
                admission: Mutex::new(()),
                heavy_imports: Arc::new(Semaphore::new(1)),
            }),
        };
        service.start_opt_in_scheduler();
        Ok(service)
    }
    /// An application-lifetime timer runs only for sources whose user
    /// explicitly enabled automatic refresh. No daemon or startup network
    /// contact is required for disabled/non-scheduled guides.
    fn start_opt_in_scheduler(&self) {
        let weak = Arc::downgrade(&self.inner);
        // GIO's local-file futures are !Send; the scheduler belongs on the
        // GLib main context. Network, decoding and SQLite still use workers.
        crate::utils::spawn(async move {
            loop {
                glib::timeout_future_seconds(60).await;
                let Some(inner) = weak.upgrade() else { break };
                let manager = GlobalGuideManager { inner };
                if let Ok(ids) = manager.due_ids().await {
                    for id in ids {
                        let _ = manager.refresh_scheduled(id).await;
                    }
                }
            }
        });
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
        let _gate = self.inner.admission.lock().await;
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
        let _gate = self.inner.admission.lock().await;
        if !enabled {
            self.cancel_scheduled(&id).await;
        }
        self.db(move |registry| registry.set_schedule(&id, enabled, seconds))
            .await
    }
    pub async fn update_settings(
        &self,
        id: String,
        revision: i64,
        name: String,
        enabled: bool,
        auto_refresh: bool,
        seconds: i64,
    ) -> Result<(), GuideServiceError> {
        let _gate = self.inner.admission.lock().await;
        let source_id = id.clone();
        // Commit the SQLite revision-checked mutation while admission is
        // serialized. A stale/invalid action must not cancel a valid refresh.
        self.db(move |registry| {
            registry.update_settings(&id, revision, &name, enabled, auto_refresh, seconds)
        })
        .await?;
        if !enabled {
            self.cancel(&source_id).await;
        } else if !auto_refresh {
            self.cancel_scheduled(&source_id).await;
        }
        Ok(())
    }
    pub async fn delete_confirmed(
        &self,
        approval: ConfirmedGuideDeletion,
    ) -> Result<(), GuideServiceError> {
        let _gate = self.inner.admission.lock().await;
        let source_id = approval.id().to_owned();
        self.db(move |registry| registry.delete_confirmed(&approval))
            .await?;
        self.cancel(&source_id).await;
        Ok(())
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
    async fn cancel_scheduled(&self, id: &str) -> bool {
        let mut current = self.inner.running.lock().await;
        let Some(running) = current.get_mut(id) else {
            return false;
        };
        if !running.scheduled || running.stage.terminal() {
            return false;
        }
        running.parser.store(true, Ordering::Release);
        running.transfer.cancel();
        true
    }

    async fn start(
        &self,
        id: &str,
        scheduled: bool,
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
                scheduled,
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
        self.refresh_admitted(id, false).await
    }

    async fn refresh_scheduled(&self, id: String) -> Result<(), GuideServiceError> {
        self.refresh_admitted(id, true).await
    }

    async fn refresh_admitted(&self, id: String, scheduled: bool) -> Result<(), GuideServiceError> {
        // The same gate covers admission and every revocation/configuration
        // update. The transport is not permitted to race a pending disable,
        // delete, or scheduler opt-out between lookup and slot registration.
        let gate = self.inner.admission.lock().await;
        let source_id = id.clone();
        let source = self
            .db(move |registry| registry.get_private(&source_id))
            .await?;
        if !source.view.enabled {
            return Err(GuideServiceError::Disabled);
        }
        if scheduled {
            if !source.view.auto_refresh {
                return Ok(());
            }
            let due = self.due_ids().await?;
            if !due.contains(&id) {
                return Ok(());
            }
        }
        let (transfer, parser_cancel) = self.start(&id, scheduled).await?;
        drop(gate);
        self.run_registered(id, source, transfer, parser_cancel)
            .await
    }

    // Every registered slot must pass through terminal handling, including
    // cancellation before its first Fetching transition.
    async fn run_registered(
        &self,
        id: String,
        source: PrivateGuideSource,
        transfer: GuideCancellation,
        parser_cancel: Arc<AtomicBool>,
    ) -> Result<(), GuideServiceError> {
        let result = match self.transition(&id, GuideStage::Fetching).await {
            Ok(()) => {
                self.refresh_one(&id, source, &transfer, &parser_cancel)
                    .await
            }
            Err(error) => Err(error),
        };
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
        source: PrivateGuideSource,
        transfer: &GuideCancellation,
        parser_cancel: &Arc<AtomicBool>,
    ) -> Result<(), GuideServiceError> {
        // Heavy EPG imports cannot multiply to exhaust the application's RAM.
        // Deny rather than creating an unbounded pending-job queue.
        let _heavy = self
            .inner
            .heavy_imports
            .clone()
            .try_acquire_owned()
            .map_err(|_| GuideServiceError::AlreadyRunning)?;
        let path = self.inner.registry.spool_path();
        struct RemoveSpool(std::path::PathBuf);
        impl Drop for RemoveSpool {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = RemoveSpool(path.clone());
        let expected_revision = source.view.updated_at;
        let gzip = match source.origin {
            SourceOrigin::RemoteUrl { locator, trust } => {
                let cancellation = transfer.clone();
                let target = path.clone();
                crate::utils::tokio(async move {
                    network::fetch_guide_spooled(&locator, trust, &cancellation, &target).await
                })
                .await?
            }
            SourceOrigin::LocalFile(locator) => {
                read_local_spooled(&locator, transfer, &path).await?
            }
        };
        self.transition(id, GuideStage::Decoding).await?;
        // Timestamp reflects completed retrieval, never a fabricated future.
        let timestamp = clock();
        self.transition(id, GuideStage::Validating).await?;
        self.transition(id, GuideStage::Committing).await?;
        let store = self.inner.registry.clone();
        let source_id = id.to_owned();
        let cancelled = parser_cancel.clone();
        crate::utils::tokio(async move {
            tokio::task::spawn_blocking(move || {
                let file =
                    std::fs::File::open(&path).map_err(|_| GuideServiceError::LocalAccess)?;
                let mut conn = store
                    .connect_private()
                    .map_err(|_| GuideServiceError::Storage)?;
                import_xmltv(
                    &mut conn,
                    BufReader::new(file),
                    &source_id,
                    timestamp,
                    expected_revision,
                    gzip,
                    &cancelled,
                )
                .map_err(|error| match error {
                    EpgError::Cancelled => GuideServiceError::Cancelled,
                    EpgError::OverLimit => GuideServiceError::Fetch(GuideFetchError::Oversize),
                    EpgError::Storage => GuideServiceError::Storage,
                    _ => GuideServiceError::InvalidXml,
                })?;
                Ok(())
            })
            .await
            .map_err(|_| GuideServiceError::WorkerFailed)?
        })
        .await
    }
}

/// A GIO adapter may not be able to interrupt an OS syscall. This deadline
/// bounds the awaiting refresh, not a guarantee about kernel I/O termination.
async fn local_deadline<T>(
    pending: impl std::future::Future<Output = Result<T, GuideServiceError>>,
    cancellation: &GuideCancellation,
    seconds: u32,
) -> Result<T, GuideServiceError> {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(GuideServiceError::Cancelled),
        _ = glib::timeout_future_seconds(seconds) =>
            Err(GuideServiceError::Fetch(GuideFetchError::Timeout)),
        result = pending => result,
    }
}

/// GIO portal grants remain owned by the desktop adapter. A 64 KiB
/// streaming copy avoids resident-memory growth on plain multi-GB guides.
async fn read_local_spooled(
    uri: &str,
    cancellation: &GuideCancellation,
    target: &Path,
) -> Result<bool, GuideServiceError> {
    local_deadline(
        read_local_spooled_inner(uri, cancellation, target),
        cancellation,
        90 * 60,
    )
    .await
}
async fn read_local_spooled_inner(
    uri: &str,
    cancellation: &GuideCancellation,
    target: &Path,
) -> Result<bool, GuideServiceError> {
    let file = gio::File::for_uri(uri);
    if file.uri_scheme().as_deref() != Some("file") {
        return Err(GuideServiceError::LocalAccess);
    }
    let stream = local_deadline(
        async {
            file.read_future(glib::Priority::DEFAULT)
                .await
                .map_err(|_| GuideServiceError::LocalAccess)
        },
        cancellation,
        30,
    )
    .await?;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|_| GuideServiceError::LocalAccess)?;
    let mut total = 0_u64;
    let mut magic = [0_u8; 2];
    let mut prefix = 0_usize;
    let mut gzip = false;
    loop {
        if cancellation.is_cancelled() {
            return Err(GuideServiceError::Cancelled);
        }
        let part = local_deadline(
            async {
                stream
                    .read_bytes_future(64 * 1024, glib::Priority::DEFAULT)
                    .await
                    .map_err(|_| GuideServiceError::LocalAccess)
            },
            cancellation,
            30,
        )
        .await?;
        if part.is_empty() {
            break;
        }
        for &b in part.as_ref().iter().take(2_usize.saturating_sub(prefix)) {
            magic[prefix] = b;
            prefix += 1;
        }
        if prefix == 2 && magic == [0x1f, 0x8b] {
            gzip = true;
        }
        total = total
            .checked_add(part.len() as u64)
            .ok_or(GuideServiceError::Fetch(GuideFetchError::Oversize))?;
        let limit = if gzip {
            256_u64 * 1024 * 1024
        } else {
            2_u64 * 1024 * 1024 * 1024
        };
        if total > limit {
            return Err(GuideServiceError::Fetch(GuideFetchError::Oversize));
        }
        out.write_all(part.as_ref())
            .map_err(|_| GuideServiceError::LocalAccess)?;
    }
    if total == 0 {
        return Err(GuideServiceError::InvalidXml);
    }
    out.flush().map_err(|_| GuideServiceError::LocalAccess)?;
    Ok(gzip)
}

#[cfg(test)]
mod e2_admission_tests {
    use super::*;
    use crate::epg::sources::DEFAULT_REFRESH_SECONDS;
    use std::{fs, path::PathBuf};
    use uuid::Uuid;

    fn manager() -> (PathBuf, GlobalGuideManager) {
        let dir = std::env::temp_dir().join(format!("epg-admission-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let store = Storage::at(dir.join("db.sqlite3"));
        store.initialize().unwrap();
        let manager = GlobalGuideManager {
            inner: Arc::new(Inner {
                registry: GuideSourceRegistry::new(store),
                running: Mutex::new(HashMap::new()),
                admission: Mutex::new(()),
                heavy_imports: Arc::new(Semaphore::new(1)),
            }),
        };
        (dir, manager)
    }

    async fn sample(manager: &GlobalGuideManager) -> GuideSource {
        // The public trust scope rejects this loopback IP before any dial;
        // if admission is accidentally allowed, tests see a fetch error
        // rather than Disabled/NotFound or a silent network request.
        manager
            .add(
                "Race".to_owned(),
                SourceOrigin::entered_remote_url(
                    "http://127.0.0.1:9999/fabricated".to_owned(),
                    network::TrustScope::Public,
                )
                .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disable_and_delete_revoke_queued_new_admissions() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (dir, manager) = manager();
                let source = sample(&manager).await;
                let gate = manager.inner.admission.lock().await;
                let pending = manager.clone();
                let id = source.id.clone();
                let disable =
                    tokio::task::spawn_local(async move { pending.set_enabled(id, false).await });
                tokio::task::yield_now().await;
                let pending = manager.clone();
                let id = source.id.clone();
                let refresh = tokio::task::spawn_local(async move { pending.refresh(id).await });
                drop(gate);
                disable.await.unwrap().unwrap();
                assert!(matches!(
                    refresh.await.unwrap(),
                    Err(GuideServiceError::Disabled)
                ));
                assert_eq!(manager.stage(&source.id).await, GuideStage::Idle);

                manager.set_enabled(source.id.clone(), true).await.unwrap();
                let fresh = manager
                    .list()
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|s| s.id == source.id)
                    .unwrap();
                let approval = ConfirmedGuideDeletion::after_user_confirmation(&fresh);
                let gate = manager.inner.admission.lock().await;
                let pending = manager.clone();
                let deletion =
                    tokio::task::spawn_local(
                        async move { pending.delete_confirmed(approval).await },
                    );
                tokio::task::yield_now().await;
                let pending = manager.clone();
                let id = source.id.clone();
                let refresh = tokio::task::spawn_local(async move { pending.refresh(id).await });
                drop(gate);
                deletion.await.unwrap().unwrap();
                assert!(matches!(
                    refresh.await.unwrap(),
                    Err(GuideServiceError::Source(RegistryError::NotFound))
                ));
                assert_eq!(manager.stage(&source.id).await, GuideStage::Idle);
                fs::remove_dir_all(dir).unwrap();
            })
            .await;
    }

    #[tokio::test]
    async fn stale_settings_save_preserves_in_flight_refresh_until_valid_disable() {
        let (dir, manager) = manager();
        let source = sample(&manager).await;
        let (transfer, parser) = manager.start(&source.id, false).await.unwrap();
        manager
            .transition(&source.id, GuideStage::Fetching)
            .await
            .unwrap();
        // The revision changes after the settings dialog was opened.
        manager
            .set_schedule(source.id.clone(), true, DEFAULT_REFRESH_SECONDS)
            .await
            .unwrap();
        let current = manager.inner.registry.get(&source.id).unwrap();
        assert!(current.updated_at > source.updated_at);
        assert!(matches!(
            manager
                .update_settings(
                    source.id.clone(),
                    source.updated_at,
                    "Stale".into(),
                    false,
                    false,
                    DEFAULT_REFRESH_SECONDS,
                )
                .await,
            Err(GuideServiceError::Source(RegistryError::StaleConfirmation))
        ));
        assert!(!transfer.is_cancelled());
        assert!(!parser.load(Ordering::Acquire));
        assert_eq!(manager.stage(&source.id).await, GuideStage::Fetching);
        assert!(manager.inner.registry.get(&source.id).unwrap().enabled);

        manager
            .update_settings(
                source.id.clone(),
                current.updated_at,
                "Valid".into(),
                false,
                false,
                DEFAULT_REFRESH_SECONDS,
            )
            .await
            .unwrap();
        assert!(transfer.is_cancelled());
        assert!(parser.load(Ordering::Acquire));
        assert!(!manager.inner.registry.get(&source.id).unwrap().enabled);
        assert!(matches!(
            manager.start(&source.id, false).await,
            Err(GuideServiceError::AlreadyRunning)
        ));
        manager
            .terminal(&source.id, &Err(GuideServiceError::Cancelled))
            .await;
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn stale_confirmed_deletion_preserves_in_flight_refresh() {
        let (dir, manager) = manager();
        let source = sample(&manager).await;
        let stale = ConfirmedGuideDeletion::after_user_confirmation(&source);
        let (transfer, parser) = manager.start(&source.id, false).await.unwrap();
        manager
            .transition(&source.id, GuideStage::Fetching)
            .await
            .unwrap();
        manager
            .set_schedule(source.id.clone(), true, DEFAULT_REFRESH_SECONDS)
            .await
            .unwrap();
        let current = manager.inner.registry.get(&source.id).unwrap();
        assert!(matches!(
            manager.delete_confirmed(stale).await,
            Err(GuideServiceError::Source(RegistryError::StaleConfirmation))
        ));
        assert!(!transfer.is_cancelled());
        assert!(!parser.load(Ordering::Acquire));
        assert_eq!(manager.stage(&source.id).await, GuideStage::Fetching);
        assert_eq!(
            manager.inner.registry.get(&source.id).unwrap().id,
            source.id
        );

        manager
            .delete_confirmed(ConfirmedGuideDeletion::after_user_confirmation(&current))
            .await
            .unwrap();
        assert!(transfer.is_cancelled());
        assert!(parser.load(Ordering::Acquire));
        assert!(matches!(
            manager.inner.registry.get(&source.id),
            Err(RegistryError::NotFound)
        ));
        manager
            .terminal(&source.id, &Err(GuideServiceError::Cancelled))
            .await;
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn cancellation_in_queued_slot_becomes_terminal_and_allows_new_refresh() {
        let (dir, manager) = manager();
        let source = sample(&manager).await;
        let id = source.id.clone();
        let private = manager.inner.registry.get_private(&id).unwrap();
        let (transfer, parser) = manager.start(&id, false).await.unwrap();
        assert_eq!(manager.stage(&id).await, GuideStage::Queued);
        assert!(manager.cancel(&id).await);
        assert!(matches!(
            manager.start(&id, false).await,
            Err(GuideServiceError::AlreadyRunning)
        ));
        assert_eq!(
            manager
                .run_registered(id.clone(), private, transfer, parser)
                .await,
            Err(GuideServiceError::Cancelled)
        );
        assert_eq!(manager.stage(&id).await, GuideStage::Cancelled);
        // A later authorized manual refresh must reach its real fetch boundary,
        // not be rejected as AlreadyRunning by an abandoned queued slot.
        assert!(matches!(
            manager.refresh(id.clone()).await,
            Err(GuideServiceError::Fetch(
                GuideFetchError::NetworkScopeDenied
            ))
        ));
        assert_eq!(manager.stage(&id).await, GuideStage::Failed);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn real_manager_preserves_last_good_after_bad_gzip_ratio_and_wire_quota() {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let (dir, manager) = manager();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let xml = b"<tv><channel id='f'><display-name>Fiction</display-name></channel>\
            <programme channel='f' start='20300101000000 +0000' stop='20300101010000 +0000'>\
            <title>Original</title></programme></tv>"
            .to_vec();
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&xml).unwrap();
        let mut broken = gzip.finish().unwrap();
        broken.truncate(3);
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&vec![b' '; 1_000_000]).unwrap();
        let ratio_bomb = gzip.finish().unwrap();
        let replies = vec![
            (xml, false, false),
            (broken, true, false),
            (ratio_bomb, true, false),
            (Vec::new(), false, true),
        ];
        let server = tokio::spawn(async move {
            for (body, gzip, declared_oversize) in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut read = [0u8; 2048];
                let _ = socket.read(&mut read).await.unwrap();
                let mut reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
                    if declared_oversize {
                        2_usize * 1024 * 1024 * 1024 + 1
                    } else {
                        body.len()
                    },
                    if gzip {
                        "Content-Encoding: gzip\r\n"
                    } else {
                        ""
                    }
                )
                .into_bytes();
                reply.extend(body);
                socket.write_all(&reply).await.unwrap();
            }
        });
        let source = manager
            .add(
                "Fixture".into(),
                SourceOrigin::entered_remote_url(
                    format!("http://127.0.0.1:{port}/guide"),
                    network::TrustScope::ExplicitLan,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        manager.refresh(source.id.clone()).await.unwrap();
        let good = manager.list().await.unwrap().pop().unwrap();
        assert!(good.last_success_at.is_some());
        for _ in 0..2 {
            assert!(manager.refresh(source.id.clone()).await.is_err());
            let after = manager.list().await.unwrap().pop().unwrap();
            assert_eq!(after.last_success_at, good.last_success_at);
            assert!(after.last_error_code.is_some());
        }
        assert!(matches!(
            manager.refresh(source.id.clone()).await,
            Err(GuideServiceError::Fetch(GuideFetchError::Oversize))
        ));
        let after = manager.list().await.unwrap().pop().unwrap();
        assert_eq!(after.last_success_at, good.last_success_at);
        let conn = manager.inner.registry.connect_private().unwrap();
        let name: String = conn
            .query_row(
                "SELECT title FROM epg_programmes WHERE source_id=?1",
                [&source.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(name, "Original");
        server.await.unwrap();
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stalled_local_adapter_times_out_and_pre_cancelled_adapter_aborts() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let token = GuideCancellation::new();
                    let stalled = std::future::pending::<Result<(), GuideServiceError>>();
                    let result = local_deadline(stalled, &token, 1).await;
                    assert!(matches!(
                        result,
                        Err(GuideServiceError::Fetch(GuideFetchError::Timeout))
                    ));
                    token.cancel();
                    let stalled = std::future::pending::<Result<(), GuideServiceError>>();
                    assert!(matches!(
                        local_deadline(stalled, &token, 1).await,
                        Err(GuideServiceError::Cancelled)
                    ));
                })
            })
            .unwrap();
    }

    #[tokio::test]
    async fn stale_due_list_cannot_fetch_after_schedule_opt_out() {
        let (dir, manager) = manager();
        let source = sample(&manager).await;
        manager
            .set_schedule(source.id.clone(), true, 86_400)
            .await
            .unwrap();
        assert_eq!(manager.due_ids().await.unwrap(), vec![source.id.clone()]);
        manager
            .set_schedule(source.id.clone(), false, 86_400)
            .await
            .unwrap();
        assert!(manager.refresh_scheduled(source.id.clone()).await.is_ok());
        assert_eq!(manager.stage(&source.id).await, GuideStage::Idle);
        // Manual refresh remains admitted despite schedule opt-out, and
        // rejects loopback in public scope before attempting any connection.
        assert!(matches!(
            manager.refresh(source.id.clone()).await,
            Err(GuideServiceError::Fetch(
                GuideFetchError::NetworkScopeDenied
            ))
        ));
        fs::remove_dir_all(dir).unwrap();
    }
}

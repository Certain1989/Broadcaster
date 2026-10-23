// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Asynchronous, application-wide XMLTV coordinator. It never consults M3U
//! hints, stream headers, Live programmes or view repaint/focus events.
use super::{
    cache::replace_guide_snapshot_if_current,
    model::EpgError,
    network::{self, GuideCancellation, GuideFetchError, GuidePayload},
    sources::{
        ConfirmedGuideDeletion, GuideSource, GuideSourceRegistry, PrivateGuideSource,
        RegistryError, SourceOrigin,
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
    scheduled: bool,
}
struct Inner {
    registry: GuideSourceRegistry,
    running: Mutex<HashMap<String, Running>>,
    /// Serializes source authorization/revocation with request admission.
    admission: Mutex<()>,
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
        if !enabled {
            self.cancel(&id).await;
        } else if !auto_refresh {
            self.cancel_scheduled(&id).await;
        }
        self.db(move |registry| {
            registry.update_settings(&id, revision, &name, enabled, auto_refresh, seconds)
        })
        .await
    }
    pub async fn delete_confirmed(
        &self,
        approval: ConfirmedGuideDeletion,
    ) -> Result<(), GuideServiceError> {
        let _gate = self.inner.admission.lock().await;
        self.cancel(approval.id()).await;
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
        self.transition(&id, GuideStage::Fetching).await?;
        drop(gate);
        let result = self
            .refresh_one(&id, source, &transfer, &parser_cancel)
            .await;
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
        let expected_revision = source.view.updated_at;
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
        // Record actual wall-clock retrieval time; cache ordering is enforced
        // by the admission-time last-success CAS, not a fabricated future time.
        let timestamp = clock();
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
                replace_guide_snapshot_if_current(
                    &mut conn,
                    &snapshot,
                    &cancelled,
                    expected_revision,
                )
                .map_err(|error| {
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

/// GIO portal grants remain owned by the desktop adapter; never turn the URI
/// into a filesystem path, or reopen it during cached schedule queries.
async fn read_local(
    uri: &str,
    cancellation: &GuideCancellation,
) -> Result<GuidePayload, GuideServiceError> {
    local_deadline(read_local_bounded(uri, cancellation), cancellation, 90).await
}

async fn read_local_bounded(
    uri: &str,
    cancellation: &GuideCancellation,
) -> Result<GuidePayload, GuideServiceError> {
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
    let mut bytes = Vec::new();
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

#[cfg(test)]
mod e2_admission_tests {
    use super::*;
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

    #[tokio::test]
    async fn disable_and_delete_revoke_queued_new_admissions() {
        let (dir, manager) = manager();
        let source = sample(&manager).await;
        let gate = manager.inner.admission.lock().await;
        let pending = manager.clone();
        let id = source.id.clone();
        let disable = tokio::spawn(async move { pending.set_enabled(id, false).await });
        tokio::task::yield_now().await;
        let pending = manager.clone();
        let id = source.id.clone();
        let refresh = tokio::spawn(async move { pending.refresh(id).await });
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
        let deletion = tokio::spawn(async move { pending.delete_confirmed(approval).await });
        tokio::task::yield_now().await;
        let pending = manager.clone();
        let id = source.id.clone();
        let refresh = tokio::spawn(async move { pending.refresh(id).await });
        drop(gate);
        deletion.await.unwrap().unwrap();
        assert!(matches!(
            refresh.await.unwrap(),
            Err(GuideServiceError::Source(RegistryError::NotFound))
        ));
        assert_eq!(manager.stage(&source.id).await, GuideStage::Idle);
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
                        16 * 1024 * 1024 + 1
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

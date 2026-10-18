// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Async user-initiated IPTV import and refresh facade.
//!
//! The serialized generation check and synchronous SQLite call are one
//! critical section on a blocking worker. Before it begins cancellation wins;
//! after it begins the caller receives the authoritative DB result. Never
//! claim rollback after a committed transaction.
use std::{
    collections::HashMap,
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use adw::{gio, gtk};
use reqwest::Client;
use tokio::sync::{broadcast, Mutex};

use super::{
    fetch::{self, Cancellation, FetchError},
    model::{
        ChannelDetail, ChannelOverrides, ChannelPage, ChannelQuery, FilterDimension, FilterOptions,
        OriginKind, RefreshDecision, RefreshPreview, Source, SourceDefaults, SourceDraft, SourceId,
        SourceOrigin, ValidatedSnapshot,
    },
    parser::{parse_playlist, ParseError},
    storage::{ConfirmedSourceDeletion, ErrorCode, Storage, StorageError},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServiceError {
    Fetch(FetchError),
    Parse(ParseError),
    Storage(StorageError),
    NeedsReauthorization,
    Cancelled,
    Declined,
    WorkerFailed,
}
impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Fetch(error) => return write!(f, "{error}"),
            Self::Parse(error) => return write!(f, "{error}"),
            Self::Storage(error) => return write!(f, "{error}"),
            Self::NeedsReauthorization => "Please reselect the local playlist",
            Self::Cancelled => "Playlist operation cancelled",
            Self::Declined => "Refresh was declined",
            Self::WorkerFailed => "Playlist worker failed",
        })
    }
}
impl std::error::Error for ServiceError {}
impl From<StorageError> for ServiceError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}
impl From<FetchError> for ServiceError {
    fn from(value: FetchError) -> Self {
        if value == FetchError::Cancelled {
            Self::Cancelled
        } else {
            Self::Fetch(value)
        }
    }
}
impl From<ParseError> for ServiceError {
    fn from(value: ParseError) -> Self {
        Self::Parse(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefreshStage {
    Idle,
    Loading,
    Parsing,
    Previewing,
    AwaitingConfirmation,
    Committing,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub(crate) enum ServiceEvent {
    State {
        source_id: SourceId,
        generation: u64,
        stage: RefreshStage,
    },
    Preview {
        source_id: SourceId,
        generation: u64,
        preview: RefreshPreview,
    },
    Finished {
        source_id: SourceId,
        generation: u64,
        outcome: Result<(), ServiceError>,
    },
}

pub(crate) enum SourceSelection {
    Local(gio::File),
    Remote(String),
}

pub(crate) enum RefreshOutcome {
    Committed(Source),
    ConfirmationRequired(RefreshTicket),
}

/// Carries the exact validated candidate, fingerprint and generation across a
/// GUI confirmation. Never expose or log its internal catalogue or locator.
pub(crate) struct RefreshTicket {
    pub preview: RefreshPreview,
    pub generation: u64,
    snapshot: ValidatedSnapshot,
    replacement: Option<SourceOrigin>,
}
impl fmt::Debug for RefreshTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshTicket")
            .field("source_id", &self.preview.source_id)
            .field("generation", &self.generation)
            .field("requires_confirmation", &self.preview.requires_confirmation)
            .finish()
    }
}

struct Running {
    generation: u64,
    token: Cancellation,
    stage: RefreshStage,
}

struct Inner {
    storage: Storage,
    http: Client,
    generations: Mutex<HashMap<SourceId, Running>>,
    counter: AtomicU64,
    events: broadcast::Sender<ServiceEvent>,
}

#[derive(Clone)]
pub(crate) struct StreamsService {
    inner: Arc<Inner>,
}

impl StreamsService {
    /// Initialize/migrate SQLite off the GTK thread. Safe to invoke from a
    /// GLib future; all later repository operations use the same worker bridge.
    pub(crate) async fn open() -> Result<Self, ServiceError> {
        Self::with_storage(Storage::new()).await
    }

    /// Also used by deterministic tests with a private temporary SQLite path.
    pub(crate) async fn with_storage(storage: Storage) -> Result<Self, ServiceError> {
        let init = storage.clone();
        crate::utils::tokio(async move {
            tokio::task::spawn_blocking(move || init.initialize())
                .await
                .map_err(|_| ServiceError::WorkerFailed)?
                .map_err(ServiceError::Storage)
        })
        .await?;
        let (events, _) = broadcast::channel(128);
        Ok(Self {
            inner: Arc::new(Inner {
                storage,
                http: fetch::http_client()?,
                generations: Mutex::new(HashMap::new()),
                counter: AtomicU64::new(0),
                events,
            }),
        })
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ServiceEvent> {
        self.inner.events.subscribe()
    }

    pub(crate) async fn stage(&self, id: &str) -> RefreshStage {
        self.inner
            .generations
            .lock()
            .await
            .get(id)
            .map_or(RefreshStage::Idle, |r| r.stage)
    }

    /// Idempotent for terminal states. An outstanding confirmation is cancelled
    /// just like a running HTTP or GIO load.
    pub(crate) async fn cancel(&self, id: &str) -> bool {
        let mut map = self.inner.generations.lock().await;
        let Some(running) = map.get_mut(id) else {
            return false;
        };
        if matches!(
            running.stage,
            RefreshStage::Succeeded | RefreshStage::Failed | RefreshStage::Cancelled
        ) {
            return false;
        }
        running.token.cancel();
        running.stage = RefreshStage::Cancelled;
        let generation = running.generation;
        // Removing/incrementing isn't necessary: the cancelled token is checked
        // *under this same mutex* before entering the commit transaction.
        drop(map);
        self.event(ServiceEvent::State {
            source_id: id.to_owned(),
            generation,
            stage: RefreshStage::Cancelled,
        });
        true
    }

    async fn begin(&self, id: &str) -> (u64, Cancellation) {
        let mut map = self.inner.generations.lock().await;
        if let Some(previous) = map.get(id) {
            previous.token.cancel();
        }
        let generation = self
            .inner
            .counter
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let token = Cancellation::new();
        map.insert(
            id.to_owned(),
            Running {
                generation,
                token: token.clone(),
                stage: RefreshStage::Loading,
            },
        );
        drop(map);
        self.event(ServiceEvent::State {
            source_id: id.to_owned(),
            generation,
            stage: RefreshStage::Loading,
        });
        (generation, token)
    }

    fn event(&self, event: ServiceEvent) {
        // Broadcast has bounded capacity; a subscriber must also check the
        // source generation after lagging or reconnecting.
        let _ = self.inner.events.send(event);
    }

    async fn is_current(&self, id: &str, generation: u64) -> bool {
        self.inner
            .generations
            .lock()
            .await
            .get(id)
            .is_some_and(|r| r.generation == generation && !r.token.is_cancelled())
    }

    async fn progress(
        &self,
        id: &str,
        generation: u64,
        stage: RefreshStage,
    ) -> Result<(), ServiceError> {
        let mut map = self.inner.generations.lock().await;
        let Some(running) = map.get_mut(id) else {
            return Err(ServiceError::Cancelled);
        };
        if running.generation != generation || running.token.is_cancelled() {
            return Err(ServiceError::Cancelled);
        }
        running.stage = stage;
        drop(map);
        self.event(ServiceEvent::State {
            source_id: id.to_owned(),
            generation,
            stage,
        });
        Ok(())
    }

    async fn finish(&self, id: &str, generation: u64, result: &Result<(), ServiceError>) {
        let stage = match result {
            Ok(()) => RefreshStage::Succeeded,
            Err(ServiceError::Cancelled | ServiceError::Declined) => RefreshStage::Cancelled,
            Err(_) => RefreshStage::Failed,
        };
        // Do not overwrite a later refresh's state with a stale completion.
        if self.progress(id, generation, stage).await.is_ok() {
            self.event(ServiceEvent::Finished {
                source_id: id.to_owned(),
                generation,
                outcome: *result,
            });
        }
    }

    async fn db<T, F>(&self, operation: F) -> Result<T, ServiceError>
    where
        T: Send + 'static,
        F: FnOnce(Storage) -> Result<T, ServiceError> + Send + 'static,
    {
        let storage = self.inner.storage.clone();
        crate::utils::tokio(async move {
            tokio::task::spawn_blocking(move || operation(storage))
                .await
                .map_err(|_| ServiceError::WorkerFailed)?
        })
        .await
    }

    async fn parse(
        &self,
        id: SourceId,
        bytes: Vec<u8>,
        base: Option<String>,
    ) -> Result<ValidatedSnapshot, ServiceError> {
        crate::utils::tokio(async move {
            tokio::task::spawn_blocking(move || {
                let parsed = parse_playlist(&bytes, base.as_deref())?;
                ValidatedSnapshot::new(id, parsed.channels).map_err(ServiceError::Storage)
            })
            .await
            .map_err(|_| ServiceError::WorkerFailed)?
        })
        .await
    }

    async fn load(
        &self,
        selection: SourceSelection,
        cancellation: &Cancellation,
    ) -> Result<(Vec<u8>, Option<String>, SourceOrigin), ServiceError> {
        match selection {
            SourceSelection::Local(file) => {
                let locator = fetch::local_locator(&file)?;
                let bytes = fetch::fetch_local(&file, cancellation).await?;
                Ok((
                    bytes,
                    None,
                    SourceOrigin {
                        kind: OriginKind::LocalFile,
                        locator,
                    },
                ))
            }
            SourceSelection::Remote(locator) => {
                let origin = SourceOrigin {
                    kind: OriginKind::RemoteUrl,
                    locator,
                };
                origin.validate()?;
                let client = self.inner.http.clone();
                let token = cancellation.clone();
                let request = origin.locator.clone();
                let download = crate::utils::tokio(async move {
                    fetch::fetch_remote(&client, &request, &token).await
                })
                .await?;
                Ok((download.bytes, Some(download.final_url), origin))
            }
        }
    }

    pub(crate) async fn add_local(
        &self,
        name: &str,
        file: gio::File,
        order: u32,
        defaults: SourceDefaults,
    ) -> Result<Source, ServiceError> {
        self.add(name, SourceSelection::Local(file), order, defaults)
            .await
    }

    pub(crate) async fn add_url(
        &self,
        name: &str,
        url: String,
        order: u32,
        defaults: SourceDefaults,
    ) -> Result<Source, ServiceError> {
        self.add(name, SourceSelection::Remote(url), order, defaults)
            .await
    }

    async fn add(
        &self,
        name: &str,
        selection: SourceSelection,
        order: u32,
        defaults: SourceDefaults,
    ) -> Result<Source, ServiceError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (generation, cancellation) = self.begin(&id).await;
        let result = async {
            let (bytes, base, origin) = self.load(selection, &cancellation).await?;
            self.progress(&id, generation, RefreshStage::Parsing)
                .await?;
            let snapshot = self.parse(id.clone(), bytes, base).await?;
            let draft = SourceDraft {
                name: name.to_owned(),
                origin,
                enabled: true,
                order,
                defaults,
            };
            draft.validate()?;
            self.progress(&id, generation, RefreshStage::Committing)
                .await?;
            let registry = self.inner.clone();
            let expected_id = id.clone();
            self.db(move |store| {
                let mut map = registry.generations.blocking_lock();
                if !map
                    .get(&expected_id)
                    .is_some_and(|r| r.generation == generation && !r.token.is_cancelled())
                {
                    return Err(ServiceError::Cancelled);
                }
                // Commit barrier: lock is held until SQLite returns.
                let committed = store.create_source_with_initial_snapshot(
                    &expected_id,
                    &draft,
                    &snapshot,
                    now(),
                );
                // Preserve the authoritative result before any later
                // cancellation can acquire this same commit barrier.
                if let Some(running) = map.get_mut(&expected_id) {
                    running.stage = if committed.is_ok() {
                        RefreshStage::Succeeded
                    } else {
                        RefreshStage::Failed
                    };
                }
                committed.map_err(ServiceError::Storage)
            })
            .await
        }
        .await;
        self.finish(
            &id,
            generation,
            &result.as_ref().map(|_| ()).map_err(|e| *e),
        )
        .await;
        result
    }

    pub(crate) async fn refresh(&self, id: &str) -> Result<RefreshOutcome, ServiceError> {
        let id = id.to_owned();
        let (generation, cancellation) = self.begin(&id).await;
        let result = async {
            let key = id.clone();
            let source = self
                .db(move |store| store.get_source(&key).map_err(Into::into))
                .await?;
            let selection = match source.origin.kind {
                OriginKind::RemoteUrl => SourceSelection::Remote(source.origin.locator),
                OriginKind::LocalFile => {
                    let file = gio::File::for_uri(&source.origin.locator);
                    if fetch::local_locator(&file).is_err() {
                        return Err(ServiceError::NeedsReauthorization);
                    }
                    SourceSelection::Local(file)
                }
            };
            let (bytes, base, _) = match self.load(selection, &cancellation).await {
                Err(ServiceError::Fetch(FetchError::LocalAccess))
                    if source.origin.kind == OriginKind::LocalFile =>
                {
                    return Err(ServiceError::NeedsReauthorization);
                }
                other => other?,
            };
            self.preview(id.clone(), generation, bytes, base, None)
                .await
        }
        .await;
        if let Err(error) = &result {
            self.failed(&id, generation, *error).await;
        }
        result
    }

    /// Reselecting a revoked portal file grant or changing a configured URL
    /// uses the same preview/approval as refresh, but commits the new origin
    /// and catalogue together using S1's D1 transaction.
    pub(crate) async fn replace_origin(
        &self,
        id: &str,
        selection: SourceSelection,
    ) -> Result<RefreshOutcome, ServiceError> {
        let id = id.to_owned();
        let (generation, cancellation) = self.begin(&id).await;
        let result = async {
            let (bytes, base, origin) = self.load(selection, &cancellation).await?;
            self.preview(id.clone(), generation, bytes, base, Some(origin))
                .await
        }
        .await;
        if let Err(error) = &result {
            self.failed(&id, generation, *error).await;
        }
        result
    }

    async fn preview(
        &self,
        id: String,
        generation: u64,
        bytes: Vec<u8>,
        base: Option<String>,
        replacement: Option<SourceOrigin>,
    ) -> Result<RefreshOutcome, ServiceError> {
        self.progress(&id, generation, RefreshStage::Parsing)
            .await?;
        let snapshot = self.parse(id.clone(), bytes, base).await?;
        self.progress(&id, generation, RefreshStage::Previewing)
            .await?;
        let candidate = snapshot.clone();
        let review = self
            .db(move |store| {
                store
                    .preview_refresh(&candidate)
                    .map_err(ServiceError::Storage)
            })
            .await?;
        self.progress(&id, generation, RefreshStage::Previewing)
            .await?;
        self.event(ServiceEvent::Preview {
            source_id: id.clone(),
            generation,
            preview: review.clone(),
        });
        let ticket = RefreshTicket {
            preview: review,
            generation,
            snapshot,
            replacement,
        };
        if ticket.preview.requires_confirmation {
            self.progress(&id, generation, RefreshStage::AwaitingConfirmation)
                .await?;
            Ok(RefreshOutcome::ConfirmationRequired(ticket))
        } else {
            let source = self.confirm_refresh(ticket, true).await?;
            Ok(RefreshOutcome::Committed(source))
        }
    }

    /// Only invoke with `approved=true` as the direct consequence of a
    /// positive user action for THIS preview; never persist an approval flag.
    pub(crate) async fn confirm_refresh(
        &self,
        ticket: RefreshTicket,
        approved: bool,
    ) -> Result<Source, ServiceError> {
        let id = ticket.preview.source_id.clone();
        let generation = ticket.generation;
        if !approved {
            self.finish(&id, generation, &Err(ServiceError::Declined))
                .await;
            return Err(ServiceError::Declined);
        }
        self.progress(&id, generation, RefreshStage::Committing)
            .await?;
        let registry = self.inner.clone();
        let preview = ticket.preview;
        let snapshot = ticket.snapshot;
        let replacement = ticket.replacement;
        let commit_id = id.clone();
        let result = self
            .db(move |store| {
                let mut map = registry.generations.blocking_lock();
                if !map
                    .get(&commit_id)
                    .is_some_and(|r| r.generation == generation && !r.token.is_cancelled())
                {
                    return Err(ServiceError::Cancelled);
                }
                // The generation/cancel mutex is the linearization barrier.
                // A new refresh/cancel cannot supersede an already entered write.
                let decision = if preview.requires_confirmation {
                    preview.explicit_approval()
                } else {
                    RefreshDecision::NotRequired
                };
                let committed = if let Some(origin) = replacement {
                    store.replace_source_origin_and_snapshot(
                        &snapshot,
                        &preview,
                        &decision,
                        origin,
                        now(),
                    )
                } else {
                    store.commit_refresh(&snapshot, &preview, &decision, now())
                };
                // Set terminal state while the commit barrier remains held: a
                // cancellation arriving *after* commit cannot mark it rolled back.
                if let Some(running) = map.get_mut(&commit_id) {
                    running.stage = if committed.is_ok() {
                        RefreshStage::Succeeded
                    } else {
                        RefreshStage::Failed
                    };
                }
                committed.map_err(ServiceError::Storage)
            })
            .await;
        self.finish(
            &id,
            generation,
            &result.as_ref().map(|_| ()).map_err(|e| *e),
        )
        .await;
        result
    }

    async fn failed(&self, id: &str, generation: u64, error: ServiceError) {
        if !self.is_current(id, generation).await {
            return;
        }
        if !matches!(error, ServiceError::Cancelled | ServiceError::Declined) {
            let id_owned = id.to_owned();
            let registry = self.inner.clone();
            let code = match error {
                ServiceError::NeedsReauthorization => ErrorCode::Reauthorization,
                ServiceError::Parse(_) => ErrorCode::InvalidPlaylist,
                ServiceError::Fetch(FetchError::LocalAccess) => ErrorCode::Permission,
                ServiceError::Storage(_) | ServiceError::WorkerFailed => ErrorCode::Write,
                _ => ErrorCode::Network,
            };
            let _ = self
                .db(move |store| {
                    let map = registry.generations.blocking_lock();
                    if !map
                        .get(&id_owned)
                        .is_some_and(|r| r.generation == generation && !r.token.is_cancelled())
                    {
                        return Err(ServiceError::Cancelled);
                    }
                    store
                        .record_refresh_failure(&id_owned, code, now())
                        .map_err(Into::into)
                })
                .await;
        }
        self.finish(id, generation, &Err(error)).await;
    }

    // Source CRUD and query facades always use an off-main worker.
    pub(crate) async fn list_sources(&self) -> Result<Vec<Source>, ServiceError> {
        self.db(|s| s.list_sources().map_err(Into::into)).await
    }
    pub(crate) async fn source(&self, id: &str) -> Result<Source, ServiceError> {
        let id = id.to_owned();
        self.db(move |s| s.get_source(&id).map_err(Into::into))
            .await
    }
    pub(crate) async fn rename_source(&self, id: &str, name: &str) -> Result<(), ServiceError> {
        let (id, name) = (id.to_owned(), name.to_owned());
        self.db(move |s| s.rename_source(&id, &name, now()).map_err(Into::into))
            .await
    }
    pub(crate) async fn toggle_source(&self, id: &str, enabled: bool) -> Result<(), ServiceError> {
        let id = id.to_owned();
        self.db(move |s| {
            s.set_source_enabled(&id, enabled, now())
                .map_err(Into::into)
        })
        .await
    }
    pub(crate) async fn reorder_source(&self, id: &str, order: u32) -> Result<(), ServiceError> {
        let id = id.to_owned();
        self.db(move |s| s.reorder_source(&id, order, now()).map_err(Into::into))
            .await
    }
    pub(crate) async fn delete_source(
        &self,
        confirmed: ConfirmedSourceDeletion,
    ) -> Result<(), ServiceError> {
        self.db(move |s| s.delete_source_confirmed(&confirmed).map_err(Into::into))
            .await
    }
    pub(crate) async fn set_source_defaults(
        &self,
        id: &str,
        defaults: SourceDefaults,
    ) -> Result<(), ServiceError> {
        let id = id.to_owned();
        self.db(move |s| {
            s.set_source_defaults(&id, &defaults, now())
                .map_err(Into::into)
        })
        .await
    }
    pub(crate) async fn query(&self, query: ChannelQuery) -> Result<ChannelPage, ServiceError> {
        self.db(move |s| s.query_channels(&query).map_err(Into::into))
            .await
    }
    pub(crate) async fn filter_options(
        &self,
        query: ChannelQuery,
        dimension: FilterDimension,
    ) -> Result<FilterOptions, ServiceError> {
        self.db(move |s| {
            s.query_filter_options(&query, dimension)
                .map_err(Into::into)
        })
        .await
    }
    pub(crate) async fn channel_detail(&self, id: &str) -> Result<ChannelDetail, ServiceError> {
        let id = id.to_owned();
        self.db(move |s| s.get_channel_detail(&id).map_err(Into::into))
            .await
    }
    pub(crate) async fn set_overrides(
        &self,
        id: &str,
        overrides: ChannelOverrides,
    ) -> Result<(), ServiceError> {
        let id = id.to_owned();
        self.db(move |s| {
            s.set_channel_overrides(&id, &overrides, now())
                .map_err(Into::into)
        })
        .await
    }
    pub(crate) async fn clear_overrides(&self, id: &str) -> Result<(), ServiceError> {
        let id = id.to_owned();
        self.db(move |s| s.clear_channel_overrides(&id).map_err(Into::into))
            .await
    }
}

/// The only GTK dependency at this stage: request a single document through
/// GtkFileDialog's portal-aware selection rather than broad filesystem grants.
/// Returning None means the chooser was closed or selection failed.
pub(crate) async fn choose_local_file(parent: &gtk::Window) -> Option<gio::File> {
    gtk::FileDialog::new().open_future(Some(parent)).await.ok()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

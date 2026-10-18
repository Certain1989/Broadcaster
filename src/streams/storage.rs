// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SQLite repository. All methods are synchronous; only call them on a blocking worker.
//! Async consumers must use tokio::task::spawn_blocking (never GTK MainContext).
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use super::{logo, model::*};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};

pub const APPLICATION_ID: i64 = 0x42524354;
pub const SCHEMA_VERSION: i64 = 1;
pub const MIGRATION_001: &str = include_str!("migrations/001_initial.sql");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageError {
    InvalidInput,
    AlreadyExists,
    NotFound,
    StaleRevision,
    SnapshotMismatch,
    ApprovalRequired,
    Declined,
    IncompatibleDatabase,
    CorruptDatabase,
    PermissionDenied,
    StorageFailure,
}
impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately never forward SQLite error messages, statement values, DB paths,
        // or URLs to UI, logs, tracing, or third-party error chains.
        f.write_str(match self {
            Self::InvalidInput => "Invalid imported channel metadata",
            Self::AlreadyExists => "A source with this name already exists",
            Self::NotFound => "The requested source or channel no longer exists",
            Self::StaleRevision => "The source changed; refresh the preview",
            Self::SnapshotMismatch => "Refresh preview no longer matches the candidate",
            Self::ApprovalRequired => "Explicit confirmation is required",
            Self::Declined => "Operation was declined",
            Self::IncompatibleDatabase => "Unsupported IPTV database version",
            Self::CorruptDatabase => "IPTV database integrity could not be verified",
            Self::PermissionDenied => "Private IPTV storage permissions are unsafe",
            Self::StorageFailure => "IPTV storage operation failed",
        })
    }
}
impl std::error::Error for StorageError {}
pub type StorageResult<T> = Result<T, StorageError>;

pub(crate) fn map_sql(err: rusqlite::Error) -> StorageError {
    if let rusqlite::Error::SqliteFailure(code, _) = &err {
        if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            || code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
        {
            return StorageError::AlreadyExists;
        }
    }
    StorageError::StorageFailure
}

#[derive(Clone)]
pub struct Storage {
    path: PathBuf,
}
impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage")
            .field("path", &"<redacted>")
            .finish()
    }
}
impl Storage {
    pub fn new() -> Self {
        Self {
            path: adw::glib::user_data_dir()
                .join("broadcaster")
                .join("broadcaster-iptv.sqlite3"),
        }
    }
    /// Worker/test-only explicit location. Call from a blocking task.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn initialize(&self) -> StorageResult<()> {
        let path = &self.path;
        ensure_private_parent(path)?;
        let exists = path.exists();
        if exists {
            require_private_file(path)?;
            // Check identity in READ ONLY mode before any PRAGMA which might write
            // journal state. Never mutate foreign databases or future versions.
            let probe = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| StorageError::IncompatibleDatabase)?;
            let id: i64 = probe
                .query_row("PRAGMA application_id", [], |r| r.get(0))
                .map_err(|_| StorageError::CorruptDatabase)?;
            let version: i64 = probe
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .map_err(|_| StorageError::CorruptDatabase)?;
            if id != APPLICATION_ID || version != SCHEMA_VERSION {
                return Err(StorageError::IncompatibleDatabase);
            }
            validate_v1_schema_shape(&probe)?;
            validate_integrity(&probe)?;
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)
                    .map_err(|_| StorageError::PermissionDenied)?;
            }
            #[cfg(not(unix))]
            {
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|_| StorageError::PermissionDenied)?;
            }
            let conn = Connection::open(path).map_err(|_| StorageError::StorageFailure)?;
            configure(&conn)?;
            // An existing empty file is not automatically considered a fresh DB;
            // this branch runs only for a new file we just created.
            conn.execute_batch(MIGRATION_001).map_err(|_| {
                let _ = conn.execute_batch("ROLLBACK");
                StorageError::StorageFailure
            })?;
            validate_v1_schema_shape(&conn)?;
            validate_integrity(&conn)?;
        }
        Ok(())
    }

    /// Does not initialize or create a missing database: callers must initialize
    /// from a blocking worker first.
    pub(crate) fn connect(&self) -> StorageResult<Connection> {
        require_private_file(&self.path)?;
        let conn = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| StorageError::StorageFailure)?;
        configure(&conn)?;
        let id: i64 = conn
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .map_err(map_sql)?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(map_sql)?;
        if id != APPLICATION_ID || version != SCHEMA_VERSION {
            return Err(StorageError::IncompatibleDatabase);
        }
        validate_v1_schema_shape(&conn)?;
        Ok(conn)
    }

    pub fn list_sources(&self) -> StorageResult<Vec<Source>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare("SELECT id FROM iptv_sources ORDER BY source_order, id")
            .map_err(map_sql)?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql)?;
        ids.iter().map(|id| load_source(&conn, id)).collect()
    }

    /// Hash only URLs from current imported channel snapshots. Disabled
    /// sources still count: hiding one must never erase its offline logos.
    /// Do not include retained missing/tombstone entries. This runs on a
    /// SQLite worker and NEVER returns private URLs or tokens to the UI.
    pub(crate) fn referenced_logo_cache_names(&self) -> StorageResult<HashSet<String>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT logo_url FROM iptv_channels
                 WHERE is_present=1 AND logo_url IS NOT NULL AND logo_url != ''",
            )
            .map_err(map_sql)?;
        let mut names = HashSet::new();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_sql)?;
        for row in rows {
            names.insert(logo::cache_name(&row.map_err(map_sql)?));
        }
        Ok(names)
    }

    /// Counts include cached missing rows, while present counts only the active snapshot.
    pub fn source_counts(&self, id: &str) -> StorageResult<(u64, u64)> {
        let conn = self.connect()?;
        let (present, retained): (i64, i64) = conn
            .query_row(
                "SELECT COALESCE(SUM(is_present), 0), COUNT(*) FROM iptv_channels WHERE source_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(map_sql)?;
        Ok((present as u64, retained as u64))
    }

    /// Move exactly one source relative to its current neighbour. The complete
    /// order is normalized atomically, so duplicate or adjacent prior order
    /// values can never make an Up/Down control silently do nothing.
    pub fn move_source(&self, id: &str, direction: i32, at: i64) -> StorageResult<()> {
        if direction != -1 && direction != 1 {
            return Err(StorageError::InvalidInput);
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        let mut stmt = tx
            .prepare("SELECT id FROM iptv_sources ORDER BY source_order, id")
            .map_err(map_sql)?;
        let mut ids = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql)?;
        drop(stmt);
        let old = ids
            .iter()
            .position(|item| item == id)
            .ok_or(StorageError::NotFound)?;
        let target = if direction < 0 {
            old.checked_sub(1)
        } else {
            old.checked_add(1).filter(|n| *n < ids.len())
        };
        if let Some(target) = target {
            ids.swap(old, target);
        }
        for (ordinal, source_id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE iptv_sources SET source_order=?1, revision=revision+1, updated_at=?2 WHERE id=?3 AND source_order != ?1",
                params![ordinal as u32, at, source_id],
            ).map_err(map_sql)?;
        }
        tx.commit().map_err(map_sql)
    }

    pub fn get_source(&self, id: &str) -> StorageResult<Source> {
        let conn = self.connect()?;
        load_source(&conn, id)
    }

    pub fn rename_source(&self, id: &str, new_name: &str, at: i64) -> StorageResult<()> {
        bounded(new_name, 128)?;
        self.update_source(
            "UPDATE iptv_sources SET name=?1, revision=revision+1,updated_at=?2 WHERE id=?3",
            params![new_name, at, id],
        )
    }

    pub fn set_source_enabled(&self, id: &str, enabled: bool, at: i64) -> StorageResult<()> {
        self.update_source(
            "UPDATE iptv_sources SET enabled=?1, revision=revision+1,updated_at=?2 WHERE id=?3",
            params![enabled as i32, at, id],
        )
    }

    pub fn reorder_source(&self, id: &str, order: u32, at: i64) -> StorageResult<()> {
        self.update_source(
            "UPDATE iptv_sources SET source_order=?1, revision=revision+1,updated_at=?2 WHERE id=?3",
            params![order, at, id],
        )
    }

    fn update_source(&self, sql: &str, args: impl rusqlite::Params) -> StorageResult<()> {
        let conn = self.connect()?;
        if conn.execute(sql, args).map_err(map_sql)? == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub fn set_source_defaults(
        &self,
        id: &str,
        defaults: &SourceDefaults,
        at: i64,
    ) -> StorageResult<()> {
        defaults.validate()?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        let changed = tx.execute(
            "UPDATE iptv_sources SET default_region_code=?1,default_region_label=?2,revision=revision+1,updated_at=?3 WHERE id=?4",
            params![defaults.region.as_ref().and_then(|r|r.code.as_ref()), defaults.region.as_ref().and_then(|r|r.label.as_ref()), at, id],
        ).map_err(map_sql)?;
        if changed == 0 {
            return Err(StorageError::NotFound);
        }
        tx.execute(
            "DELETE FROM iptv_source_default_facets WHERE source_id=?1",
            [id],
        )
        .map_err(map_sql)?;
        for (kind, set) in [
            (FacetDimension::Language, &defaults.languages),
            (FacetDimension::Country, &defaults.countries),
        ] {
            for tag in set {
                tx.execute(
                    "INSERT INTO iptv_source_default_facets (source_id,facet,value) VALUES (?1,?2,?3)",
                    params![id, kind.sql(), tag],
                ).map_err(map_sql)?;
            }
        }
        tx.commit().map_err(map_sql)
    }

    /// Token must be produced by a deliberate confirmation action in the UI.
    pub fn delete_source_confirmed(
        &self,
        confirmation: &ConfirmedSourceDeletion,
    ) -> StorageResult<()> {
        let conn = self.connect()?;
        if conn
            .execute(
                "DELETE FROM iptv_sources WHERE id=?1 AND revision=?2",
                params![confirmation.id, confirmation.revision],
            )
            .map_err(map_sql)?
            == 0
        {
            return Err(StorageError::StaleRevision);
        }
        Ok(())
    }

    pub fn set_channel_overrides(
        &self,
        id: &str,
        edits: &ChannelOverrides,
        at: i64,
    ) -> StorageResult<()> {
        edits.validate()?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM iptv_channels WHERE id=?1)",
                [id],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        if !exists {
            return Err(StorageError::NotFound);
        }
        tx.execute(
            "DELETE FROM iptv_channel_override_facets WHERE channel_id=?1",
            [id],
        )
        .map_err(map_sql)?;
        if !edits.has_values() {
            tx.execute(
                "DELETE FROM iptv_channel_overrides WHERE channel_id=?1",
                [id],
            )
            .map_err(map_sql)?;
        } else {
            tx.execute(
                "INSERT INTO iptv_channel_overrides
                (channel_id,display_name_override,group_mode,group_override,
                 language_mode,country_mode,region_mode,region_code_override,region_label_override,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(channel_id) DO UPDATE SET
                 display_name_override=excluded.display_name_override,
                 group_mode=excluded.group_mode,group_override=excluded.group_override,
                 language_mode=excluded.language_mode,country_mode=excluded.country_mode,
                 region_mode=excluded.region_mode,region_code_override=excluded.region_code_override,
                 region_label_override=excluded.region_label_override,updated_at=excluded.updated_at",
                params![
                    id, edits.display_name, edits.group.mode.sql(), edits.group.value,
                    edits.languages.mode.sql(), edits.countries.mode.sql(), edits.region.mode.sql(),
                    edits.region.value.as_ref().and_then(|r|r.code.as_ref()),
                    edits.region.value.as_ref().and_then(|r|r.label.as_ref()), at,
                ],
            ).map_err(map_sql)?;
            for (kind, field) in [
                (FacetDimension::Language, &edits.languages),
                (FacetDimension::Country, &edits.countries),
            ] {
                if field.mode == FacetMode::Replace {
                    for value in &field.values {
                        tx.execute("INSERT INTO iptv_channel_override_facets (channel_id,facet,value) VALUES(?1,?2,?3)",
                            params![id, kind.sql(), value]).map_err(map_sql)?;
                    }
                }
            }
        }
        tx.commit().map_err(map_sql)
    }

    pub fn clear_channel_overrides(&self, id: &str) -> StorageResult<()> {
        self.set_channel_overrides(id, &ChannelOverrides::default(), 0)
    }

    /// Only fixed, sanitized codes are stored; status writes do not bump the CAS revision.
    pub fn record_refresh_failure(&self, id: &str, code: ErrorCode, at: i64) -> StorageResult<()> {
        let conn = self.connect()?;
        if conn
            .execute(
                "UPDATE iptv_sources SET last_attempt_at=?1,last_error_code=?2 WHERE id=?3",
                params![at, code.sql(), id],
            )
            .map_err(map_sql)?
            == 0
        {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub fn purge_expired_missing(&self, now: i64) -> StorageResult<usize> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM iptv_channels WHERE is_present=0 AND missing_since <= ?1
                AND NOT EXISTS (SELECT 1 FROM iptv_channel_overrides o WHERE o.channel_id=iptv_channels.id)
                AND NOT EXISTS (SELECT 1 FROM iptv_channel_override_facets f WHERE f.channel_id=iptv_channels.id)",
            [now.saturating_sub(90 * 24 * 3600)],
        ).map_err(map_sql)
    }

    pub fn get_channel_detail(&self, id: &str) -> StorageResult<ChannelDetail> {
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(map_sql)?;
        let detail = load_channel_detail(&tx, id)?;
        tx.commit().map_err(map_sql)?;
        Ok(detail)
    }
}
impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

/// App-level deletion requires explicit confirmation tied to a currently displayed source.
#[derive(Clone, Debug)]
pub struct ConfirmedSourceDeletion {
    id: SourceId,
    revision: i64,
}
impl ConfirmedSourceDeletion {
    pub fn after_user_confirmation(source: &Source) -> Self {
        Self {
            id: source.id.clone(),
            revision: source.revision,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Network,
    InvalidPlaylist,
    Permission,
    Reauthorization,
    Cancelled,
    Write,
}
impl ErrorCode {
    fn sql(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::InvalidPlaylist => "invalid_playlist",
            Self::Permission => "permission",
            Self::Reauthorization => "reauthorization",
            Self::Cancelled => "cancelled",
            Self::Write => "write",
        }
    }
}

fn configure(conn: &Connection) -> StorageResult<()> {
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(map_sql)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(map_sql)?;
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .map_err(map_sql)?;
    if fk != 1 {
        return Err(StorageError::StorageFailure);
    }
    Ok(())
}
fn validate_v1_schema_shape(conn: &Connection) -> StorageResult<()> {
    const TABLES: &[&str] = &[
        "iptv_channel_imported_facets",
        "iptv_channel_override_facets",
        "iptv_channel_overrides",
        "iptv_channels",
        "iptv_source_default_facets",
        "iptv_sources",
    ];
    const INDEXES: &[&str] = &[
        "idx_iptv_channels_missing",
        "idx_iptv_channels_source_group",
        "idx_iptv_channels_source_present_order",
        "idx_iptv_channels_source_tvgid",
        "idx_iptv_imported_facet_filter",
        "idx_iptv_override_facet_filter",
        "idx_iptv_source_default_facet_filter",
        "idx_iptv_sources_enabled_order",
    ];
    for (kind, prefix, expected) in [
        ("table", "iptv_%", TABLES),
        ("index", "idx_iptv_%", INDEXES),
    ] {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type=?1 AND name LIKE ?2 ORDER BY name",
            )
            .map_err(|_| StorageError::IncompatibleDatabase)?;
        let actual = stmt
            .query_map(params![kind, prefix], |row| row.get::<_, String>(0))
            .map_err(|_| StorageError::IncompatibleDatabase)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| StorageError::IncompatibleDatabase)?;
        if !actual
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
        {
            return Err(StorageError::IncompatibleDatabase);
        }
    }
    Ok(())
}

fn validate_integrity(conn: &Connection) -> StorageResult<()> {
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|_| StorageError::CorruptDatabase)?;
    if integrity != "ok" {
        return Err(StorageError::CorruptDatabase);
    }
    let bad: Option<String> = conn
        .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
        .optional()
        .map_err(|_| StorageError::CorruptDatabase)?;
    if bad.is_some() {
        return Err(StorageError::CorruptDatabase);
    }
    Ok(())
}
fn ensure_private_parent(path: &Path) -> StorageResult<()> {
    let parent = path.parent().ok_or(StorageError::InvalidInput)?;
    if !parent.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|_| StorageError::PermissionDenied)?;
        }
        #[cfg(not(unix))]
        {
            fs::create_dir_all(parent).map_err(|_| StorageError::PermissionDenied)?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::symlink_metadata(parent).map_err(|_| StorageError::PermissionDenied)?;
        if !meta.file_type().is_dir()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != unsafe { geteuid() }
        {
            return Err(StorageError::PermissionDenied);
        }
    }
    Ok(())
}
#[cfg(unix)]
extern "C" {
    fn geteuid() -> u32;
}
fn require_private_file(path: &Path) -> StorageResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::symlink_metadata(path).map_err(|_| StorageError::PermissionDenied)?;
        if !meta.file_type().is_file()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != unsafe { geteuid() }
        {
            return Err(StorageError::PermissionDenied);
        }
    }
    #[cfg(not(unix))]
    {
        if !path.is_file() {
            return Err(StorageError::PermissionDenied);
        }
    }
    Ok(())
}

pub(crate) fn load_source(conn: &Connection, id: &str) -> StorageResult<Source> {
    let source = conn
        .query_row(
            "SELECT name,origin_kind,origin_locator,enabled,source_order,
                default_region_code,default_region_label,revision,created_at,updated_at,
                last_attempt_at,last_success_at,last_error_code FROM iptv_sources WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get::<_, u32>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                    r.get::<_, Option<i64>>(11)?,
                    r.get::<_, Option<String>>(12)?,
                ))
            },
        )
        .optional()
        .map_err(map_sql)?
        .ok_or(StorageError::NotFound)?;
    let (
        name,
        kind,
        locator,
        enabled,
        order,
        code,
        label,
        revision,
        created_at,
        updated_at,
        last_attempt_at,
        last_success_at,
        last_error_code,
    ) = source;
    Ok(Source {
        id: id.to_owned(),
        name,
        origin: SourceOrigin {
            kind: OriginKind::from_sql(&kind)?,
            locator,
        },
        enabled,
        order,
        defaults: SourceDefaults {
            languages: load_facets(
                conn,
                "iptv_source_default_facets",
                "source_id",
                id,
                FacetDimension::Language,
            )?,
            countries: load_facets(
                conn,
                "iptv_source_default_facets",
                "source_id",
                id,
                FacetDimension::Country,
            )?,
            region: if code.is_some() || label.is_some() {
                Some(Region { code, label })
            } else {
                None
            },
        },
        revision,
        created_at,
        updated_at,
        last_attempt_at,
        last_success_at,
        last_error_code,
    })
}
pub(crate) fn load_facets(
    conn: &Connection,
    table: &str,
    id_col: &str,
    id: &str,
    facet: FacetDimension,
) -> StorageResult<BTreeSet<String>> {
    let sql = format!("SELECT value FROM {table} WHERE {id_col}=?1 AND facet=?2 ORDER BY value");
    let mut stmt = conn.prepare(&sql).map_err(map_sql)?;
    let rows = stmt
        .query_map(params![id, facet.sql()], |r| r.get::<_, String>(0))
        .map_err(map_sql)?;
    rows.collect::<rusqlite::Result<BTreeSet<_>>>()
        .map_err(map_sql)
}
pub(crate) fn load_channel_detail(conn: &Connection, id: &str) -> StorageResult<ChannelDetail> {
    let raw = conn
        .query_row(
            "SELECT c.source_id,c.playlist_order,c.imported_name,c.stream_url,c.tvg_id,c.tvg_name,
                c.group_title,c.logo_url,c.channel_number,c.imported_region_code,
                c.imported_region_label,c.user_agent,c.http_referer
         FROM iptv_channels c WHERE c.id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, Option<String>>(11)?,
                    r.get::<_, Option<String>>(12)?,
                ))
            },
        )
        .optional()
        .map_err(map_sql)?
        .ok_or(StorageError::NotFound)?;
    let (
        source_id,
        order,
        imported_name,
        stream_url,
        tvg_id,
        tvg_name,
        group_title,
        logo_url,
        channel_number,
        region_code,
        region_label,
        user_agent,
        referer,
    ) = raw;
    let source = load_source(conn, &source_id)?;
    let imported = IncomingChannel {
        order,
        imported_name: imported_name.clone(),
        stream_url: stream_url.clone(),
        tvg_id,
        tvg_name,
        group_title: group_title.clone(),
        logo_url: logo_url.clone(),
        channel_number: channel_number.clone(),
        region: if region_code.is_some() || region_label.is_some() {
            Some(Region {
                code: region_code,
                label: region_label,
            })
        } else {
            None
        },
        languages: load_facets(
            conn,
            "iptv_channel_imported_facets",
            "channel_id",
            id,
            FacetDimension::Language,
        )?,
        countries: load_facets(
            conn,
            "iptv_channel_imported_facets",
            "channel_id",
            id,
            FacetDimension::Country,
        )?,
        user_agent,
        referer,
    };
    let values = conn
        .query_row(
            "SELECT display_name_override,group_mode,group_override,language_mode,country_mode,
                region_mode,region_code_override,region_label_override
         FROM iptv_channel_overrides WHERE channel_id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()
        .map_err(map_sql)?;
    let overrides = if let Some((
        display_name,
        group_mode,
        group_value,
        lang_mode,
        country_mode,
        region_mode,
        code,
        label,
    )) = values
    {
        ChannelOverrides {
            display_name,
            group: GroupOverride {
                mode: FacetMode::from_sql(&group_mode)?,
                value: group_value,
            },
            languages: SetOverride {
                mode: FacetMode::from_sql(&lang_mode)?,
                values: load_facets(
                    conn,
                    "iptv_channel_override_facets",
                    "channel_id",
                    id,
                    FacetDimension::Language,
                )?,
            },
            countries: SetOverride {
                mode: FacetMode::from_sql(&country_mode)?,
                values: load_facets(
                    conn,
                    "iptv_channel_override_facets",
                    "channel_id",
                    id,
                    FacetDimension::Country,
                )?,
            },
            region: RegionOverride {
                mode: FacetMode::from_sql(&region_mode)?,
                value: if code.is_some() || label.is_some() {
                    Some(Region { code, label })
                } else {
                    None
                },
            },
        }
    } else {
        ChannelOverrides::default()
    };

    let (visible_name, name_from) = if let Some(name) = &overrides.display_name {
        (name.clone(), FieldOrigin::Manual)
    } else {
        (imported_name, FieldOrigin::Imported)
    };
    let (group, group_from) = match overrides.group.mode {
        FacetMode::Replace => (overrides.group.value.clone(), FieldOrigin::Manual),
        FacetMode::Unknown => (None, FieldOrigin::Unknown),
        FacetMode::Auto if group_title.is_some() => (group_title, FieldOrigin::Imported),
        _ => (None, FieldOrigin::Unknown),
    };
    let (region, region_from) = match overrides.region.mode {
        FacetMode::Replace => (overrides.region.value.clone(), FieldOrigin::Manual),
        FacetMode::Unknown => (None, FieldOrigin::Unknown),
        FacetMode::Auto if imported.region.is_some() => {
            (imported.region.clone(), FieldOrigin::Imported)
        }
        FacetMode::Auto if source.defaults.region.is_some() => {
            (source.defaults.region.clone(), FieldOrigin::SourceFallback)
        }
        _ => (None, FieldOrigin::Unknown),
    };
    let (languages, languages_from) = effective_set(
        &overrides.languages,
        &imported.languages,
        &source.defaults.languages,
    );
    let (countries, countries_from) = effective_set(
        &overrides.countries,
        &imported.countries,
        &source.defaults.countries,
    );
    let effective = ChannelEffective {
        id: id.to_owned(),
        source_id,
        source_name: source.name,
        visible_name,
        stream_url,
        group,
        languages,
        countries,
        region,
        logo_url,
        channel_number,
        has_user_overrides: overrides.has_values(),
    };
    Ok(ChannelDetail {
        effective,
        imported,
        source_defaults: source.defaults,
        overrides,
        provenance: EditProvenance {
            name: name_from,
            group: group_from,
            languages: languages_from,
            countries: countries_from,
            region: region_from,
        },
    })
}
fn effective_set(
    manual: &SetOverride,
    imported: &BTreeSet<String>,
    fallback: &BTreeSet<String>,
) -> (BTreeSet<String>, FieldOrigin) {
    match manual.mode {
        FacetMode::Replace => (manual.values.clone(), FieldOrigin::Manual),
        FacetMode::Unknown => (BTreeSet::new(), FieldOrigin::Unknown),
        FacetMode::Auto if !imported.is_empty() => (imported.clone(), FieldOrigin::Imported),
        FacetMode::Auto if !fallback.is_empty() => (fallback.clone(), FieldOrigin::SourceFallback),
        _ => (BTreeSet::new(), FieldOrigin::Unknown),
    }
}

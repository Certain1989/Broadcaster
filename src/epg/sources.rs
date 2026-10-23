// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Single global SQLite-v2 XMLTV source registry. Locators never enter public
//! model, Debug, event payloads or UI status. IPTV sources are not modified.
use super::network::{validate_remote_url, TrustScope};
use crate::streams::storage::Storage;
use rusqlite::{params, OptionalExtension};
use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const DEFAULT_REFRESH_SECONDS: i64 = 86_400;
pub const MIN_REFRESH_SECONDS: i64 = 21_600;
pub const MAX_REFRESH_SECONDS: i64 = 604_800;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    InvalidInput,
    NotFound,
    AlreadyExists,
    StaleConfirmation,
    Disabled,
    Storage,
}
impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid_epg_source",
            Self::NotFound => "epg_source_not_found",
            Self::AlreadyExists => "epg_source_already_exists",
            Self::StaleConfirmation => "epg_source_changed",
            Self::Disabled => "epg_source_disabled",
            Self::Storage => "epg_storage_failed",
        })
    }
}
impl std::error::Error for RegistryError {}
fn db(_: rusqlite::Error) -> RegistryError {
    RegistryError::Storage
}
fn clock() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs() as i64)
        .unwrap_or(0)
}
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name == name.trim()
        && name.len() <= 128
        && !name.chars().any(char::is_control)
}
fn safe_code(code: &str) -> &'static str {
    match code {
        "invalid_origin" => "invalid_origin",
        "network_scope_denied" => "network_scope_denied",
        "redirect_denied" => "redirect_denied",
        "http_status" => "http_status",
        "network" => "network",
        "timeout" => "timeout",
        "oversize" => "oversize",
        "encoding" => "encoding",
        "cancelled" => "cancelled",
        "invalid_xml" => "invalid_xml",
        "storage_failed" => "storage_failed",
        "local_access" => "local_access",
        _ => "unknown",
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    LocalFile,
    RemoteUrl,
}
#[derive(Clone)]
pub struct GuideSource {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    pub trust: TrustScope,
    pub enabled: bool,
    pub auto_refresh: bool,
    pub refresh_interval_seconds: i64,
    pub updated_at: i64,
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub last_error_code: Option<&'static str>,
}
impl fmt::Debug for GuideSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuideSource")
            .field("id", &self.id)
            .field("name", &"<redacted>")
            .field("kind", &self.kind)
            .field("enabled", &self.enabled)
            .finish()
    }
}
/// Private locator descriptor: Debug deliberately cannot reveal a URL token
/// or local portal path.
#[derive(Clone)]
pub enum SourceOrigin {
    LocalFile(String),
    RemoteUrl { locator: String, trust: TrustScope },
}
impl fmt::Debug for SourceOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GuideOrigin(<redacted>)")
    }
}
impl SourceOrigin {
    pub fn selected_local_file(uri: String) -> Result<Self, RegistryError> {
        if uri.len() > 8192 || !uri.starts_with("file://") || uri.chars().any(char::is_control) {
            return Err(RegistryError::InvalidInput);
        }
        Ok(Self::LocalFile(uri))
    }
    pub fn entered_remote_url(url: String, scope: TrustScope) -> Result<Self, RegistryError> {
        validate_remote_url(&url).map_err(|_| RegistryError::InvalidInput)?;
        Ok(Self::RemoteUrl {
            locator: url,
            trust: scope,
        })
    }
    fn parts(&self) -> (&'static str, &str, &'static str) {
        match self {
            Self::LocalFile(value) => ("local_file", value, "public"),
            Self::RemoteUrl { locator, trust } => (
                "remote_url",
                locator,
                if *trust == TrustScope::ExplicitLan {
                    "explicit_lan"
                } else {
                    "public"
                },
            ),
        }
    }
}
#[derive(Clone)]
pub(crate) struct PrivateGuideSource {
    pub view: GuideSource,
    pub origin: SourceOrigin,
}
impl fmt::Debug for PrivateGuideSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateGuideSource")
            .field("id", &self.view.id)
            .field("origin", &"<redacted>")
            .finish()
    }
}
pub struct ConfirmedGuideDeletion {
    id: String,
    updated_at: i64,
}
impl ConfirmedGuideDeletion {
    /// Only construct after the owner confirms the cascade in a visible dialog.
    pub fn after_user_confirmation(source: &GuideSource) -> Self {
        Self {
            id: source.id.clone(),
            updated_at: source.updated_at,
        }
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}
#[derive(Clone)]
pub struct GuideSourceRegistry {
    storage: Storage,
}
impl fmt::Debug for GuideSourceRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GuideSourceRegistry(<private>)")
    }
}
impl GuideSourceRegistry {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }
    pub(crate) fn connect_private(&self) -> Result<rusqlite::Connection, RegistryError> {
        self.storage.connect().map_err(|_| RegistryError::Storage)
    }

    /// Call only from a blocking worker, never from GTK MainContext.
    pub fn add(&self, name: &str, origin: &SourceOrigin) -> Result<GuideSource, RegistryError> {
        if !valid_name(name) {
            return Err(RegistryError::InvalidInput);
        }
        let (kind, locator, trust) = origin.parts();
        let id = Uuid::new_v4().to_string();
        let now = clock();
        let conn = self.storage.connect().map_err(|_| RegistryError::Storage)?;
        match conn.execute(
            "INSERT INTO epg_sources
            (id,name,origin_kind,origin_locator,trust_scope,created_at,updated_at)
            VALUES (?1,?2,?3,?4,?5,?6,?6)",
            params![id, name, kind, locator, trust, now],
        ) {
            Ok(_) => self.get(&id),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
            {
                Err(RegistryError::AlreadyExists)
            }
            Err(_) => Err(RegistryError::Storage),
        }
    }

    pub fn list(&self) -> Result<Vec<GuideSource>, RegistryError> {
        let conn = self.storage.connect().map_err(|_| RegistryError::Storage)?;
        let mut stmt = conn
            .prepare(
                "SELECT id,name,origin_kind,trust_scope,
          enabled,auto_refresh,refresh_interval_seconds,updated_at,
          last_attempt_at,last_success_at,last_error_code
          FROM epg_sources ORDER BY name COLLATE NOCASE, id",
            )
            .map_err(db)?;
        let rows = stmt.query_map([], map_row).map_err(db)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db)
    }
    pub fn get(&self, id: &str) -> Result<GuideSource, RegistryError> {
        let conn = self.storage.connect().map_err(|_| RegistryError::Storage)?;
        conn.query_row(
            "SELECT id,name,origin_kind,trust_scope,
          enabled,auto_refresh,refresh_interval_seconds,updated_at,
          last_attempt_at,last_success_at,last_error_code
          FROM epg_sources WHERE id=?1",
            [id],
            map_row,
        )
        .optional()
        .map_err(db)?
        .ok_or(RegistryError::NotFound)
    }
    pub(crate) fn get_private(&self, id: &str) -> Result<PrivateGuideSource, RegistryError> {
        let conn = self.storage.connect().map_err(|_| RegistryError::Storage)?;
        let mut stmt = conn
            .prepare(
                "SELECT id,name,origin_kind,trust_scope,
          enabled,auto_refresh,refresh_interval_seconds,updated_at,
          last_attempt_at,last_success_at,last_error_code,origin_locator
          FROM epg_sources WHERE id=?1",
            )
            .map_err(db)?;
        stmt.query_row([id], |row| {
            let view = map_row(row)?;
            let locator: String = row.get(11)?;
            let origin = match view.kind {
                SourceKind::LocalFile => SourceOrigin::LocalFile(locator),
                SourceKind::RemoteUrl => SourceOrigin::RemoteUrl {
                    locator,
                    trust: view.trust,
                },
            };
            Ok(PrivateGuideSource { view, origin })
        })
        .optional()
        .map_err(db)?
        .ok_or(RegistryError::NotFound)
    }
    pub fn rename(&self, id: &str, name: &str) -> Result<(), RegistryError> {
        if !valid_name(name) {
            return Err(RegistryError::InvalidInput);
        }
        self.update(
            "UPDATE epg_sources SET name=?1,updated_at=MAX(updated_at+1,?2) WHERE id=?3",
            params![name, clock(), id],
        )
    }
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), RegistryError> {
        self.update(
            "UPDATE epg_sources SET enabled=?1,updated_at=MAX(updated_at+1,?2) WHERE id=?3",
            params![enabled as i64, clock(), id],
        )
    }
    pub fn set_schedule(&self, id: &str, enabled: bool, seconds: i64) -> Result<(), RegistryError> {
        if !(MIN_REFRESH_SECONDS..=MAX_REFRESH_SECONDS).contains(&seconds) {
            return Err(RegistryError::InvalidInput);
        }
        self.update(
            "UPDATE epg_sources SET auto_refresh=?1,
            refresh_interval_seconds=?2,updated_at=MAX(updated_at+1,?3) WHERE id=?4",
            params![enabled as i64, seconds, clock(), id],
        )
    }
    fn update(&self, sql: &str, values: impl rusqlite::Params) -> Result<(), RegistryError> {
        let conn = self.storage.connect().map_err(|_| RegistryError::Storage)?;
        if conn.execute(sql, values).map_err(db)? == 0 {
            Err(RegistryError::NotFound)
        } else {
            Ok(())
        }
    }
    pub fn delete_confirmed(&self, approval: &ConfirmedGuideDeletion) -> Result<(), RegistryError> {
        let conn = self.storage.connect().map_err(|_| RegistryError::Storage)?;
        if conn
            .execute(
                "DELETE FROM epg_sources WHERE id=?1 AND updated_at=?2",
                params![approval.id, approval.updated_at],
            )
            .map_err(db)?
            == 0
        {
            return Err(RegistryError::StaleConfirmation);
        }
        Ok(())
    }
    /// Fixed reason codes only. A failed fetch leaves last_success and guide
    /// data unchanged, including manual global guide settings and aliases.
    pub fn record_failure(&self, id: &str, code: &str) -> Result<(), RegistryError> {
        self.update(
            "UPDATE epg_sources SET last_attempt_at=?1,
             last_error_code=?2 WHERE id=?3",
            params![clock(), safe_code(code), id],
        )
    }
    /// Scheduled work is opt-in and requires an explicit caller-owned timer.
    /// Merely enumerating sources or looking up cached EPG never starts I/O.
    pub fn due_source_ids(&self, at: i64) -> Result<Vec<String>, RegistryError> {
        let conn = self.storage.connect().map_err(|_| RegistryError::Storage)?;
        let mut stmt = conn
            .prepare(
                "SELECT id FROM epg_sources
          WHERE enabled=1 AND auto_refresh=1 AND
          (last_attempt_at IS NULL OR
            last_attempt_at <= ?1 - refresh_interval_seconds)
          ORDER BY id",
            )
            .map_err(db)?;
        stmt.query_map([at], |row| row.get(0))
            .map_err(db)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db)
    }
}
fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GuideSource> {
    let kind: String = row.get(2)?;
    let scope: String = row.get(3)?;
    let error: Option<String> = row.get(10)?;
    Ok(GuideSource {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: if kind == "local_file" {
            SourceKind::LocalFile
        } else {
            SourceKind::RemoteUrl
        },
        trust: if scope == "explicit_lan" {
            TrustScope::ExplicitLan
        } else {
            TrustScope::Public
        },
        enabled: row.get::<_, i64>(4)? != 0,
        auto_refresh: row.get::<_, i64>(5)? != 0,
        refresh_interval_seconds: row.get(6)?,
        updated_at: row.get(7)?,
        last_attempt_at: row.get(8)?,
        last_success_at: row.get(9)?,
        last_error_code: error.as_deref().map(safe_code),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};
    fn temp_store() -> (PathBuf, GuideSourceRegistry) {
        let dir = std::env::temp_dir().join(format!("epg-source-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let db = Storage::at(dir.join("db.sqlite3"));
        db.initialize().unwrap();
        (dir, GuideSourceRegistry::new(db))
    }
    #[test]
    fn global_sources_are_opt_in_and_locators_never_leave_public_records() {
        let (dir, store) = temp_store();
        let origin = SourceOrigin::entered_remote_url(
            "https://example.com/guide?token=hidden".into(),
            TrustScope::Public,
        )
        .unwrap();
        let source = store.add("Guide A", &origin).unwrap();
        assert!(!source.auto_refresh);
        assert!(!format!("{source:?}").contains("token=hidden"));
        assert!(!format!("{origin:?}").contains("token=hidden"));
        assert_eq!(store.list().unwrap().len(), 1);
        store.set_enabled(&source.id, false).unwrap();
        assert!(!store.get(&source.id).unwrap().enabled);
        store
            .record_failure(&source.id, "https://somewhere/token")
            .unwrap();
        assert_eq!(
            store.get(&source.id).unwrap().last_error_code,
            Some("unknown")
        );
        let approval =
            ConfirmedGuideDeletion::after_user_confirmation(&store.get(&source.id).unwrap());
        store.delete_confirmed(&approval).unwrap();
        assert!(store.list().unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn only_conservative_opt_in_refresh_schedule_is_accepted() {
        let (dir, store) = temp_store();
        let source = store
            .add(
                "One",
                &SourceOrigin::entered_remote_url(
                    "https://example.com/xmltv".into(),
                    TrustScope::Public,
                )
                .unwrap(),
            )
            .unwrap();
        assert!(store.set_schedule(&source.id, true, 3600).is_err());
        store.set_schedule(&source.id, true, 86400).unwrap();
        assert_eq!(store.due_source_ids(clock()).unwrap(), vec![source.id]);
        fs::remove_dir_all(dir).unwrap();
    }
}

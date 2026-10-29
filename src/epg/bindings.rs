// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SQLite-v2 EPG preferences and read-only semantic projections.
//! Invoke on an existing validated private SQLite connection from a BLOCKING worker.
//! No network calls, GTK, stream URLs, stream headers, or source refresh side effects.
use super::{
    cache::{read_guide_window, BoundedGuidePage, Coverage, Freshness},
    matching::{
        choose_source, match_in_source, GuideAvailability, IptvRow, ManualTarget, MatchDecision,
        MatchMethod,
    },
    model::{select_programmes, EpgError, GuideChannel, NowNext},
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingError {
    InvalidInput,
    NotFound,
    ApprovalRequired,
    Storage,
}
impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "Invalid EPG binding input",
            Self::NotFound => "Selected EPG source or channel no longer exists",
            Self::ApprovalRequired => "Explicit EPG mapping confirmation required",
            Self::Storage => "EPG bindings could not be stored",
        })
    }
}
impl std::error::Error for BindingError {}
fn db(_: rusqlite::Error) -> BindingError {
    BindingError::Storage
}
fn id(text: &str, max: usize) -> Result<(), BindingError> {
    if text.is_empty()
        || text.len() > max
        || text.trim() != text
        || text.chars().any(char::is_control)
    {
        Err(BindingError::InvalidInput)
    } else {
        Ok(())
    }
}
fn fk(conn: &Connection) -> Result<(), BindingError> {
    let enabled: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .map_err(db)?;
    if enabled != 1 {
        Err(BindingError::Storage)
    } else {
        Ok(())
    }
}
fn guide_exists(conn: &Connection, source_id: &str) -> Result<(), BindingError> {
    id(source_id, 64)?;
    let present: i64 = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM epg_sources WHERE id=?1)",
            [source_id],
            |r| r.get(0),
        )
        .map_err(db)?;
    if present == 1 {
        Ok(())
    } else {
        Err(BindingError::NotFound)
    }
}
fn guide_channel_exists(
    conn: &Connection,
    source_id: &str,
    channel_id: &str,
) -> Result<(), BindingError> {
    guide_exists(conn, source_id)?;
    id(channel_id, 512)?;
    let exists: i64 = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM epg_channels WHERE source_id=?1 AND channel_id=?2)",
            params![source_id, channel_id],
            |r| r.get(0),
        )
        .map_err(db)?;
    if exists == 1 {
        Ok(())
    } else {
        Err(BindingError::NotFound)
    }
}
fn row_in_playlist(
    conn: &Connection,
    channel_id: &str,
) -> Result<(String, Option<String>), BindingError> {
    id(channel_id, 64)?;
    conn.query_row(
        "SELECT source_id,tvg_id FROM iptv_channels WHERE id=?1",
        [channel_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(db)?
    .ok_or(BindingError::NotFound)
}
fn playlist_exists(conn: &Connection, source_id: &str) -> Result<(), BindingError> {
    id(source_id, 64)?;
    let exists: i64 = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM iptv_sources WHERE id=?1)",
            [source_id],
            |r| r.get(0),
        )
        .map_err(db)?;
    if exists == 1 {
        Ok(())
    } else {
        Err(BindingError::NotFound)
    }
}

/// E4 must call this constructor ONLY after the user explicitly confirms the
/// selected guide. It cannot be obtained as the result of a matching query.
#[derive(Debug)]
pub struct ConfirmedGuideSelection(());
impl ConfirmedGuideSelection {
    pub fn after_user_confirmation() -> Self {
        Self(())
    }
}

/// Per-row confirmation affects ONE stable IPTV channel UUID only.
#[derive(Debug)]
pub struct ConfirmedRowMapping(());
impl ConfirmedRowMapping {
    pub fn after_user_confirmation_for_this_stream() -> Self {
        Self(())
    }
}

/// Global confirmation MUST label its cross-playlist impact to the user.
/// The write re-reads the imported exact tvg-id and selected guide inside the
/// transaction to guard against races and wrong-guise source assignment.
#[derive(Debug)]
pub struct ConfirmedGlobalMapping(());
impl ConfirmedGlobalMapping {
    pub fn after_user_confirmation_for_all_matching_streams() -> Self {
        Self(())
    }
}

pub fn set_shared_guide(
    conn: &mut Connection,
    _: ConfirmedGuideSelection,
    source_id: Option<&str>,
) -> Result<(), BindingError> {
    fk(conn)?;
    let tx = conn.transaction().map_err(db)?;
    if let Some(source_id) = source_id {
        guide_exists(&tx, source_id)?;
    }
    let affected = tx
        .execute(
            "UPDATE epg_settings SET shared_guide_source_id=?1 WHERE singleton=1",
            [source_id],
        )
        .map_err(db)?;
    if affected != 1 {
        return Err(BindingError::Storage);
    }
    tx.commit().map_err(db)
}
pub fn set_playlist_exception(
    conn: &mut Connection,
    _: ConfirmedGuideSelection,
    playlist_source_id: &str,
    guide_source_id: Option<&str>,
    at: i64,
) -> Result<(), BindingError> {
    fk(conn)?;
    let tx = conn.transaction().map_err(db)?;
    playlist_exists(&tx, playlist_source_id)?;
    if let Some(source_id) = guide_source_id {
        guide_exists(&tx, source_id)?;
        tx.execute(
            "INSERT INTO epg_playlist_links (playlist_source_id,guide_source_id,updated_at)
             VALUES (?1,?2,?3)
             ON CONFLICT(playlist_source_id) DO UPDATE
             SET guide_source_id=excluded.guide_source_id,updated_at=excluded.updated_at",
            params![playlist_source_id, source_id, at],
        )
        .map_err(db)?;
    } else {
        tx.execute(
            "DELETE FROM epg_playlist_links WHERE playlist_source_id=?1",
            [playlist_source_id],
        )
        .map_err(db)?;
    }
    tx.commit().map_err(db)
}
pub fn set_row_mapping(
    conn: &mut Connection,
    _: ConfirmedRowMapping,
    iptv_channel_id: &str,
    guide_source_id: &str,
    guide_channel_id: &str,
    at: i64,
) -> Result<(), BindingError> {
    fk(conn)?;
    let tx = conn.transaction().map_err(db)?;
    row_in_playlist(&tx, iptv_channel_id)?;
    guide_channel_exists(&tx, guide_source_id, guide_channel_id)?;
    tx.execute(
        "INSERT INTO epg_channel_overrides
         (iptv_channel_id,guide_source_id,guide_channel_id,updated_at)
         VALUES (?1,?2,?3,?4)
         ON CONFLICT(iptv_channel_id) DO UPDATE
         SET guide_source_id=excluded.guide_source_id,
             guide_channel_id=excluded.guide_channel_id,updated_at=excluded.updated_at",
        params![iptv_channel_id, guide_source_id, guide_channel_id, at],
    )
    .map_err(db)?;
    tx.commit().map_err(db)
}
/// A name hint is only a navigational convenience. Confirm the *one* row,
/// guide import generation, literal ID, displayed name and exact XMLTV target
/// under the same write transaction before a per-stream override is stored.
/// The shared guide, aliases and ordinary explicit mapping API are unchanged.
pub struct HintMappingSnapshot<'a> {
    pub row_id: &'a str,
    pub playlist_id: &'a str,
    pub tvg_id: Option<&'a str>,
    pub effective_name: &'a str,
    pub guide_source_id: &'a str,
    pub channel_id: &'a str,
    pub last_success_at: i64,
    pub guide_revision: i64,
}

pub fn set_row_mapping_from_hint_if_unchanged(
    conn: &mut Connection,
    _: ConfirmedRowMapping,
    expected: HintMappingSnapshot<'_>,
    at: i64,
) -> Result<(), BindingError> {
    let HintMappingSnapshot {
        row_id: iptv_channel_id,
        playlist_id: expected_playlist_id,
        tvg_id: expected_tvg_id,
        effective_name: expected_effective_name,
        guide_source_id,
        channel_id: guide_channel_id,
        last_success_at: expected_last_success_at,
        guide_revision: expected_guide_revision,
    } = expected;
    fk(conn)?;
    id(iptv_channel_id, 64)?;
    id(expected_playlist_id, 64)?;
    id(guide_source_id, 64)?;
    id(guide_channel_id, 512)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db)?;
    let current: Option<(String, Option<String>, String)> = tx
        .query_row(
            "SELECT c.source_id,c.tvg_id,COALESCE(o.display_name_override,c.imported_name)
             FROM iptv_channels c JOIN iptv_sources s ON s.id=c.source_id
             LEFT JOIN iptv_channel_overrides o ON o.channel_id=c.id
             WHERE c.id=?1 AND c.is_present=1 AND s.enabled=1",
            [iptv_channel_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(db)?;
    let Some((playlist_id, actual_tvg_id, effective_name)) = current else {
        return Err(BindingError::NotFound);
    };
    if playlist_id != expected_playlist_id
        || actual_tvg_id.as_deref() != expected_tvg_id
        || effective_name != expected_effective_name
        || resolve_source_id(&tx, iptv_channel_id, &playlist_id)?.as_deref()
            != Some(guide_source_id)
    {
        return Err(BindingError::InvalidInput);
    }
    // A newly created manual override or exact-ID alias since inspection is
    // a changed binding decision, not permission to replace it silently.
    let changed_binding: i64 = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM epg_channel_overrides WHERE iptv_channel_id=?1)
               OR EXISTS(SELECT 1 FROM epg_global_id_aliases
                         WHERE guide_source_id=?2 AND iptv_tvg_id=?3)",
            params![iptv_channel_id, guide_source_id, expected_tvg_id],
            |row| row.get(0),
        )
        .map_err(db)?;
    if changed_binding != 0 {
        return Err(BindingError::InvalidInput);
    }
    // The successful-import timestamp is seconds resolution. A same-second
    // replacement can preserve it and the chosen ID, but still advance the
    // durable selected-guide source revision.
    let imported: Option<(Option<i64>, i64)> = tx
        .query_row(
            "SELECT last_success_at,updated_at FROM epg_sources WHERE id=?1 AND enabled=1",
            [guide_source_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(db)?;
    if imported != Some((Some(expected_last_success_at), expected_guide_revision)) {
        return Err(BindingError::InvalidInput);
    }
    guide_channel_exists(&tx, guide_source_id, guide_channel_id)?;
    tx.execute(
        "INSERT INTO epg_channel_overrides
         (iptv_channel_id,guide_source_id,guide_channel_id,updated_at)
         VALUES (?1,?2,?3,?4)",
        params![iptv_channel_id, guide_source_id, guide_channel_id, at],
    )
    .map_err(db)?;
    tx.commit().map_err(db)
}

/// A picker captures a revision-stable, read-only row/guide state. A deliberate
/// click writes only this row, and only if all referenced state is unchanged.
/// Existing explicit overrides may be replaced when the user deliberately
/// selects a different target, unlike the older new-hint-only path.
#[derive(Clone, Debug)]
pub struct PickerMappingSnapshot {
    pub row_id: String,
    pub playlist_id: String,
    pub tvg_id: Option<String>,
    pub effective_name: String,
    pub playlist_revision: i64,
    pub guide_source_id: String,
    pub guide_last_success_at: i64,
    pub guide_revision: i64,
    pub prior_row_target: Option<String>,
    pub prior_alias_target: Option<String>,
}

pub fn set_row_mapping_from_picker_if_unchanged(
    conn: &mut Connection,
    _: ConfirmedRowMapping,
    expected: &PickerMappingSnapshot,
    exact_target: &str,
    at: i64,
) -> Result<(), BindingError> {
    id(&expected.row_id, 64)?;
    id(&expected.playlist_id, 64)?;
    id(&expected.guide_source_id, 64)?;
    id(exact_target, 512)?;
    fk(conn)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db)?;
    let actual: Option<(String, Option<String>, String, i64)> = tx.query_row(
        "SELECT c.source_id,c.tvg_id,COALESCE(o.display_name_override,c.imported_name),s.revision
         FROM iptv_channels c JOIN iptv_sources s ON s.id=c.source_id
         LEFT JOIN iptv_channel_overrides o ON o.channel_id=c.id
         WHERE c.id=?1 AND c.is_present=1 AND s.enabled=1",
        [&expected.row_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).optional().map_err(db)?;
    let Some((playlist, tvg_id, name, revision)) = actual else {
        return Err(BindingError::NotFound);
    };
    if playlist != expected.playlist_id
        || tvg_id != expected.tvg_id
        || name != expected.effective_name
        || revision != expected.playlist_revision
        || resolve_source_id(&tx, &expected.row_id, &playlist)?.as_deref()
            != Some(expected.guide_source_id.as_str())
    {
        return Err(BindingError::InvalidInput);
    }
    let row_target: Option<String> = tx
        .query_row(
            "SELECT guide_channel_id FROM epg_channel_overrides WHERE iptv_channel_id=?1",
            [&expected.row_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db)?;
    let alias_target: Option<String> = tx
        .query_row(
            "SELECT guide_channel_id FROM epg_global_id_aliases
         WHERE guide_source_id=?1 AND iptv_tvg_id=?2",
            params![expected.guide_source_id, expected.tvg_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db)?;
    if row_target != expected.prior_row_target || alias_target != expected.prior_alias_target {
        return Err(BindingError::InvalidInput);
    }
    // Both fields must match under the same IMMEDIATE transaction as the
    // row, effective guide and exact guide-channel existence checks.
    let imported: Option<(Option<i64>, i64)> = tx
        .query_row(
            "SELECT last_success_at,updated_at FROM epg_sources WHERE id=?1 AND enabled=1",
            [&expected.guide_source_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db)?;
    if imported
        != Some((
            Some(expected.guide_last_success_at),
            expected.guide_revision,
        ))
    {
        return Err(BindingError::InvalidInput);
    }
    guide_channel_exists(&tx, &expected.guide_source_id, exact_target)?;
    tx.execute(
        "INSERT INTO epg_channel_overrides(iptv_channel_id,guide_source_id,guide_channel_id,updated_at)
         VALUES(?1,?2,?3,?4) ON CONFLICT(iptv_channel_id) DO UPDATE SET
         guide_source_id=excluded.guide_source_id,
         guide_channel_id=excluded.guide_channel_id,updated_at=excluded.updated_at",
        params![expected.row_id, expected.guide_source_id, exact_target, at],
    ).map_err(db)?;
    tx.commit().map_err(db)
}

/// Read-only selected-guide diagnosis; does not consult stream URLs or EPG
/// schedule bodies. Return None if no effective shared/playlist/row choice.
pub fn selected_guide_for_row(
    conn: &Connection,
    iptv_channel_id: &str,
) -> Result<Option<String>, BindingError> {
    let (playlist, _) = row_in_playlist(conn, iptv_channel_id)?;
    resolve_source_id(conn, iptv_channel_id, &playlist)
}

pub fn clear_row_mapping(
    conn: &mut Connection,
    _: ConfirmedRowMapping,
    iptv_channel_id: &str,
) -> Result<(), BindingError> {
    fk(conn)?;
    let tx = conn.transaction().map_err(db)?;
    row_in_playlist(&tx, iptv_channel_id)?;
    tx.execute(
        "DELETE FROM epg_channel_overrides WHERE iptv_channel_id=?1",
        [iptv_channel_id],
    )
    .map_err(db)?;
    tx.commit().map_err(db)
}
pub fn set_global_alias(
    conn: &mut Connection,
    _: ConfirmedGlobalMapping,
    sample_iptv_channel_id: &str,
    guide_source_id: &str,
    guide_channel_id: &str,
    at: i64,
) -> Result<(), BindingError> {
    fk(conn)?;
    let tx = conn.transaction().map_err(db)?;
    let (playlist, imported_tvg) = row_in_playlist(&tx, sample_iptv_channel_id)?;
    let tvg = imported_tvg.as_deref().ok_or(BindingError::InvalidInput)?;
    id(tvg, 512)?;
    guide_channel_exists(&tx, guide_source_id, guide_channel_id)?;
    let selected = resolve_source_id(&tx, sample_iptv_channel_id, &playlist)?;
    if selected.as_deref() != Some(guide_source_id) {
        return Err(BindingError::InvalidInput);
    }
    tx.execute(
        "INSERT INTO epg_global_id_aliases
         (guide_source_id,iptv_tvg_id,guide_channel_id,updated_at) VALUES (?1,?2,?3,?4)
         ON CONFLICT(guide_source_id,iptv_tvg_id) DO UPDATE
         SET guide_channel_id=excluded.guide_channel_id,updated_at=excluded.updated_at",
        params![guide_source_id, tvg, guide_channel_id, at],
    )
    .map_err(db)?;
    tx.commit().map_err(db)
}
/// Store an all-playlist exact-ID alias only for the literal imported tvg-id
/// the user saw on the confirmation sheet. A playlist refresh may retain the
/// row UUID while changing its tvg-id; the comparison and UPSERT therefore
/// MUST share one immediate write transaction. Mismatch is fail-closed.
pub fn set_global_alias_if_imported_id_matches(
    conn: &mut Connection,
    _: ConfirmedGlobalMapping,
    sample_iptv_channel_id: &str,
    expected_imported_tvg_id: &str,
    guide_source_id: &str,
    guide_channel_id: &str,
    at: i64,
) -> Result<(), BindingError> {
    id(expected_imported_tvg_id, 512)?;
    fk(conn)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db)?;
    let (playlist, imported_tvg) = row_in_playlist(&tx, sample_iptv_channel_id)?;
    if imported_tvg.as_deref() != Some(expected_imported_tvg_id) {
        return Err(BindingError::InvalidInput);
    }
    guide_channel_exists(&tx, guide_source_id, guide_channel_id)?;
    let selected = resolve_source_id(&tx, sample_iptv_channel_id, &playlist)?;
    if selected.as_deref() != Some(guide_source_id) {
        return Err(BindingError::InvalidInput);
    }
    tx.execute(
        "INSERT INTO epg_global_id_aliases
         (guide_source_id,iptv_tvg_id,guide_channel_id,updated_at) VALUES (?1,?2,?3,?4)
         ON CONFLICT(guide_source_id,iptv_tvg_id) DO UPDATE
         SET guide_channel_id=excluded.guide_channel_id,updated_at=excluded.updated_at",
        params![
            guide_source_id,
            expected_imported_tvg_id,
            guide_channel_id,
            at
        ],
    )
    .map_err(db)?;
    tx.commit().map_err(db)
}

pub fn clear_global_alias(
    conn: &mut Connection,
    _: ConfirmedGlobalMapping,
    guide_source_id: &str,
    imported_tvg_id: &str,
) -> Result<(), BindingError> {
    fk(conn)?;
    id(guide_source_id, 64)?;
    id(imported_tvg_id, 512)?;
    let tx = conn.transaction().map_err(db)?;
    guide_exists(&tx, guide_source_id)?;
    tx.execute(
        "DELETE FROM epg_global_id_aliases WHERE guide_source_id=?1 AND iptv_tvg_id=?2",
        params![guide_source_id, imported_tvg_id],
    )
    .map_err(db)?;
    tx.commit().map_err(db)
}
fn resolve_source_id(
    conn: &Connection,
    channel_id: &str,
    playlist: &str,
) -> Result<Option<String>, BindingError> {
    let manual: Option<String> = conn
        .query_row(
            "SELECT guide_source_id FROM epg_channel_overrides WHERE iptv_channel_id=?1",
            [channel_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db)?;
    let exception: Option<String> = conn
        .query_row(
            "SELECT guide_source_id FROM epg_playlist_links WHERE playlist_source_id=?1",
            [playlist],
            |r| r.get(0),
        )
        .optional()
        .map_err(db)?;
    let shared: Option<String> = conn
        .query_row(
            "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(db)?;
    Ok(manual.or(exception).or(shared))
}

// A SAVEPOINT can be nested under a caller-owned transaction, unlike BEGIN.
// Its first SELECT establishes one SQLite snapshot for the complete semantic
// read. Always release it, even when a query is cancelled or fails. The reader
// performs no writes; ROLLBACK TO also protects an outer caller's transaction.
fn with_read_snapshot<T>(
    conn: &Connection,
    operation: impl FnOnce() -> Result<T, EpgError>,
) -> Result<T, EpgError> {
    conn.execute_batch("SAVEPOINT broadcaster_epg_read_snapshot")
        .map_err(|_| EpgError::Storage)?;
    let result = operation();
    let cleanup = conn.execute_batch(
        "ROLLBACK TO broadcaster_epg_read_snapshot; RELEASE broadcaster_epg_read_snapshot",
    );
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (_, Err(_)) => Err(EpgError::Storage),
    }
}

// The SQL-v2 composite PRIMARY KEY(source_id,channel_id) makes this an
// indexed case-sensitive lookup. Never scan the catalogue for explicit targets.
fn channel_present(conn: &Connection, source_id: &str, target_id: &str) -> Result<bool, EpgError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM epg_channels WHERE source_id=?1 AND channel_id=?2)",
        params![source_id, target_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|present| present == 1)
    .map_err(|_| EpgError::Storage)
}

/// Maximum whole-guide name suggestion scan per individual row. Above this
/// bound, a suggestion cannot be proved unique and is not evaluated.
const NAME_SUGGESTION_SCAN_LIMIT: usize = 10_000;

/// Read resolution without reading media URLs or touching network adapters.
/// Row input includes the current effective name (user override, if any).
pub fn resolve_row(
    conn: &Connection,
    channel_id: &str,
    cancel: &AtomicBool,
) -> Result<MatchDecision, EpgError> {
    with_read_snapshot(conn, || resolve_row_inner(conn, channel_id, cancel))
}

fn resolve_row_inner(
    conn: &Connection,
    channel_id: &str,
    cancel: &AtomicBool,
) -> Result<MatchDecision, EpgError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(EpgError::Cancelled);
    }
    if id(channel_id, 64).is_err() {
        return Err(EpgError::InvalidInput);
    }
    let row: Option<IptvRow> = conn
        .query_row(
            "SELECT c.id,c.source_id,c.tvg_id,c.tvg_name,
                COALESCE(NULLIF(o.display_name_override,''),c.imported_name)
         FROM iptv_channels c
         LEFT JOIN iptv_channel_overrides o ON o.channel_id=c.id WHERE c.id=?1",
            [channel_id],
            |r| {
                Ok(IptvRow {
                    id: r.get(0)?,
                    playlist_source_id: r.get(1)?,
                    tvg_id: r.get(2)?,
                    tvg_name: r.get(3)?,
                    effective_name: r.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|_| EpgError::Storage)?;
    let row = row.ok_or(EpgError::InvalidInput)?;
    let manual: Option<ManualTarget> = conn.query_row(
        "SELECT guide_source_id,guide_channel_id FROM epg_channel_overrides WHERE iptv_channel_id=?1",
        [channel_id],
        |r| Ok(ManualTarget { guide_source_id: r.get(0)?, guide_channel_id: r.get(1)? }),
    ).optional().map_err(|_| EpgError::Storage)?;
    let exception: Option<String> = conn
        .query_row(
            "SELECT guide_source_id FROM epg_playlist_links WHERE playlist_source_id=?1",
            [&row.playlist_source_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| EpgError::Storage)?;
    let shared: Option<String> = conn
        .query_row(
            "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(|_| EpgError::Storage)?;
    let chosen = choose_source(manual.as_ref(), exception.as_deref(), shared.as_deref());
    let Some(chosen_source) = chosen.as_ref() else {
        return Ok(MatchDecision::Unconfigured);
    };
    let selected_id = &chosen_source.guide_source_id;
    let src: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT enabled,last_success_at FROM epg_sources WHERE id=?1",
            [selected_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| EpgError::Storage)?;
    let Some((enabled, last_success)) = src else {
        return Ok(MatchDecision::Unavailable {
            source_id: selected_id.clone(),
        });
    };
    let availability = if enabled == 0 {
        GuideAvailability::Disabled
    } else if last_success.is_none() {
        GuideAvailability::NotFetched
    } else {
        GuideAvailability::Available
    };
    if availability != GuideAvailability::Available {
        return Ok(match_in_source(
            &row,
            chosen.as_ref(),
            availability,
            manual.as_ref(),
            None,
            &[],
        ));
    }
    let alias: Option<String> = match row.tvg_id.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(tvg) => conn
            .query_row(
                "SELECT guide_channel_id FROM epg_global_id_aliases
             WHERE guide_source_id=?1 AND iptv_tvg_id=?2",
                params![selected_id, tvg],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| EpgError::Storage)?,
        None => None,
    };
    // An explicit target must resolve independently of catalogue cardinality.
    // Missing manual/confirmed targets never fall through to name or exact ID.
    if let Some(target) = manual.as_ref() {
        if target.guide_source_id != *selected_id {
            return Ok(MatchDecision::Unavailable {
                source_id: selected_id.clone(),
            });
        }
        return Ok(
            if channel_present(conn, selected_id, &target.guide_channel_id)? {
                MatchDecision::Matched {
                    source_id: selected_id.clone(),
                    channel_id: target.guide_channel_id.clone(),
                    method: MatchMethod::RowOverride,
                }
            } else {
                MatchDecision::MissingTarget {
                    source_id: selected_id.clone(),
                    channel_id: target.guide_channel_id.clone(),
                }
            },
        );
    }
    if let Some(target) = alias.as_deref() {
        return Ok(if channel_present(conn, selected_id, target)? {
            MatchDecision::Matched {
                source_id: selected_id.clone(),
                channel_id: target.to_owned(),
                method: MatchMethod::ConfirmedGlobalAlias,
            }
        } else {
            MatchDecision::MissingTarget {
                source_id: selected_id.clone(),
                channel_id: target.to_owned(),
            }
        });
    }
    if let Some(tvg_id) = row.tvg_id.as_deref().filter(|s| !s.trim().is_empty()) {
        if channel_present(conn, selected_id, tvg_id)? {
            return Ok(MatchDecision::Matched {
                source_id: selected_id.clone(),
                channel_id: tvg_id.to_owned(),
                method: MatchMethod::ExactTvgId,
            });
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(EpgError::Cancelled);
    }

    // Full identity candidate keys are generated without touching programme
    // contents or another guide. SQLite's selected-guide composite PK proves
    // presence; same read snapshot guarantees source/guide coherence. Unlike
    // review-only folded names, this result feeds the ACTUAL now/next query.
    if let Some(original) = row.tvg_id.as_deref() {
        let mut proven = Vec::new();
        for (candidate_id, kind) in super::matching::automatic_identity_candidates(original) {
            let record: Option<String> = conn
                .query_row(
                    "SELECT display_name FROM epg_channels WHERE source_id=?1 AND channel_id=?2",
                    params![selected_id, candidate_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|_| EpgError::Storage)?;
            if let Some(display_name) = record {
                let channel = GuideChannel {
                    source_id: selected_id.clone(),
                    channel_id: candidate_id.clone(),
                    display_name,
                };
                if super::matching::corroborated_auto_identity(original, kind, &channel) {
                    // Multiple feed-specific guide IDs for the same base veto
                    // a quality-only collapse even if the bare stem exists.
                    let ambiguous_feed = if kind == super::matching::AutoIdentityKind::Quality {
                        let pattern = format!("{}@*", candidate_id);
                        let count: i64 = conn.query_row(
                            "SELECT count(*) FROM epg_channels WHERE source_id=?1 AND channel_id GLOB ?2",
                            params![selected_id, pattern], |r| r.get(0),
                        ).map_err(|_| EpgError::Storage)?;
                        count > 0
                    } else {
                        false
                    };
                    if !ambiguous_feed {
                        proven.push(candidate_id);
                    }
                }
            }
        }
        if proven.len() == 1 {
            return Ok(MatchDecision::Matched {
                source_id: selected_id.clone(),
                channel_id: proven.remove(0),
                method: MatchMethod::AutomaticIdentity,
            });
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(EpgError::Cancelled);
    }

    // Name suggestions are never attached bindings. A LIMIT+sentinel cannot
    // establish uniqueness if the guide contains more channels than the cap.
    // Explicit targets above bypass this entire scan (and its allocations).
    let mut stmt = conn
        .prepare(
            "SELECT source_id,channel_id,display_name FROM epg_channels
             WHERE source_id=?1 ORDER BY channel_id LIMIT 10001",
        )
        .map_err(|_| EpgError::Storage)?;
    let rows = stmt
        .query_map([selected_id], |r| {
            Ok(GuideChannel {
                source_id: r.get(0)?,
                channel_id: r.get(1)?,
                display_name: r.get(2)?,
            })
        })
        .map_err(|_| EpgError::Storage)?;
    let mut channels = Vec::new();
    for item in rows {
        if cancel.load(Ordering::Relaxed) {
            return Err(EpgError::Cancelled);
        }
        if channels.len() == NAME_SUGGESTION_SCAN_LIMIT {
            return Ok(MatchDecision::SuggestionsUnavailable {
                source_id: selected_id.clone(),
            });
        }
        channels.push(item.map_err(|_| EpgError::Storage)?);
    }
    Ok(match_in_source(
        &row,
        chosen.as_ref(),
        availability,
        None,
        None,
        &channels,
    ))
}

#[derive(Debug, Clone)]
pub enum SemanticProgrammeState {
    Unconfigured,
    Disabled,
    Unavailable,
    Unmatched,
    AmbiguousMapping,
    MissingTarget,
    /// Catalogue too large to prove a unique nonbinding name suggestion.
    SuggestionsUnavailable,
    /// A proposal that is NOT a saved or attached mapping.
    Suggested {
        source_id: String,
        channel_id: String,
    },
    /// All scheduling information is from ONE selected cached guide.
    Matched {
        method: MatchMethod,
        page: Box<BoundedGuidePage>,
        /// None for stale/unknown freshness or truncated results.
        now_next: Option<Box<NowNext>>,
        refresh_failed: bool,
    },
}

/// E3's reusable semantic now/next seam; no GTK, UI focus or network operations.
/// Reads a full 48-hour half-open window to include programmes starting up to
/// 24h before now and a bounded next lookahead. Never claims full coverage.
pub fn read_semantic_now_next(
    conn: &Connection,
    iptv_channel_id: &str,
    now_utc: i64,
    cancel: &AtomicBool,
) -> Result<SemanticProgrammeState, EpgError> {
    with_read_snapshot(conn, || {
        read_semantic_now_next_inner(conn, iptv_channel_id, now_utc, cancel)
    })
}

fn read_semantic_now_next_inner(
    conn: &Connection,
    iptv_channel_id: &str,
    now_utc: i64,
    cancel: &AtomicBool,
) -> Result<SemanticProgrammeState, EpgError> {
    let match_state = resolve_row_inner(conn, iptv_channel_id, cancel)?;
    let (source, channel, method) = match match_state {
        MatchDecision::Unconfigured => return Ok(SemanticProgrammeState::Unconfigured),
        MatchDecision::Disabled { .. } => return Ok(SemanticProgrammeState::Disabled),
        MatchDecision::Unavailable { .. } => return Ok(SemanticProgrammeState::Unavailable),
        MatchDecision::Unmatched { .. } => return Ok(SemanticProgrammeState::Unmatched),
        MatchDecision::Ambiguous { .. } => return Ok(SemanticProgrammeState::AmbiguousMapping),
        MatchDecision::SuggestionsUnavailable { .. } => {
            return Ok(SemanticProgrammeState::SuggestionsUnavailable)
        }
        MatchDecision::MissingTarget { .. } => return Ok(SemanticProgrammeState::MissingTarget),
        MatchDecision::Suggested {
            source_id,
            channel_id,
        } => {
            return Ok(SemanticProgrammeState::Suggested {
                source_id,
                channel_id,
            })
        }
        MatchDecision::Matched {
            source_id,
            channel_id,
            method,
        } => (source_id, channel_id, method),
    };
    let from = now_utc.checked_sub(86_400).ok_or(EpgError::InvalidInput)?;
    let to = now_utc.checked_add(86_400).ok_or(EpgError::InvalidInput)?;
    let page = read_guide_window(
        conn,
        &source,
        std::slice::from_ref(&channel),
        from,
        to,
        now_utc,
        cancel,
    )?;
    let refresh_failed: bool = conn
        .query_row(
            "SELECT last_error_code IS NOT NULL FROM epg_sources WHERE id=?1",
            [&source],
            |r| r.get(0),
        )
        .map_err(|_| EpgError::Storage)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(EpgError::Cancelled);
    }
    let now_next = if page.truncated
        || matches!(
            page.freshness,
            Freshness::Stale | Freshness::NeverFetched | Freshness::Unknown
        )
        || matches!(page.coverage, Coverage::Disabled | Coverage::NoData)
    {
        None
    } else {
        Some(Box::new(select_programmes(
            &page.programmes,
            &channel,
            now_utc,
        )))
    };
    Ok(SemanticProgrammeState::Matched {
        method,
        page: Box::new(page),
        now_next,
        refresh_failed,
    })
}

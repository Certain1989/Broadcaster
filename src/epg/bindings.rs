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
use rusqlite::{params, Connection, OptionalExtension};
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

/// Read resolution without reading media URLs or touching network adapters.
/// Row input includes the current effective name (user override, if any).
pub fn resolve_row(
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
        if channels.len() >= 10_000 {
            return Err(EpgError::OverLimit);
        }
        channels.push(item.map_err(|_| EpgError::Storage)?);
    }
    Ok(match_in_source(
        &row,
        chosen.as_ref(),
        availability,
        manual.as_ref(),
        alias.as_deref(),
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
    /// A proposal that is NOT a saved or attached mapping.
    Suggested {
        source_id: String,
        channel_id: String,
    },
    /// All scheduling information is from ONE selected cached guide.
    Matched {
        method: MatchMethod,
        page: BoundedGuidePage,
        /// None for stale/unknown freshness or truncated results.
        now_next: Option<NowNext>,
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
    let match_state = resolve_row(conn, iptv_channel_id, cancel)?;
    let (source, channel, method) = match match_state {
        MatchDecision::Unconfigured => return Ok(SemanticProgrammeState::Unconfigured),
        MatchDecision::Disabled { .. } => return Ok(SemanticProgrammeState::Disabled),
        MatchDecision::Unavailable { .. } => return Ok(SemanticProgrammeState::Unavailable),
        MatchDecision::Unmatched { .. } => return Ok(SemanticProgrammeState::Unmatched),
        MatchDecision::Ambiguous { .. } => return Ok(SemanticProgrammeState::AmbiguousMapping),
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
        Some(select_programmes(&page.programmes, &channel, now_utc))
    };
    Ok(SemanticProgrammeState::Matched {
        method,
        page,
        now_next,
        refresh_failed,
    })
}

// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SQLite-v2 atomic confirmation for explicitly reviewed XMLTV aliases.
//! Blocking-worker API: never call from GTK's main loop. No network or URL reads.
//! This module does not infer identity; only the caller's approved original IDs
//! are eligible, and transaction-time state is checked before any insert.

use super::candidates::RULE_PROFILE_VERSION;
use rusqlite::{params, types::ValueRef, Connection, OptionalExtension, TransactionBehavior};
use std::{
    collections::{hash_map::RandomState, HashSet},
    hash::{BuildHasher, Hash, Hasher},
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_ALIAS_KEYS: usize = 4_096;
const MAX_FINGERPRINT_ROWS: usize = 6_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkError {
    InvalidInput,
    MissingGuideOrTarget,
    ExistingAlias,
    Stale,
    Cancelled,
    Storage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasChoice {
    /// Original imported case-sensitive M3U ID (not a folded display name).
    pub original_tvg_id: String,
    /// Full, unmodified channel ID from the explicitly selected XMLTV guide.
    pub full_xmltv_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasImpact {
    pub choice: AliasChoice,
    /// Present, enabled playlist rows currently selecting this guide.
    pub selected_guide_rows: u64,
    /// Among those, manual per-row mappings take precedence over this alias.
    pub manual_exclusions: u64,
    /// Other currently playable rows with the same ID but another selected guide.
    pub other_guide_rows: u64,
    /// The confirmed alias also affects FUTURE rows with this exact original ID.
    pub future_rows_also_affected: bool,
}

/// A preview must be treated as opaque and discarded on *any* UI invalidation.
/// The keyed fingerprint is regenerated from SQLite inside an IMMEDIATE write
/// transaction at confirmation, including all relevant playlist/guide rows and
/// complete selected-guide programme contents, without retaining programme data.
#[derive(Clone)]
pub struct BulkPreview {
    guide: String,
    impact: Vec<AliasImpact>,
    hasher: RandomState,
    before: u64,
}

impl BulkPreview {
    pub fn guide_source_id(&self) -> &str {
        &self.guide
    }
    pub fn impact(&self) -> &[AliasImpact] {
        &self.impact
    }
}

/// Construct only from an explicit review confirmation UI action.
pub struct ConfirmedBulkReview(());
impl ConfirmedBulkReview {
    pub fn after_user_confirmation() -> Self {
        Self(())
    }
}

/// Volatile same-session undo; never persist or replay after restart.
/// Fingerprint includes the post-commit alias values and revisions, making an
/// intervening write fail closed even when timestamps were reused.
pub struct SessionUndo {
    preview: BulkPreview,
    after: u64,
    write_time: i64,
}

fn sql(_: rusqlite::Error) -> BulkError {
    BulkError::Storage
}

fn valid_id(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn guard_cancel(cancel: &AtomicBool) -> Result<(), BulkError> {
    if cancel.load(Ordering::Acquire) {
        Err(BulkError::Cancelled)
    } else {
        Ok(())
    }
}

fn foreign_keys(conn: &Connection) -> Result<(), BulkError> {
    let enabled: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .map_err(sql)?;
    if enabled != 1 {
        return Err(BulkError::Storage);
    }
    Ok(())
}

fn guide_ready(conn: &Connection, guide: &str) -> Result<(), BulkError> {
    let status: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT enabled,last_success_at FROM epg_sources WHERE id=?1",
            [guide],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sql)?;
    if !matches!(status, Some((1, Some(_)))) {
        return Err(BulkError::MissingGuideOrTarget);
    }
    Ok(())
}

fn validate_choices(
    conn: &Connection,
    guide: &str,
    choices: &[AliasChoice],
) -> Result<(), BulkError> {
    if !valid_id(guide, 64) || choices.is_empty() || choices.len() > MAX_ALIAS_KEYS {
        return Err(BulkError::InvalidInput);
    }
    guide_ready(conn, guide)?;
    let mut originals = HashSet::new();
    for choice in choices {
        if !valid_id(&choice.original_tvg_id, 512)
            || !valid_id(&choice.full_xmltv_id, 512)
            || !originals.insert(choice.original_tvg_id.as_str())
        {
            return Err(BulkError::InvalidInput);
        }
        let exists: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM epg_channels WHERE source_id=?1 AND channel_id=?2)",
                params![guide, choice.full_xmltv_id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if exists != 1 {
            return Err(BulkError::MissingGuideOrTarget);
        }
        let alias: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM epg_global_id_aliases WHERE guide_source_id=?1 AND iptv_tvg_id=?2)",
                params![guide, choice.original_tvg_id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if alias != 0 {
            // Existing confirmed aliases always win, even when target agrees.
            return Err(BulkError::ExistingAlias);
        }
    }
    Ok(())
}

fn impact(conn: &Connection, guide: &str, choice: &AliasChoice) -> Result<AliasImpact, BulkError> {
    // Row overrides take priority over global and per-playlist guide choice.
    // Rows not present, sources disabled, and rows using another guide are not
    // counted as CURRENT effects. Future same-ID rows are a separate warning.
    let (selected, manual, elsewhere): (i64, i64, i64) = conn
        .query_row(
            "SELECT
               COALESCE(SUM(CASE WHEN COALESCE(o.guide_source_id,l.guide_source_id,e.shared_guide_source_id)=?1 THEN 1 ELSE 0 END),0),
               COALESCE(SUM(CASE WHEN COALESCE(o.guide_source_id,l.guide_source_id,e.shared_guide_source_id)=?1 AND o.iptv_channel_id IS NOT NULL THEN 1 ELSE 0 END),0),
               COALESCE(SUM(CASE WHEN COALESCE(o.guide_source_id,l.guide_source_id,e.shared_guide_source_id) IS NOT ?1 THEN 1 ELSE 0 END),0)
             FROM iptv_channels c
             JOIN iptv_sources s ON s.id=c.source_id
             JOIN epg_settings e ON e.singleton=1
             LEFT JOIN epg_playlist_links l ON l.playlist_source_id=c.source_id
             LEFT JOIN epg_channel_overrides o ON o.iptv_channel_id=c.id
             WHERE c.tvg_id=?2 AND s.enabled=1 AND c.is_present=1",
            params![guide, choice.original_tvg_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sql)?;
    Ok(AliasImpact {
        choice: choice.clone(),
        selected_guide_rows: selected as u64,
        manual_exclusions: manual as u64,
        other_guide_rows: elsewhere as u64,
        future_rows_also_affected: true,
    })
}

fn hash_rows(
    conn: &Connection,
    query: &str,
    guide: Option<&str>,
    hasher: &mut impl Hasher,
    cancel: &AtomicBool,
    total: &mut usize,
) -> Result<(), BulkError> {
    query.hash(hasher); // Namespaces identical sequences from different tables.
    let mut stmt = conn.prepare(query).map_err(sql)?;
    let columns = stmt.column_count();
    let mut rows = if let Some(guide) = guide {
        stmt.query([guide]).map_err(sql)?
    } else {
        stmt.query([]).map_err(sql)?
    };
    while let Some(row) = rows.next().map_err(sql)? {
        *total += 1;
        if *total > MAX_FINGERPRINT_ROWS
            || (*total).is_multiple_of(1024) && cancel.load(Ordering::Relaxed)
        {
            return if *total > MAX_FINGERPRINT_ROWS {
                Err(BulkError::Storage)
            } else {
                Err(BulkError::Cancelled)
            };
        }
        for index in 0..columns {
            match row.get_ref(index).map_err(sql)? {
                ValueRef::Null => 0_u8.hash(hasher),
                ValueRef::Integer(value) => {
                    1_u8.hash(hasher);
                    value.hash(hasher);
                }
                ValueRef::Real(value) => {
                    2_u8.hash(hasher);
                    value.to_bits().hash(hasher);
                }
                ValueRef::Text(value) => {
                    3_u8.hash(hasher);
                    value.hash(hasher);
                }
                ValueRef::Blob(value) => {
                    4_u8.hash(hasher);
                    value.hash(hasher);
                }
            }
        }
    }
    Ok(())
}

fn fingerprint(
    conn: &Connection,
    preview: &BulkPreview,
    cancel: &AtomicBool,
) -> Result<u64, BulkError> {
    guard_cancel(cancel)?;
    let mut hasher = preview.hasher.build_hasher();
    RULE_PROFILE_VERSION.hash(&mut hasher);
    preview.guide.hash(&mut hasher);
    for item in &preview.impact {
        item.choice.original_tvg_id.hash(&mut hasher);
        item.choice.full_xmltv_id.hash(&mut hasher);
        item.selected_guide_rows.hash(&mut hasher);
        item.manual_exclusions.hash(&mut hasher);
        item.other_guide_rows.hash(&mut hasher);
    }
    let mut total = 0;
    // These ordered scans include disabled/deleted/import revision state,
    // manual overrides, shared/per-list choice, all guide channel identities,
    // aliases and the guide's ACTUAL cached programme contents. No timestamps
    // or PRAGMA data_version are trusted as a sufficient revision by themselves.
    for (query, scoped) in [
        ("SELECT id,name,origin_kind,enabled,auto_refresh,updated_at,last_attempt_at,last_success_at,last_error_code FROM epg_sources WHERE id=?1 ORDER BY id", true),
        ("SELECT channel_id,display_name FROM epg_channels WHERE source_id=?1 ORDER BY channel_id", true),
        ("SELECT channel_id,start_utc,stop_utc,title,subtitle,description FROM epg_programmes WHERE source_id=?1 ORDER BY channel_id,start_utc,stop_utc,title", true),
        ("SELECT iptv_tvg_id,guide_channel_id,updated_at FROM epg_global_id_aliases WHERE guide_source_id=?1 ORDER BY iptv_tvg_id", true),
        ("SELECT id,enabled,revision,updated_at,last_attempt_at,last_success_at FROM iptv_sources ORDER BY id", false),
        ("SELECT id,source_id,tvg_id,tvg_name,imported_name,is_present,last_seen_revision,updated_at,imported_region_code,imported_region_label FROM iptv_channels ORDER BY id", false),
        ("SELECT iptv_channel_id,guide_source_id,guide_channel_id,updated_at FROM epg_channel_overrides ORDER BY iptv_channel_id", false),
        ("SELECT playlist_source_id,guide_source_id,updated_at FROM epg_playlist_links ORDER BY playlist_source_id", false),
        ("SELECT singleton,shared_guide_source_id FROM epg_settings ORDER BY singleton", false),
        ("SELECT channel_id,display_name_override,group_mode,group_override,language_mode,country_mode,region_mode,region_code_override,region_label_override,updated_at FROM iptv_channel_overrides ORDER BY channel_id", false),
    ] {
        hash_rows(conn, query, scoped.then_some(preview.guide.as_str()), &mut hasher, cancel, &mut total)?;
    }
    guard_cancel(cancel)?;
    Ok(hasher.finish())
}

/// Preview can be computed on a blocking worker. Only the caller's explicit
/// approved candidate choices are considered; no suggestion is applied here.
pub fn preview_bulk(
    conn: &mut Connection,
    guide: &str,
    choices: &[AliasChoice],
    cancel: &AtomicBool,
) -> Result<BulkPreview, BulkError> {
    foreign_keys(conn)?;
    let tx = conn.transaction().map_err(sql)?;
    validate_choices(&tx, guide, choices)?;
    let mut sorted = choices.to_vec();
    sorted.sort_by(|a, b| a.original_tvg_id.cmp(&b.original_tvg_id));
    let impacts = sorted
        .iter()
        .map(|choice| impact(&tx, guide, choice))
        .collect::<Result<Vec<_>, _>>()?;
    let mut preview = BulkPreview {
        guide: guide.into(),
        impact: impacts,
        hasher: RandomState::new(),
        before: 0,
    };
    preview.before = fingerprint(&tx, &preview, cancel)?;
    tx.commit().map_err(sql)?;
    Ok(preview)
}

/// Entire batch is atomic, prevalidated under SQLite's IMMEDIATE write lock.
/// `at` is a caller-supplied Unix timestamp; the fingerprint, not that clock,
/// is the authoritative conflict/undo guard.
pub fn apply_bulk(
    conn: &mut Connection,
    _: ConfirmedBulkReview,
    preview: BulkPreview,
    at: i64,
    cancel: &AtomicBool,
) -> Result<SessionUndo, BulkError> {
    foreign_keys(conn)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    let choices = preview
        .impact
        .iter()
        .map(|item| item.choice.clone())
        .collect::<Vec<_>>();
    validate_choices(&tx, &preview.guide, &choices)?;
    if fingerprint(&tx, &preview, cancel)? != preview.before {
        return Err(BulkError::Stale);
    }
    // SQLite rolls back the *entire* IMMEDIATE transaction on any error/cancel.
    for item in &preview.impact {
        guard_cancel(cancel)?;
        if tx.execute(
            "INSERT INTO epg_global_id_aliases (guide_source_id,iptv_tvg_id,guide_channel_id,updated_at) VALUES (?1,?2,?3,?4)",
            params![preview.guide, item.choice.original_tvg_id, item.choice.full_xmltv_id, at],
        ).map_err(sql)? != 1 {
            return Err(BulkError::Storage);
        }
    }
    let after = fingerprint(&tx, &preview, cancel)?;
    tx.commit().map_err(sql)?;
    Ok(SessionUndo {
        preview,
        after,
        write_time: at,
    })
}

/// This is intentionally all-or-nothing; an unrelated intervening write may
/// conservatively refuse undo. Never erase newer alias edits or playlist work.
pub fn undo_same_session(
    conn: &mut Connection,
    undo: SessionUndo,
    cancel: &AtomicBool,
) -> Result<(), BulkError> {
    foreign_keys(conn)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    if fingerprint(&tx, &undo.preview, cancel)? != undo.after {
        return Err(BulkError::Stale);
    }
    for item in &undo.preview.impact {
        guard_cancel(cancel)?;
        let deleted = tx.execute(
            "DELETE FROM epg_global_id_aliases WHERE guide_source_id=?1 AND iptv_tvg_id=?2 AND guide_channel_id=?3 AND updated_at=?4",
            params![undo.preview.guide, item.choice.original_tvg_id, item.choice.full_xmltv_id, undo.write_time],
        ).map_err(sql)?;
        if deleted != 1 {
            return Err(BulkError::Stale);
        }
    }
    tx.commit().map_err(sql)
}
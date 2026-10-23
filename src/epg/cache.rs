// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Offline SQLite adapter. External downloads, XML/file parsing, GTK and fetching
//! are forbidden inside a window read. All calls are synchronous/blocking-worker only.
use super::model::{bounded_text, validate_snapshot, EpgError, GuideSnapshot, Programme};
use rusqlite::{
    params, params_from_iter, types::Value, Connection, OptionalExtension, TransactionBehavior,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

fn db(_: rusqlite::Error) -> EpgError {
    EpgError::Storage
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    NeverFetched,
    Fresh,
    Aging,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// Missing listings never establish that a channel is off-air.
    Partial {
        observed_start_utc: Option<i64>,
        observed_stop_utc: Option<i64>,
    },
    NoData,
    Disabled,
}

#[derive(Debug, Clone)]
pub struct BoundedGuidePage {
    pub source_id: String,
    pub selected_guide_channels: Vec<String>,
    pub start_utc: i64,
    pub end_utc: i64,
    pub programmes: Vec<Programme>,
    /// True exactly when one additional matching row was found after 4,096.
    pub truncated: bool,
    pub last_success_at: Option<i64>,
    pub freshness: Freshness,
    pub coverage: Coverage,
}

fn freshness(last: Option<i64>, now: i64) -> Freshness {
    match last {
        None => Freshness::NeverFetched,
        Some(t) if t > now => Freshness::Unknown,
        Some(t) => match now - t {
            0..=129_600 => Freshness::Fresh,
            129_601..=259_200 => Freshness::Aging,
            _ => Freshness::Stale,
        },
    }
}

/// Bounded cached UTC intersection query for ONE saved XMLTV guide.
/// Calling code must dispatch off GTK MainContext and may set 'cancel' at any
/// time. A SQL page is bounded to 4,097 rows, then truncated to 4,096.
pub fn read_guide_window(
    conn: &Connection,
    source_id: &str,
    guide_channel_ids: &[String],
    start_utc: i64,
    end_utc: i64,
    now_utc: i64,
    cancel: &AtomicBool,
) -> Result<BoundedGuidePage, EpgError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(EpgError::Cancelled);
    }
    let width = end_utc
        .checked_sub(start_utc)
        .ok_or(EpgError::InvalidInput)?;
    if width <= 0
        || width > 172_800
        || source_id.is_empty()
        || source_id.len() > 64
        || guide_channel_ids.is_empty()
        || guide_channel_ids.len() > 64
    {
        return Err(EpgError::InvalidInput);
    }
    let mut ids = BTreeSet::new();
    for id in guide_channel_ids {
        if !bounded_text(id, 512) || !ids.insert(id.as_str()) {
            return Err(EpgError::InvalidInput);
        }
    }
    let selected: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    let source: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT enabled,last_success_at FROM epg_sources WHERE id=?1",
            [source_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db)?;
    let (enabled, last_success_at) = source.ok_or(EpgError::Unavailable)?;
    // Unknown IDs must not inherit the apparent coverage of unrelated channels.
    // The aggregate min/max describes selected channels only; it is not proof
    // that every selected channel or gap has complete listings.
    let ids_sql = (0..selected.len())
        .map(|i| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(",");
    let selected_args = std::iter::once(Value::Text(source_id.to_owned()))
        .chain(selected.iter().cloned().map(Value::Text))
        .collect::<Vec<_>>();
    let count_sql = format!(
        "SELECT COUNT(*) FROM epg_channels WHERE source_id=?1 AND channel_id IN ({ids_sql})"
    );
    let count: i64 = conn
        .query_row(&count_sql, params_from_iter(selected_args.iter()), |r| {
            r.get(0)
        })
        .map_err(db)?;
    if count != selected.len() as i64 {
        return Err(EpgError::InvalidInput);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(EpgError::Cancelled);
    }
    let bounds_sql = format!(
        "SELECT MIN(start_utc), MAX(stop_utc) FROM epg_programmes
         WHERE source_id=?1 AND channel_id IN ({ids_sql})"
    );
    let observed: (Option<i64>, Option<i64>) = conn
        .query_row(&bounds_sql, params_from_iter(selected_args.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(db)?;
    let coverage = if enabled == 0 {
        Coverage::Disabled
    } else if observed.0.is_none() {
        Coverage::NoData
    } else {
        Coverage::Partial {
            observed_start_utc: observed.0,
            observed_stop_utc: observed.1,
        }
    };

    // Indices are generated locally, never from untrusted programme fields.
    let placeholders = (0..selected.len())
        .map(|i| format!("?{}", i + 4))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT source_id,channel_id,start_utc,stop_utc,title,subtitle,description
         FROM epg_programmes
         WHERE source_id=?1 AND start_utc<?2 AND stop_utc>?3
           AND channel_id IN ({placeholders})
         ORDER BY source_id,channel_id,start_utc,stop_utc,title
         LIMIT 4097"
    );
    let mut args = vec![
        Value::Text(source_id.to_owned()),
        Value::Integer(end_utc),
        Value::Integer(start_utc),
    ];
    args.extend(selected.iter().cloned().map(Value::Text));
    if enabled == 0 {
        return Ok(BoundedGuidePage {
            source_id: source_id.to_owned(),
            selected_guide_channels: selected,
            start_utc,
            end_utc,
            programmes: Vec::new(),
            truncated: false,
            last_success_at,
            freshness: freshness(last_success_at, now_utc),
            coverage,
        });
    }
    let mut stmt = conn.prepare(&sql).map_err(db)?;
    let mut rows = stmt.query(params_from_iter(args.iter())).map_err(db)?;
    let mut programmes = Vec::new();
    while let Some(r) = rows.next().map_err(db)? {
        if cancel.load(Ordering::Relaxed) {
            return Err(EpgError::Cancelled);
        }
        programmes.push(Programme {
            source_id: r.get(0).map_err(db)?,
            channel_id: r.get(1).map_err(db)?,
            start_utc: r.get(2).map_err(db)?,
            stop_utc: r.get(3).map_err(db)?,
            title: r.get(4).map_err(db)?,
            subtitle: r.get(5).map_err(db)?,
            description: r.get(6).map_err(db)?,
        });
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(EpgError::Cancelled);
    }
    let truncated = programmes.len() > 4096;
    programmes.truncate(4096);
    Ok(BoundedGuidePage {
        source_id: source_id.to_owned(),
        selected_guide_channels: selected,
        start_utc,
        end_utc,
        programmes,
        truncated,
        last_success_at,
        freshness: freshness(last_success_at, now_utc),
        coverage,
    })
}

/// Validate the WHOLE snapshot before touching cached rows. A failed parse,
/// validation, cancellation or SQLite write leaves the previous success intact.
/// All original guide programmes (not just now/next) are stored atomically.
/// The original E1 import remains strict about equal retrieval times.
pub fn replace_guide_snapshot(
    conn: &mut Connection,
    snapshot: &GuideSnapshot,
    cancel: &AtomicBool,
) -> Result<(), EpgError> {
    replace_snapshot_inner(conn, snapshot, cancel, None)
}

/// E2's refresher supplies the monotonic metadata revision at admission.
/// Same-second wall-clock refreshes are allowed only while that exact
/// revision is still current; a competing write cannot evict newer data.
pub fn replace_guide_snapshot_if_current(
    conn: &mut Connection,
    snapshot: &GuideSnapshot,
    cancel: &AtomicBool,
    expected_revision: i64,
) -> Result<(), EpgError> {
    replace_snapshot_inner(conn, snapshot, cancel, Some(expected_revision))
}

fn replace_snapshot_inner(
    conn: &mut Connection,
    snapshot: &GuideSnapshot,
    cancel: &AtomicBool,
    expected_revision: Option<i64>,
) -> Result<(), EpgError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(EpgError::Cancelled);
    }
    validate_snapshot(snapshot)?;
    let mut unique = BTreeMap::new();
    for p in &snapshot.programmes {
        let key = (
            &p.source_id,
            &p.channel_id,
            p.start_utc,
            p.stop_utc,
            &p.title,
        );
        match unique.insert(key, p) {
            Some(other) if other != p => return Err(EpgError::InvalidSchedule),
            _ => {}
        }
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db)?;
    let persisted: Option<(i64, Option<i64>, i64)> = tx
        .query_row(
            "SELECT enabled,last_success_at,updated_at FROM epg_sources WHERE id=?1",
            [&snapshot.source_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(db)?;
    let (enabled, last_success, current_revision) = persisted.ok_or(EpgError::Unavailable)?;
    if enabled != 1 {
        return Err(EpgError::Unavailable);
    }
    // Preserve E1's original strict success-timestamp policy. E2 instead
    // checks the exact source revision, including competing same-second
    // refreshes, without falsifying the real last-success wall-clock time.
    match expected_revision {
        None if last_success.is_some_and(|last| snapshot.retrieved_utc <= last) => {
            return Err(EpgError::InvalidInput);
        }
        Some(expected)
            if current_revision != expected
                || last_success.is_some_and(|last| snapshot.retrieved_utc < last) =>
        {
            return Err(EpgError::InvalidInput);
        }
        _ => {}
    }
    tx.execute(
        "DELETE FROM epg_channels WHERE source_id=?1",
        [&snapshot.source_id],
    )
    .map_err(db)?;
    for channel in &snapshot.channels {
        if cancel.load(Ordering::Relaxed) {
            return Err(EpgError::Cancelled);
        }
        tx.execute(
            "INSERT INTO epg_channels(source_id,channel_id,display_name) VALUES (?1,?2,?3)",
            params![channel.source_id, channel.channel_id, channel.display_name],
        )
        .map_err(db)?;
    }
    for p in unique.values() {
        if cancel.load(Ordering::Relaxed) {
            return Err(EpgError::Cancelled);
        }
        tx.execute(
            "INSERT INTO epg_programmes
             (source_id,channel_id,start_utc,stop_utc,title,subtitle,description)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                p.source_id,
                p.channel_id,
                p.start_utc,
                p.stop_utc,
                p.title,
                p.subtitle,
                p.description
            ],
        )
        .map_err(db)?;
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(EpgError::Cancelled);
    }
    tx.execute(
        "UPDATE epg_sources SET last_success_at=?1,last_attempt_at=?1,
         last_error_code=NULL,updated_at=MAX(updated_at+1,?1) WHERE id=?2",
        params![snapshot.retrieved_utc, snapshot.source_id],
    )
    .map_err(db)?;
    tx.commit().map_err(db)
}

#[cfg(test)]
mod e2_revision_tests {
    use super::super::{
        model::{GuideChannel, GuideSnapshot, Programme},
        network::TrustScope,
        sources::{ConfirmedGuideDeletion, GuideSourceRegistry, RegistryError, SourceOrigin},
    };
    use super::*;
    use crate::streams::storage::Storage;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };
    use uuid::Uuid;

    fn registry() -> (PathBuf, GuideSourceRegistry) {
        let dir = std::env::temp_dir().join(format!("epg-revision-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let storage = Storage::at(dir.join("db.sqlite3"));
        storage.initialize().unwrap();
        (dir, GuideSourceRegistry::new(storage))
    }

    #[test]
    fn same_second_replacement_preserves_real_freshness_and_increments_cas() {
        let (dir, registry) = registry();
        let source = registry
            .add(
                "Clock",
                &SourceOrigin::entered_remote_url(
                    "https://example.com/guide".into(),
                    TrustScope::Public,
                )
                .unwrap(),
            )
            .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let snapshot = |title: &str| GuideSnapshot {
            source_id: source.id.clone(),
            channels: vec![GuideChannel {
                source_id: source.id.clone(),
                channel_id: "one".to_owned(),
                display_name: "One".to_owned(),
            }],
            programmes: vec![Programme {
                source_id: source.id.clone(),
                channel_id: "one".to_owned(),
                start_utc: now,
                stop_utc: now + 3600,
                title: title.to_owned(),
                subtitle: None,
                description: None,
            }],
            retrieved_utc: now,
        };
        let old_approval = ConfirmedGuideDeletion::after_user_confirmation(&source);
        let cancelled = AtomicBool::new(false);
        let mut conn = registry.connect_private().unwrap();
        replace_guide_snapshot_if_current(
            &mut conn,
            &snapshot("First"),
            &cancelled,
            source.updated_at,
        )
        .unwrap();
        let first = registry.get(&source.id).unwrap();
        assert_eq!(first.last_success_at, Some(now));
        assert!(first.updated_at > source.updated_at);

        // The admitted predecessor is still the same real second, but the
        // second full replacement gets an independent monotonic revision.
        replace_guide_snapshot_if_current(
            &mut conn,
            &snapshot("Second"),
            &cancelled,
            first.updated_at,
        )
            .unwrap();
        let second = registry.get(&source.id).unwrap();
        assert_eq!(second.last_success_at, Some(now));
        assert!(second.updated_at > first.updated_at);
        assert_eq!(freshness(second.last_success_at, now), Freshness::Fresh);
        assert_eq!(
            registry.delete_confirmed(&old_approval),
            Err(RegistryError::StaleConfirmation),
        );
        let saved: String = conn
            .query_row(
                "SELECT title FROM epg_programmes WHERE source_id=?1",
                [&source.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(saved, "Second");
        // A stale import cannot evict the last-good payload.
        assert!(replace_guide_snapshot_if_current(
            &mut conn,
            &snapshot("Rejected"),
            &cancelled,
            source.updated_at
        )
        .is_err());
        let saved: String = conn
            .query_row(
                "SELECT title FROM epg_programmes WHERE source_id=?1",
                [&source.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(saved, "Second");
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }
}

// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Transactional row-streamed XMLTV import on the existing SQLite-v2 schema.
//! Runs only on a blocking worker. A failed parse or cancelled import rolls
//! back all changes, retaining the prior complete guide for offline readers.
use super::{
    model::{EpgError, LimitKind},
    xmltv::{decode_xmltv_rows, StreamedRows, StreamingLimits},
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    hash::{Hash, Hasher},
    io::BufRead,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const MAX_OPERATION: Duration = Duration::from_secs(90 * 60);
// Reserve a full 2 GiB for the largest permitted plain-XML staging file.
// The active DB (old + replacement), WAL and input therefore fit <= 8 GiB.
const MAX_DATABASE_AND_WAL: u64 = 6 * 1024 * 1024 * 1024;

fn storage_error(_: rusqlite::Error) -> EpgError {
    EpgError::Storage
}

/// Import a fully source-authorized stream. The source adapter must separately
/// enforce the network/portal consent, transfer and staging-file quotas.
pub fn import_xmltv<R: BufRead>(
    conn: &mut Connection,
    input: R,
    source_id: &str,
    retrieved_utc: i64,
    expected_revision: i64,
    gzip: bool,
    cancelled: &AtomicBool,
) -> Result<StreamedRows, EpgError> {
    let started = Instant::now();
    if cancelled.load(Ordering::Acquire) {
        return Err(EpgError::Cancelled);
    }
    let database: String = conn
        .query_row("PRAGMA database_list", [], |r| r.get(2))
        .map_err(storage_error)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    let meta: Option<(i64, Option<i64>, i64)> = tx
        .query_row(
            "SELECT enabled,last_success_at,updated_at FROM epg_sources WHERE id=?1",
            [source_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(storage_error)?;
    let (enabled, last_success, revision) = meta.ok_or(EpgError::Unavailable)?;
    if enabled != 1 {
        return Err(EpgError::Unavailable);
    }
    if revision != expected_revision || last_success.is_some_and(|last| retrieved_utc < last) {
        return Err(EpgError::InvalidInput);
    }

    // XMLTV programmes may appear before their channel declarations. In this
    // transaction only, defer FKs until all channels have been verified.
    tx.execute_batch("PRAGMA defer_foreign_keys=ON;")
        .map_err(storage_error)?;
    tx.execute("DELETE FROM epg_channels WHERE source_id=?1", [source_id])
        .map_err(storage_error)?;

    let mut inserted = 0_usize;
    // A shortened title can collide with a different full title after 512
    // bytes. Track only keys of truncated rows so SQLite IGNORE never
    // silently merges two programme identities with the same prefix.
    let mut truncated_keys = HashSet::<u64>::new();
    let check_budget = || -> Result<(), EpgError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(EpgError::Cancelled);
        }
        if started.elapsed() >= MAX_OPERATION {
            return Err(EpgError::Capacity(LimitKind::Duration));
        }
        if !database.is_empty() {
            let db = std::fs::metadata(&database).map(|m| m.len()).unwrap_or(0);
            let wal = std::fs::metadata(format!("{database}-wal"))
                .map(|m| m.len())
                .unwrap_or(0);
            if db.saturating_add(wal) > MAX_DATABASE_AND_WAL {
                return Err(EpgError::Capacity(LimitKind::Database));
            }
        }
        Ok(())
    };
    let rows = decode_xmltv_rows(
        input,
        source_id,
        retrieved_utc,
        gzip,
        StreamingLimits::candidate(),
        cancelled,
        |c| {
            check_budget()?;
            tx.execute(
                "INSERT INTO epg_channels(source_id,channel_id,display_name) VALUES (?1,?2,?3)",
                params![c.source_id, c.channel_id, c.display_name],
            )
            .map_err(storage_error)?;
            Ok(())
        },
        |p, title_truncated| {
            inserted += 1;
            if inserted.is_multiple_of(1024) {
                check_budget()?;
            }
            let mut hasher = DefaultHasher::new();
            (&p.channel_id, p.start_utc, p.stop_utc, &p.title).hash(&mut hasher);
            let row_key = hasher.finish();
            let inserted_row = tx
                .execute(
                    "INSERT OR IGNORE INTO epg_programmes
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
                .map_err(storage_error)?;
            if inserted_row == 0 {
                // A truncated and a longer original title may share a key
                // despite distinct meaning. Reject this ambiguous schedule
                // rather than treating it as an identical duplicate.
                if title_truncated || truncated_keys.contains(&row_key) {
                    return Err(EpgError::InvalidSchedule);
                }
                // Match the legacy sorted snapshot's conflict semantics:
                // byte-identical duplicates deduplicate; different metadata
                // for the same identity is an invalid schedule, not silent loss.
                let prior: (Option<String>, Option<String>) = tx
                    .query_row(
                        "SELECT subtitle,description FROM epg_programmes
                     WHERE source_id=?1 AND channel_id=?2 AND start_utc=?3
                       AND stop_utc=?4 AND title=?5",
                        params![p.source_id, p.channel_id, p.start_utc, p.stop_utc, p.title],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .map_err(storage_error)?;
                if prior != (p.subtitle, p.description) {
                    return Err(EpgError::InvalidSchedule);
                }
            } else if title_truncated {
                truncated_keys.insert(row_key);
            }
            Ok(())
        },
    )?;
    check_budget()?;

    // Deferred FKs protect commit. This explicit check supplies the same
    // typed MissingChannel diagnostic as the legacy snapshot validator.
    let dangling: i64 = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM epg_programmes p
         LEFT JOIN epg_channels c ON c.source_id=p.source_id AND c.channel_id=p.channel_id
         WHERE p.source_id=?1 AND c.channel_id IS NULL LIMIT 1)",
            [source_id],
            |r| r.get(0),
        )
        .map_err(storage_error)?;
    if dangling != 0 {
        return Err(EpgError::MissingChannel);
    }
    tx.execute(
        "UPDATE epg_sources SET last_success_at=?1,last_attempt_at=?1,
         last_error_code=NULL,updated_at=MAX(updated_at+1,?1) WHERE id=?2",
        params![retrieved_utc, source_id],
    )
    .map_err(storage_error)?;
    check_budget()?;
    tx.commit().map_err(storage_error)?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streams::storage::{MIGRATION_001, MIGRATION_002};
    use std::io::Cursor;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_001).unwrap();
        conn.execute_batch(MIGRATION_002).unwrap();
        conn.execute(
            "INSERT INTO epg_sources
            (id,name,origin_kind,origin_locator,created_at,updated_at)
            VALUES ('source','Fabricated','local_file','/synthetic',1,1)",
            [],
        )
        .unwrap();
        conn
    }
    fn xml(title: &str) -> Vec<u8> {
        format!(
            "<tv><channel id='c'><display-name>Example</display-name></channel>
            <programme channel='c' start='20300101000000 +0000'
             stop='20300101010000 +0000'><title>{title}</title></programme></tv>"
        )
        .into_bytes()
    }

    #[test]
    fn streamed_plain_import_commits_complete_guide_and_rolls_back_invalid_refresh() {
        let mut conn = db();
        let cancel = AtomicBool::new(false);
        let stats = import_xmltv(
            &mut conn,
            Cursor::new(xml("Old")),
            "source",
            1,
            1,
            false,
            &cancel,
        )
        .unwrap();
        assert_eq!(
            stats,
            StreamedRows {
                channels: 1,
                programmes: 1,
                truncated_descriptions: 0,
                truncated_titles: 0,
                truncated_subtitles: 0,
                truncated_channel_names: 0,
            }
        );
        let failed = import_xmltv(
            &mut conn,
            Cursor::new(
                b"<tv><channel id='c'><display-name>X</display-name></channel><programme".to_vec(),
            ),
            "source",
            2,
            2,
            false,
            &cancel,
        );
        assert_eq!(failed.unwrap_err(), EpgError::InvalidInput);
        let title: String = conn
            .query_row("SELECT title FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Old");
    }

    #[test]
    fn validates_forward_references_and_conflicting_programme_duplicates() {
        let mut conn = db();
        let cancel = AtomicBool::new(false);
        let before = b"<tv><programme channel='c' start='20300101000000 +0000' stop='20300101010000 +0000'><title>A</title></programme><channel id='c'><display-name>Example</display-name></channel></tv>";
        let outcome = import_xmltv(
            &mut conn,
            Cursor::new(before),
            "source",
            1,
            1,
            false,
            &cancel,
        );
        assert!(outcome.is_ok(), "{outcome:?}");
        let conflicting = b"<tv><channel id='c'><display-name>Example</display-name></channel><programme channel='c' start='20300101000000 +0000' stop='20300101010000 +0000'><title>A</title><desc>One</desc></programme><programme channel='c' start='20300101000000 +0000' stop='20300101010000 +0000'><title>A</title><desc>Two</desc></programme></tv>";
        assert_eq!(
            import_xmltv(
                &mut conn,
                Cursor::new(conflicting),
                "source",
                2,
                2,
                false,
                &cancel
            )
            .unwrap_err(),
            EpgError::InvalidSchedule
        );
        let original: (i64, Option<String>) = conn
            .query_row(
                "SELECT COUNT(*), MIN(description) FROM epg_programmes",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(original, (1, None));
    }
    #[test]
    fn truncation_commits_all_rows_and_rolls_back_on_later_invalid_xml() {
        let mut conn = db();
        let cancelled = AtomicBool::new(false);
        let good = format!(
            "<tv><channel id='c'><display-name>Example</display-name></channel>\
             <programme channel='c' start='20300101000000 +0000'\
             stop='20300101010000 +0000'><title>One</title><desc>{}</desc></programme>\
             <programme channel='c' start='20300101010000 +0000'\
             stop='20300101020000 +0000'><title>Two</title></programme></tv>",
            "é".repeat(1400),
        );
        let rows = import_xmltv(
            &mut conn,
            Cursor::new(good.as_bytes()),
            "source",
            1,
            1,
            false,
            &cancelled,
        )
        .unwrap();
        assert_eq!(rows.programmes, 2);
        assert_eq!(rows.truncated_descriptions, 1);
        let synopsis: String = conn
            .query_row(
                "SELECT description FROM epg_programmes WHERE title='One'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(synopsis.len(), 2048);
        assert!(synopsis.ends_with('é'));
        let broken = format!(
            "<tv><channel id='c'><display-name>Example</display-name></channel>\
             <programme channel='c' start='20300101000000 +0000'\
             stop='20300101010000 +0000'><title>One</title><desc>{}</desc></programme>\
             <!DOCTYPE foo>",
            "x".repeat(4000),
        );
        assert_eq!(
            import_xmltv(
                &mut conn,
                Cursor::new(broken.as_bytes()),
                "source",
                2,
                2,
                false,
                &cancelled
            )
            .unwrap_err(),
            EpgError::InvalidInput,
        );
        let persisted: i64 = conn
            .query_row("SELECT COUNT(*) FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(persisted, 2);
        let still: String = conn
            .query_row(
                "SELECT description FROM epg_programmes WHERE title='One'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still, synopsis);
    }
    #[test]
    fn display_field_truncation_commits_preserved_ids_and_schedule() {
        let mut conn = db();
        let cancelled = AtomicBool::new(false);
        let name = format!("{}é", "N".repeat(511));
        let title = format!("{}é", "T".repeat(511));
        let subtitle = format!("{}é", "S".repeat(511));
        let xml = format!(
            "<tv><channel id='unchanged.id'><display-name>{name}</display-name></channel>\
             <programme channel='unchanged.id' start='20300101000000 +0000'\
             stop='20300101010000 +0000'><title>{title}</title>\
             <sub-title>{subtitle}</sub-title></programme></tv>"
        );
        let stats = import_xmltv(
            &mut conn,
            Cursor::new(xml.as_bytes()),
            "source",
            1,
            1,
            false,
            &cancelled,
        )
        .unwrap();
        assert_eq!(stats.programmes, 1);
        assert_eq!(stats.truncated_titles, 1);
        assert_eq!(stats.truncated_subtitles, 1);
        assert_eq!(stats.truncated_channel_names, 1);
        let (id, stored_name): (String, String) = conn
            .query_row(
                "SELECT channel_id,display_name FROM epg_channels",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(id, "unchanged.id");
        assert_eq!(stored_name.len(), 511);
        let (id, begin, end, stored_title, sub): (String, i64, i64, String, String) = conn
            .query_row(
                "SELECT channel_id,start_utc,stop_utc,title,subtitle FROM epg_programmes",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(id, "unchanged.id");
        assert_eq!(end - begin, 3600);
        assert_eq!(stored_title.len(), 511);
        assert_eq!(sub.len(), 511);
    }

    #[test]
    fn colliding_truncated_title_rolls_back_and_keeps_previous_guide() {
        let mut conn = db();
        let cancelled = AtomicBool::new(false);
        import_xmltv(
            &mut conn,
            Cursor::new(xml("Last-good")),
            "source",
            1,
            1,
            false,
            &cancelled,
        )
        .unwrap();
        let prefix = "X".repeat(512);
        let bad = format!(
            "<tv><channel id='c'><display-name>C</display-name></channel>\
             <programme channel='c' start='20300101000000 +0000'\
             stop='20300101010000 +0000'><title>{prefix}a</title></programme>\
             <programme channel='c' start='20300101000000 +0000'\
             stop='20300101010000 +0000'><title>{prefix}b</title></programme></tv>"
        );
        assert_eq!(
            import_xmltv(
                &mut conn,
                Cursor::new(bad.as_bytes()),
                "source",
                2,
                2,
                false,
                &cancelled,
            )
            .unwrap_err(),
            EpgError::InvalidSchedule,
        );
        let stored: String = conn
            .query_row("SELECT title FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "Last-good");
    }

    #[test]
    fn strict_oversized_channel_id_still_fails_and_preserves_last_good() {
        let mut conn = db();
        let cancelled = AtomicBool::new(false);
        import_xmltv(
            &mut conn,
            Cursor::new(xml("Last-good")),
            "source",
            1,
            1,
            false,
            &cancelled,
        )
        .unwrap();
        let id = "c".repeat(513);
        let malformed = format!(
            "<tv><channel id='{id}'><display-name>Long ID</display-name></channel>\
             <programme channel='{id}' start='20300101000000 +0000'\
             stop='20300101010000 +0000'><title>Show</title></programme></tv>"
        );
        assert!(import_xmltv(
            &mut conn,
            Cursor::new(malformed.as_bytes()),
            "source",
            2,
            2,
            false,
            &cancelled,
        )
        .is_err());
        let stored: String = conn
            .query_row("SELECT title FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "Last-good");
    }
}

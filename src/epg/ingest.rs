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
    io::BufRead,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const MAX_OPERATION: Duration = Duration::from_secs(90 * 60);
// Reserve a full 2 GiB for the largest permitted plain-XML staging file.
// The active DB (old + replacement), WAL and input therefore fit <= 8 GiB.
const MAX_DATABASE_AND_WAL: u64 = 6 * 1024 * 1024 * 1024;

#[cfg(test)]
std::thread_local! {
    // Test-only observation made immediately after SQLite accepts a row.
    static TEST_STAGED_INSERTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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

    // Track only keys involving any shortened display metadata. A TEMP table
    // gives exact SQLite-key identity without retaining unbounded original
    // descriptions in Rust memory or changing the persistent v2 schema.
    // It is dropped before a successful commit and rolled back on failure.
    tx.execute_batch(
        "CREATE TEMP TABLE epg_import_truncated_keys (
            channel_id TEXT NOT NULL,
            start_utc INTEGER NOT NULL,
            stop_utc INTEGER NOT NULL,
            title TEXT NOT NULL,
            PRIMARY KEY(channel_id,start_utc,stop_utc,title)
         ) WITHOUT ROWID;",
    )
    .map_err(storage_error)?;
    let mut parsed = 0_usize;
    let mut retained = 0_usize;
    let mut retained_title_shortened = 0_usize;
    let mut retained_subtitle_shortened = 0_usize;
    let mut retained_description_shortened = 0_usize;
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
    let mut rows = decode_xmltv_rows(
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
        |p, shortened| {
            parsed += 1;
            if parsed.is_multiple_of(1024) {
                check_budget()?;
            }
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
                // Once *any* field was shortened, the original values cannot
                // be reconstructed from the stored row. Reject both arrival
                // orders, including a shortened row followed by a plain row.
                if shortened.any() {
                    return Err(EpgError::InvalidSchedule);
                }
                let prior_shortened: i64 = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM temp.epg_import_truncated_keys
                         WHERE channel_id=?1 AND start_utc=?2 AND stop_utc=?3
                           AND title=?4)",
                        params![p.channel_id, p.start_utc, p.stop_utc, p.title],
                        |r| r.get(0),
                    )
                    .map_err(storage_error)?;
                if prior_shortened != 0 {
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
            } else {
                retained += 1;
                #[cfg(test)]
                TEST_STAGED_INSERTS.with(|count| count.set(count.get() + 1));
                retained_title_shortened += usize::from(shortened.title);
                retained_subtitle_shortened += usize::from(shortened.subtitle);
                retained_description_shortened += usize::from(shortened.description);
                if shortened.any() {
                    tx.execute(
                        "INSERT INTO temp.epg_import_truncated_keys
                         (channel_id,start_utc,stop_utc,title)
                         VALUES (?1,?2,?3,?4)",
                        params![p.channel_id, p.start_utc, p.stop_utc, p.title],
                    )
                    .map_err(storage_error)?;
                }
            }
            Ok(())
        },
    )?;
    // The parser saw candidates, but only committed, unique programme rows
    // contribute to the displayed retained-row counts.
    rows.programmes = retained;
    rows.truncated_titles = retained_title_shortened;
    rows.truncated_subtitles = retained_subtitle_shortened;
    rows.truncated_descriptions = retained_description_shortened;
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
    tx.execute_batch("DROP TABLE temp.epg_import_truncated_keys;")
        .map_err(storage_error)?;
    tx.commit().map_err(storage_error)?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streams::storage::{MIGRATION_001, MIGRATION_002};
    use std::{
        cell::Cell,
        io::{self, BufReader, Cursor, Read},
        rc::Rc,
    };

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

    #[test]
    fn shortened_optional_metadata_collisions_fail_atomically_in_both_orders() {
        for (field, cap) in [("sub-title", 512), ("desc", 2048)] {
            for reverse in [false, true] {
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
                let prefix = "A".repeat(cap);
                let record = |suffix: &str| {
                    format!(
                        "<programme channel='c' start='20300101000000 +0000'\
                     stop='20300101010000 +0000'><title>Show</title>\
                     <{field}>{prefix}{suffix}</{field}></programme>"
                    )
                };
                let a = record("one");
                let b = record("two");
                let (first, second) = if reverse { (&b, &a) } else { (&a, &b) };
                let bad = format!(
                    "<tv><channel id='c'><display-name>C</display-name></channel>\
                     {first}{second}</tv>"
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
                let title: String = conn
                    .query_row("SELECT title FROM epg_programmes", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(title, "Last-good");
            }
        }
    }

    #[test]
    fn shortened_vs_unshortened_duplicate_key_is_rejected_in_both_orders() {
        for field in ["title", "sub-title", "desc"] {
            for reverse in [false, true] {
                let mut conn = db();
                let cancelled = AtomicBool::new(false);
                let cap = if field == "desc" { 2048 } else { 512 };
                let prefix = "K".repeat(cap);
                let programme = |extra: &str| {
                    format!(
                        "<programme channel='c' start='20300101000000 +0000'\
                     stop='20300101010000 +0000'><title>{}</title>{}</programme>",
                        if field == "title" {
                            format!("{prefix}{extra}")
                        } else {
                            "Show".to_string()
                        },
                        if field == "title" {
                            String::new()
                        } else {
                            format!("<{field}>{prefix}{extra}</{field}>")
                        }
                    )
                };
                let a = programme("");
                let b = programme("extra");
                let (first, second) = if reverse { (&b, &a) } else { (&a, &b) };
                let bad = format!(
                    "<tv><channel id='c'><display-name>C</display-name></channel>\
                     {first}{second}</tv>"
                );
                assert_eq!(
                    import_xmltv(
                        &mut conn,
                        Cursor::new(bad.as_bytes()),
                        "source",
                        1,
                        1,
                        false,
                        &cancelled,
                    )
                    .unwrap_err(),
                    EpgError::InvalidSchedule,
                );
                let count: i64 = conn
                    .query_row("SELECT COUNT(*) FROM epg_programmes", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(count, 0);
            }
        }
    }

    #[test]
    fn retained_rows_not_duplicate_candidates_drive_warning_counts() {
        let mut conn = db();
        let cancel = AtomicBool::new(false);
        let duplicate = "<programme channel='c' start='20300101000000 +0000'\
            stop='20300101010000 +0000'><title>Repeated</title></programme>";
        let long = "D".repeat(2049);
        let guide = format!(
            "<tv><channel id='c'><display-name>C</display-name></channel>\
             {duplicate}{duplicate}<programme channel='c' start='20300101010000 +0000'\
             stop='20300101020000 +0000'><title>Unique</title><desc>{long}</desc></programme></tv>"
        );
        let stats = import_xmltv(
            &mut conn,
            Cursor::new(guide.as_bytes()),
            "source",
            1,
            1,
            false,
            &cancel,
        )
        .unwrap();
        assert_eq!(stats.programmes, 2);
        assert_eq!(stats.truncated_descriptions, 1);
        assert_eq!(stats.truncated_titles, 0);
        assert_eq!(stats.truncated_subtitles, 0);
        let stored: i64 = conn
            .query_row("SELECT COUNT(*) FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 2);
    }

    #[test]
    fn cancelled_refresh_keeps_last_good_guide() {
        let mut conn = db();
        let cancel = AtomicBool::new(false);
        import_xmltv(
            &mut conn,
            Cursor::new(xml("Cached")),
            "source",
            1,
            1,
            false,
            &cancel,
        )
        .unwrap();
        cancel.store(true, Ordering::Release);
        assert_eq!(
            import_xmltv(
                &mut conn,
                Cursor::new(xml("Replacement")),
                "source",
                2,
                2,
                false,
                &cancel,
            )
            .unwrap_err(),
            EpgError::Cancelled
        );
        let title: String = conn
            .query_row("SELECT title FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Cached");
    }

    /// The reader exposes the closing programme tag before its trailing XML.
    /// Its next read only cancels after the importer's SQLite INSERT returned.
    struct CancelAfterSqlInsert<'a> {
        bytes: Vec<u8>,
        split: usize,
        position: usize,
        cancelled: &'a AtomicBool,
        fired: Rc<Cell<bool>>,
    }

    impl Read for CancelAfterSqlInsert<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if output.is_empty() {
                return Ok(0);
            }
            if self.position == self.split && !self.fired.get() {
                let inserted = TEST_STAGED_INSERTS.with(|count| count.get());
                assert_eq!(
                    inserted, 1,
                    "the cancellation gate must follow one completed SQLite INSERT"
                );
                self.cancelled.store(true, Ordering::Release);
                self.fired.set(true);
            }
            let boundary = if self.position < self.split {
                self.split
            } else {
                self.bytes.len()
            };
            let n = output.len().min(boundary - self.position);
            output[..n].copy_from_slice(&self.bytes[self.position..self.position + n]);
            self.position += n;
            Ok(n)
        }
    }

    #[test]
    fn cancellation_after_sql_insert_rolls_back_temp_and_allows_same_connection_retry() {
        let mut conn = db();
        let cancel = AtomicBool::new(false);
        import_xmltv(
            &mut conn,
            Cursor::new(xml("Cached")),
            "source",
            1,
            1,
            false,
            &cancel,
        )
        .unwrap();

        let original_channel: (String, String) = conn
            .query_row(
                "SELECT channel_id, display_name FROM epg_channels WHERE source_id='source'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let original_programme: (String, i64, i64, String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT channel_id, start_utc, stop_utc, title, subtitle, description
                 FROM epg_programmes WHERE source_id='source'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        let original_metadata: (Option<i64>, Option<i64>, i64, Option<String>) = conn
            .query_row(
                "SELECT last_success_at,last_attempt_at,updated_at,last_error_code
                 FROM epg_sources WHERE id='source'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();

        // An overlong title also inserts a key into the transaction's TEMP
        // table; rollback must discard that table along with the new rows.
        let replacement = format!(
            "<tv><channel id='c'><display-name>Replacement</display-name></channel>\
             <programme channel='c' start='20300101000000 +0000'\
             stop='20300101010000 +0000'><title>{}</title></programme></tv>",
            "T".repeat(513)
        );
        let split = replacement.find("</programme>").unwrap() + "</programme>".len();
        TEST_STAGED_INSERTS.with(|count| count.set(0));
        let fired = Rc::new(Cell::new(false));
        let gate = CancelAfterSqlInsert {
            bytes: replacement.into_bytes(),
            split,
            position: 0,
            cancelled: &cancel,
            fired: Rc::clone(&fired),
        };
        let failed = import_xmltv(
            &mut conn,
            BufReader::new(gate),
            "source",
            2,
            2,
            false,
            &cancel,
        );
        assert_eq!(failed.unwrap_err(), EpgError::Cancelled);
        assert!(
            fired.get(),
            "cancellation must occur after an actual staged INSERT"
        );
        assert_eq!(TEST_STAGED_INSERTS.with(|count| count.get()), 1);
        let retained_channel: (String, String) = conn
            .query_row(
                "SELECT channel_id,display_name FROM epg_channels WHERE source_id='source'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let retained_programme: (String, i64, i64, String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT channel_id,start_utc,stop_utc,title,subtitle,description
                 FROM epg_programmes WHERE source_id='source'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        let retained_metadata: (Option<i64>, Option<i64>, i64, Option<String>) = conn
            .query_row(
                "SELECT last_success_at,last_attempt_at,updated_at,last_error_code
                 FROM epg_sources WHERE id='source'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(retained_channel, original_channel);
        assert_eq!(retained_programme, original_programme);
        assert_eq!(retained_metadata, original_metadata);
        let (channels, programmes): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM epg_channels WHERE source_id='source'),
                        (SELECT COUNT(*) FROM epg_programmes WHERE source_id='source')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((channels, programmes), (1, 1));
        assert_eq!(original_programme.3, "Cached");
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master
                 WHERE name='epg_import_truncated_keys'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);

        // The cancelled transaction must not leave connection-local SQL state
        // that blocks a subsequent valid import.
        cancel.store(false, Ordering::Release);
        let retried = import_xmltv(
            &mut conn,
            Cursor::new(xml("Retried")),
            "source",
            3,
            2,
            false,
            &cancel,
        )
        .unwrap();
        assert_eq!(retried.programmes, 1);
        assert_eq!(retried.truncated_titles, 0);
        let title: String = conn
            .query_row("SELECT title FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Retried");
        let success_at: Option<i64> = conn
            .query_row(
                "SELECT last_success_at FROM epg_sources WHERE id='source'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(success_at, Some(3));
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master
                 WHERE name='epg_import_truncated_keys'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);
    }
}

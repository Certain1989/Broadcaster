// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Transactional row-streamed XMLTV import on the existing SQLite-v2 schema.
//! Runs only on a blocking worker. A failed parse or cancelled import rolls
//! back all changes, retaining the prior complete guide for offline readers.
use super::{
    headroom::ImportHeadroom,
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
// Cap only the main database plus WAL. Staging and SQLite TEMP may use
// separate filesystems; this does not impose a total disk-footprint bound.
const MAX_DATABASE_AND_WAL: u64 = 6 * 1024 * 1024 * 1024;

#[cfg(test)]
std::thread_local! {
    // Test-only observation made immediately after SQLite accepts a row.
    static TEST_STAGED_INSERTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TEST_STAGED_MARKERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct O01Observation {
    pub sqlite_version: String,
    pub compile_temp_store: Option<String>,
    pub temp_store: i64,
    pub temp_pages: i64,
    pub temp_page_size: i64,
    pub parsed: usize,
    pub retained: usize,
    pub marked: usize,
    pub elapsed_ms: u128,
    pub db_bytes: u64,
    pub wal_bytes: u64,
    pub rss_kib: Option<u64>,
    pub hwm_kib: Option<u64>,
    pub temp_fd_bytes: Option<u64>,
}

#[cfg(test)]
std::thread_local! {
    // Opt-in only. No SQLite PRAGMAs, /proc reads, or per-row probes in normal tests.
    static TEST_O01_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEST_O01_OBSERVATION: std::cell::RefCell<Option<O01Observation>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn enable_o01_measurement() {
    TEST_O01_ENABLED.with(|enabled| enabled.set(true));
    TEST_O01_OBSERVATION.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(test)]
pub(super) fn take_o01_observation() -> Option<O01Observation> {
    TEST_O01_ENABLED.with(|enabled| enabled.set(false));
    TEST_O01_OBSERVATION.with(|slot| slot.borrow_mut().take())
}

#[cfg(test)]
fn observe_o01(
    tx: &rusqlite::Transaction<'_>,
    database: &str,
    parsed: usize,
    retained: usize,
    marked: usize,
    started: Instant,
) -> O01Observation {
    let scalar = |sql| tx.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let status_kib = |key: &str| {
        status.lines().find_map(|line| {
            line.strip_prefix(key)?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
    };
    // SQLite's deleted-open FILE-mode TEMP descriptor: st_size is a sampled
    // logical file length, not allocated disk blocks or an observed peak.
    #[cfg(target_os = "linux")]
    let temp_fd_bytes = std::fs::read_dir("/proc/self/fd").ok().map(|entries| {
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let link = std::fs::read_link(entry.path()).ok()?;
                let name = link.to_string_lossy();
                (name.contains("etilqs_") || name.contains("sqlite-temp"))
                    .then(|| std::fs::metadata(entry.path()).ok().map(|m| m.len()))
                    .flatten()
            })
            .sum()
    });
    #[cfg(not(target_os = "linux"))]
    let temp_fd_bytes = None;
    let disk_bytes = |suffix: &str| -> u64 {
        if database.is_empty() {
            0
        } else {
            std::fs::metadata(format!("{database}{suffix}"))
                .map(|m| m.len())
                .unwrap_or(0)
        }
    };
    O01Observation {
        sqlite_version: tx
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))
            .unwrap(),
        compile_temp_store: tx
            .query_row(
                "SELECT compile_options FROM pragma_compile_options
                 WHERE compile_options LIKE 'TEMP_STORE=%' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap(),
        temp_store: scalar("PRAGMA temp_store"),
        temp_pages: scalar("PRAGMA temp.page_count"),
        temp_page_size: scalar("PRAGMA temp.page_size"),
        parsed,
        retained,
        marked,
        elapsed_ms: started.elapsed().as_millis(),
        db_bytes: disk_bytes(""),
        wal_bytes: disk_bytes("-wal"),
        rss_kib: status_kib("VmRSS:"),
        hwm_kib: status_kib("VmHWM:"),
        temp_fd_bytes,
    }
}

fn storage_error(error: rusqlite::Error) -> EpgError {
    match error {
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DiskFull =>
        {
            EpgError::DiskFull
        }
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                rusqlite::ErrorCode::PermissionDenied | rusqlite::ErrorCode::ReadOnly
            ) =>
        {
            EpgError::StoragePermission
        }
        _ => EpgError::Storage,
    }
}

pub(crate) struct ImportChecks<'a> {
    pub cancelled: &'a AtomicBool,
    pub headroom: Option<&'a ImportHeadroom>,
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
    import_xmltv_inner(
        conn,
        input,
        source_id,
        retrieved_utc,
        expected_revision,
        gzip,
        ImportChecks {
            cancelled,
            headroom: None,
        },
    )
}

/// Import with best-effort filesystem observations. All SQLite changes still
/// take place in one atomic transaction with the existing hard quotas.
pub(crate) fn import_xmltv_guarded<R: BufRead>(
    conn: &mut Connection,
    input: R,
    source_id: &str,
    retrieved_utc: i64,
    expected_revision: i64,
    gzip: bool,
    checks: ImportChecks<'_>,
) -> Result<StreamedRows, EpgError> {
    import_xmltv_inner(
        conn,
        input,
        source_id,
        retrieved_utc,
        expected_revision,
        gzip,
        checks,
    )
}

fn import_xmltv_inner<R: BufRead>(
    conn: &mut Connection,
    input: R,
    source_id: &str,
    retrieved_utc: i64,
    expected_revision: i64,
    gzip: bool,
    checks: ImportChecks<'_>,
) -> Result<StreamedRows, EpgError> {
    let cancelled = checks.cancelled;
    let headroom = checks.headroom;
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

    // The migrated epg_programmes table MUST remain an ordinary rowid table.
    // Its complete BINARY composite primary key stays in main SQLite, while
    // TEMP stores only rowids of freshly inserted rows with shortened metadata.
    // RETURNING rowid makes successful insertion unambiguous even after IGNORE.
    // TEMP is created in this transaction, dropped before commit or rolled back.
    tx.execute_batch(
        "CREATE TEMP TABLE epg_import_truncated_rowids (
            programme_rowid INTEGER PRIMARY KEY
         );",
    )
    .map_err(storage_error)?;
    #[cfg(test)]
    let mut marked = 0_usize;
    let mut parsed = 0_usize;
    let mut retained = 0_usize;
    let mut retained_title_shortened = 0_usize;
    let mut retained_subtitle_shortened = 0_usize;
    let mut retained_description_shortened = 0_usize;
    let mut conflicting_duplicate_rows = 0_usize;
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
            if parsed.is_multiple_of(4096) && headroom.is_some_and(|guard| !guard.active()) {
                return Err(EpgError::DiskFull);
            }
            let inserted_rowid: Option<i64> = tx
                .query_row(
                    "INSERT OR IGNORE INTO epg_programmes
                     (source_id,channel_id,start_utc,stop_utc,title,subtitle,description)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)
                     RETURNING rowid",
                    params![
                        p.source_id,
                        p.channel_id,
                        p.start_utc,
                        p.stop_utc,
                        p.title,
                        p.subtitle,
                        p.description
                    ],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage_error)?;
            if let Some(programme_rowid) = inserted_rowid {
                retained += 1;
                #[cfg(test)]
                TEST_STAGED_INSERTS.with(|count| count.set(count.get() + 1));
                retained_title_shortened += usize::from(shortened.title);
                retained_subtitle_shortened += usize::from(shortened.subtitle);
                retained_description_shortened += usize::from(shortened.description);
                if shortened.any() {
                    tx.execute(
                        "INSERT INTO temp.epg_import_truncated_rowids (programme_rowid)
                         VALUES (?1)",
                        [programme_rowid],
                    )
                    .map_err(storage_error)?;
                    #[cfg(test)]
                    {
                        marked += 1;
                    }
                    #[cfg(test)]
                    TEST_STAGED_MARKERS.with(|count| count.set(count.get() + 1));
                }
            } else {
                // Only a genuinely ignored insertion follows this path; never
                // use last_insert_rowid(), which could belong to another row.
                // Resolve the full composite PK with SQLite's BINARY identity,
                // including source_id, before checking the TEMP marker.
                if shortened.any() {
                    return Err(EpgError::InvalidSchedule);
                }
                let prior: (i64, Option<String>, Option<String>) = tx
                    .query_row(
                        "SELECT rowid,subtitle,description FROM epg_programmes
                         WHERE source_id=?1 AND channel_id=?2 AND start_utc=?3
                           AND stop_utc=?4 AND title=?5",
                        params![p.source_id, p.channel_id, p.start_utc, p.stop_utc, p.title],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(storage_error)?;
                let prior_shortened: i64 = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM temp.epg_import_truncated_rowids
                         WHERE programme_rowid=?1)",
                        [prior.0],
                        |r| r.get(0),
                    )
                    .map_err(storage_error)?;
                // Shortened metadata can hide a distinct complete title or
                // differences beyond the retained prefix: reject it as before.
                if prior_shortened != 0 {
                    return Err(EpgError::InvalidSchedule);
                }
                let subtitle_conflict = prior.1 != p.subtitle;
                let description_conflict = prior.2 != p.description;
                if subtitle_conflict || description_conflict {
                    // NULL is absorbing; a later duplicate cannot restore a
                    // field omitted because earlier metadata disagreed.
                    tx.execute(
                        "UPDATE epg_programmes
                         SET subtitle=CASE WHEN ?1 THEN NULL ELSE subtitle END,
                             description=CASE WHEN ?2 THEN NULL ELSE description END
                         WHERE rowid=?3",
                        params![subtitle_conflict, description_conflict, prior.0],
                    )
                    .map_err(storage_error)?;
                    conflicting_duplicate_rows = conflicting_duplicate_rows.saturating_add(1);
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
    rows.conflicting_duplicate_rows = conflicting_duplicate_rows;
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
    #[cfg(test)]
    if TEST_O01_ENABLED.with(|enabled| enabled.get()) {
        let observation = observe_o01(&tx, &database, parsed, retained, marked, started);
        TEST_O01_OBSERVATION.with(|slot| *slot.borrow_mut() = Some(observation));
    }
    tx.execute_batch("DROP TABLE temp.epg_import_truncated_rowids;")
        .map_err(storage_error)?;
    // Final check after all SQL writes and TEMP cleanup, before COMMIT.
    // SQLite's typed FULL/IOERR results remain the final defence.
    if headroom.is_some_and(|guard| !guard.active()) {
        return Err(EpgError::DiskFull);
    }
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
    fn sqlite_full_after_staged_insert_preserves_last_good_and_same_connection_retry() {
        // A bounded, isolated real SQLite SQLITE_FULL probe. Never exhaust the
        // runner's filesystem: SQLite's logical max_page_count is the limiter.
        let dir =
            std::env::temp_dir().join(format!("broadcaster-epg-full-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let mut conn = Connection::open(dir.join("fixture.sqlite3")).unwrap();
        conn.execute_batch(MIGRATION_001).unwrap();
        conn.execute_batch(MIGRATION_002).unwrap();
        conn.execute(
            "INSERT INTO epg_sources
             (id,name,origin_kind,origin_locator,created_at,updated_at)
             VALUES ('source','Fabricated','local_file','/synthetic',1,1)",
            [],
        )
        .unwrap();
        let cancelled = AtomicBool::new(false);
        import_xmltv(
            &mut conn,
            Cursor::new(xml("Cached")),
            "source",
            1,
            1,
            false,
            &cancelled,
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
        let original_channel: String = conn
            .query_row("SELECT display_name FROM epg_channels", [], |r| r.get(0))
            .unwrap();
        let original_programme: (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT title,subtitle,description FROM epg_programmes",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();

        // 384 unique programmes with genuinely shortened title metadata and
        // substantial descriptions. This needs more database pages than the
        // bounded per-attempt limits below, without using large host files.
        let mut replacement =
            String::from("<tv><channel id='c'><display-name>Replacement</display-name></channel>");
        for index in 0..384 {
            let start_day = 1 + index / 24;
            let start_hour = index % 24;
            let stop_day = 1 + (index + 1) / 24;
            let stop_hour = (index + 1) % 24;
            let title = format!("{index:04}{}", "T".repeat(513));
            replacement.push_str(&format!(
                "<programme channel='c' start='203001{start_day:02}{start_hour:02}0000 +0000' \
                 stop='203001{stop_day:02}{stop_hour:02}0000 +0000'>\
                 <title>{title}</title><desc>{}</desc></programme>",
                "D".repeat(2300),
            ));
        }
        replacement.push_str("</tv>");
        let base_pages: i64 = conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap();
        let mut exercised = false;
        // Search for a cap that permits an actual SQL INSERT and TEMP marker
        // before SQLite runs out of logical pages. Bounded at 96 small probes.
        for spare_pages in 1..=96 {
            let limit = base_pages + spare_pages;
            let set_limit: i64 = conn
                .query_row(&format!("PRAGMA max_page_count={limit}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(set_limit, limit);
            TEST_STAGED_INSERTS.with(|counter| counter.set(0));
            TEST_STAGED_MARKERS.with(|counter| counter.set(0));
            let failed = import_xmltv(
                &mut conn,
                Cursor::new(replacement.as_bytes()),
                "source",
                2,
                2,
                false,
                &cancelled,
            );
            let staged = TEST_STAGED_INSERTS.with(|counter| counter.get());
            let marked = TEST_STAGED_MARKERS.with(|counter| counter.get());
            assert_eq!(
                failed,
                Err(EpgError::DiskFull),
                "a bounded synthetic guide must hit the real SQLite page limit"
            );
            let persisted: (i64, String, Option<String>, Option<String>) = conn
                .query_row(
                    "SELECT COUNT(*),MIN(title),MIN(subtitle),MIN(description)
                     FROM epg_programmes WHERE source_id='source'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                persisted,
                (
                    1,
                    original_programme.0.clone(),
                    original_programme.1.clone(),
                    original_programme.2.clone()
                )
            );
            let channel: String = conn
                .query_row("SELECT display_name FROM epg_channels", [], |r| r.get(0))
                .unwrap();
            assert_eq!(channel, original_channel);
            let metadata: (Option<i64>, Option<i64>, i64, Option<String>) = conn
                .query_row(
                    "SELECT last_success_at,last_attempt_at,updated_at,last_error_code
                     FROM epg_sources WHERE id='source'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!(metadata, original_metadata);
            let leaked: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_temp_master
                     WHERE name='epg_import_truncated_rowids'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                leaked, 0,
                "failed import must drop transaction-local TEMP marker"
            );
            if staged > 0 && marked > 0 {
                exercised = true;
                break;
            }
        }
        assert!(
            exercised,
            "SQLITE_FULL must occur after a staged INSERT and TEMP marker"
        );
        // Lift the imposed page limit, then retry on this very same connection.
        conn.execute_batch("PRAGMA max_page_count=1073741823;")
            .unwrap();
        let retried = import_xmltv(
            &mut conn,
            Cursor::new(xml("Retried")),
            "source",
            3,
            2,
            false,
            &cancelled,
        )
        .unwrap();
        assert_eq!(retried.programmes, 1);
        let (count, title): (i64, String) = conn
            .query_row("SELECT COUNT(*),MIN(title) FROM epg_programmes", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((count, title.as_str()), (1, "Retried"));
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn headroom_final_check_rolls_back_and_same_connection_retry_succeeds() {
        let mut conn = db();
        let cancelled = AtomicBool::new(false);
        import_xmltv(
            &mut conn,
            Cursor::new(xml("Cached")),
            "source",
            1,
            1,
            false,
            &cancelled,
        )
        .unwrap();
        let original: (Option<i64>, Option<i64>, i64, Option<String>) = conn
            .query_row(
                "SELECT last_success_at,last_attempt_at,updated_at,last_error_code
                 FROM epg_sources WHERE id='source'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        let db_path = std::path::PathBuf::from("synthetic-headroom-db");
        let guard = ImportHeadroom::new(
            db_path.clone(),
            "absent-epg-staging-directory/stage.xml".into(),
            4096,
            false,
        );
        TEST_STAGED_INSERTS.with(|cell| cell.set(0));
        TEST_STAGED_MARKERS.with(|cell| cell.set(0));
        let observation = crate::epg::headroom::override_space(
            db_path,
            vec![Some(crate::epg::headroom::IMPORT_FLOOR - 1)],
        );
        assert_eq!(
            import_xmltv_guarded(
                &mut conn,
                Cursor::new(xml(&"T".repeat(513))),
                "source",
                2,
                2,
                false,
                ImportChecks {
                    cancelled: &cancelled,
                    headroom: Some(&guard),
                },
            ),
            Err(EpgError::DiskFull),
        );
        drop(observation);
        assert_eq!(TEST_STAGED_INSERTS.with(|c| c.get()), 1);
        assert_eq!(TEST_STAGED_MARKERS.with(|c| c.get()), 1);
        let previous_title: String = conn
            .query_row("SELECT title FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(previous_title, "Cached");
        let current: (Option<i64>, Option<i64>, i64, Option<String>) = conn
            .query_row(
                "SELECT last_success_at,last_attempt_at,updated_at,last_error_code
                 FROM epg_sources WHERE id='source'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(current, original);
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master
                 WHERE name='epg_import_truncated_rowids'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);
        let retried = import_xmltv(
            &mut conn,
            Cursor::new(xml("Retried")),
            "source",
            3,
            2,
            false,
            &cancelled,
        )
        .unwrap();
        assert_eq!(retried.programmes, 1);
    }

    #[test]
    fn headroom_4096_programme_check_rejects_without_partial_publication() {
        let mut conn = db();
        let cancelled = AtomicBool::new(false);
        import_xmltv(
            &mut conn,
            Cursor::new(xml("Cached")),
            "source",
            1,
            1,
            false,
            &cancelled,
        )
        .unwrap();
        let mut replacement =
            String::from("<tv><channel id='c'><display-name>Updated</display-name></channel>");
        for n in 0..4096 {
            let month = 1 + n / (28 * 24);
            let day = 1 + (n / 24) % 28;
            let hour = n % 24;
            let stop_day = day + usize::from(hour == 23);
            let stop_hour = (hour + 1) % 24;
            let title = if n == 0 {
                "T".repeat(513)
            } else {
                format!("T{n}")
            };
            replacement.push_str(&format!(
                "<programme channel='c' start='2032{month:02}{day:02}{hour:02}0000 +0000'
                 stop='2032{month:02}{stop_day:02}{stop_hour:02}0000 +0000'>
                 <title>{title}</title></programme>"
            ));
        }
        replacement.push_str("</tv>");
        let db_path = std::path::PathBuf::from("synthetic-headroom-db");
        let guard = ImportHeadroom::new(
            db_path.clone(),
            "absent-epg-staging-directory/stage.xml".into(),
            4096,
            false,
        );
        TEST_STAGED_INSERTS.with(|cell| cell.set(0));
        TEST_STAGED_MARKERS.with(|cell| cell.set(0));
        let observation = crate::epg::headroom::override_space(
            db_path,
            vec![Some(crate::epg::headroom::IMPORT_FLOOR - 1)],
        );
        let result = import_xmltv_guarded(
            &mut conn,
            Cursor::new(replacement),
            "source",
            2,
            2,
            false,
            ImportChecks {
                cancelled: &cancelled,
                headroom: Some(&guard),
            },
        );
        drop(observation);
        assert_eq!(result, Err(EpgError::DiskFull));
        assert_eq!(TEST_STAGED_INSERTS.with(|c| c.get()), 4095);
        assert_eq!(TEST_STAGED_MARKERS.with(|c| c.get()), 1);
        let retained: (i64, String) = conn
            .query_row("SELECT COUNT(*),MIN(title) FROM epg_programmes", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(retained, (1, "Cached".to_owned()));
        let last_success: Option<i64> = conn
            .query_row(
                "SELECT last_success_at FROM epg_sources WHERE id='source'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last_success, Some(1));
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master
                 WHERE name='epg_import_truncated_rowids'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);
    }

    #[test]
    fn headroom_unavailable_probe_does_not_reject_import() {
        let mut conn = db();
        let cancelled = AtomicBool::new(false);
        let db_path = std::path::PathBuf::from("synthetic-headroom-db");
        let guard = ImportHeadroom::new(
            db_path.clone(),
            "absent-epg-staging-directory/stage.xml".into(),
            4096,
            true,
        );
        let observation = crate::epg::headroom::override_space(db_path, vec![None]);
        let result = import_xmltv_guarded(
            &mut conn,
            Cursor::new(xml("Valid")),
            "source",
            1,
            1,
            false,
            ImportChecks {
                cancelled: &cancelled,
                headroom: Some(&guard),
            },
        )
        .unwrap();
        drop(observation);
        assert_eq!(result.programmes, 1);
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
                conflicting_duplicate_rows: 0,
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
                assert_eq!(
                    TEST_STAGED_MARKERS.with(|count| count.get()),
                    1,
                    "the cancellation gate must follow the shortened-row TEMP marker"
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
        TEST_STAGED_MARKERS.with(|count| count.set(0));
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
        assert_eq!(TEST_STAGED_MARKERS.with(|count| count.get()), 1);
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
                 WHERE name='epg_import_truncated_rowids'",
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
                 WHERE name='epg_import_truncated_rowids'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);
    }

    fn import_fixture(
        conn: &mut Connection,
        xml: &str,
        source: &str,
        stamp: i64,
        revision: i64,
    ) -> Result<StreamedRows, EpgError> {
        import_xmltv(
            conn,
            Cursor::new(xml.as_bytes()),
            source,
            stamp,
            revision,
            false,
            &AtomicBool::new(false),
        )
    }

    fn programme(channel: &str, title: &str, subtitle: Option<&str>, desc: Option<&str>) -> String {
        let subtitle = subtitle
            .map(|value| format!("<sub-title>{value}</sub-title>"))
            .unwrap_or_default();
        let desc = desc
            .map(|value| format!("<desc>{value}</desc>"))
            .unwrap_or_default();
        format!(
            "<programme channel='{channel}' start='20300101000000 +0000'              stop='20300101010000 +0000'><title>{title}</title>{subtitle}{desc}</programme>"
        )
    }

    fn guide(channels: &str, programmes: &str) -> String {
        format!("<tv>{channels}{programmes}</tv>")
    }

    fn assert_no_temp_marker(conn: &Connection) {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master                  WHERE name='epg_import_truncated_rowids'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "TEMP state must be absent after import");
    }

    #[test]
    fn migrated_programme_table_is_a_rowid_table_and_temp_markers_do_not_leak() {
        let mut conn = db();
        // WITHOUT ROWID schema migration would invalidate INSERT ... RETURNING rowid.
        let without_rowid: i64 = conn
            .query_row(
                "SELECT wr FROM pragma_table_list                  WHERE schema='main' AND name='epg_programmes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(without_rowid, 0);
        let g = guide(
            "<channel id='c'><display-name>C</display-name></channel>",
            &programme("c", &"T".repeat(513), None, None),
        );
        let stats = import_fixture(&mut conn, &g, "source", 1, 1).unwrap();
        assert_eq!((stats.programmes, stats.truncated_titles), (1, 1));
        let rowid: i64 = conn
            .query_row("SELECT rowid FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert!(rowid > 0);
        assert_no_temp_marker(&conn);
    }

    #[test]
    fn ignored_identical_rows_must_not_inherit_other_rowid_markers() {
        let mut conn = db();
        let c = "<channel id='c'><display-name>C</display-name></channel>";
        let short = programme("c", &"T".repeat(513), None, None);
        let plain = programme("c", "Plain", Some("Same"), Some("Same"));
        let input = guide(c, &format!("{short}{plain}{plain}"));
        let stats = import_fixture(&mut conn, &input, "source", 1, 1).unwrap();
        assert_eq!((stats.programmes, stats.truncated_titles), (2, 1));
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        assert_no_temp_marker(&conn);

        // Two equal BINARY identities with different complete metadata conflict.
        let plain_changed = programme("c", "Plain", Some("Different"), Some("Same"));
        let rejected = guide(c, &format!("{plain}{plain_changed}"));
        assert_eq!(
            import_fixture(&mut conn, &rejected, "source", 2, 2).unwrap_err(),
            EpgError::InvalidSchedule
        );
        assert_no_temp_marker(&conn);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM epg_programmes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "last-good imported rows must survive conflict");
    }

    #[test]
    fn wide_binary_identities_with_common_prefixes_are_not_conflated() {
        let mut conn = db();
        let channel_a = format!("{}a", "C".repeat(511));
        let channel_b = format!("{}b", "C".repeat(511));
        let title_a = format!("{}a", "T".repeat(511));
        let title_b = format!("{}b", "T".repeat(511));
        let channels = format!(
            "<channel id='{channel_a}'><display-name>One</display-name></channel>             <channel id='{channel_b}'><display-name>Two</display-name></channel>"
        );
        let mut programmes = String::new();
        for channel in [&channel_a, &channel_b] {
            for title in [&title_a, &title_b] {
                programmes.push_str(&programme(channel, title, Some(&"S".repeat(513)), None));
            }
        }
        let stats =
            import_fixture(&mut conn, &guide(&channels, &programmes), "source", 1, 1).unwrap();
        assert_eq!((stats.programmes, stats.truncated_subtitles), (4, 4));
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT rowid) FROM epg_programmes",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 4);
        assert_no_temp_marker(&conn);

        // Exact BINARY identity distinguishes case, not just long shared prefixes.
        let input = guide(
            "<channel id='c'><display-name>C</display-name></channel>",
            &format!(
                "{}{}",
                programme("c", "Show", None, None),
                programme("c", "show", None, None)
            ),
        );
        assert_eq!(
            import_fixture(&mut conn, &input, "source", 2, 2)
                .unwrap()
                .programmes,
            2
        );
    }

    #[test]
    fn all_shortening_combinations_and_utf8_byte_boundaries_roundtrip() {
        let channels = "<channel id='c'><display-name>C</display-name></channel>";
        for len in [511, 512, 513] {
            for field in ["title", "sub-title", "desc"] {
                let mut conn = db();
                let value = if field == "desc" {
                    "D".repeat(2048 - 512 + len)
                } else {
                    "T".repeat(len)
                };
                let title = if field == "title" {
                    value.as_str()
                } else {
                    "Normal"
                };
                let sub = (field == "sub-title").then_some(value.as_str());
                let desc = (field == "desc").then_some(value.as_str());
                let stats = import_fixture(
                    &mut conn,
                    &guide(channels, &programme("c", title, sub, desc)),
                    "source",
                    1,
                    1,
                )
                .unwrap();
                assert_eq!(stats.programmes, 1);
                let expected_short = usize::from(len == 513);
                assert_eq!(
                    stats.truncated_titles,
                    if field == "title" { expected_short } else { 0 }
                );
                assert_eq!(
                    stats.truncated_subtitles,
                    if field == "sub-title" {
                        expected_short
                    } else {
                        0
                    }
                );
                assert_eq!(
                    stats.truncated_descriptions,
                    if field == "desc" { expected_short } else { 0 }
                );
                assert_no_temp_marker(&conn);
            }
        }
        let mut conn = db();
        let t = format!("{}é", "X".repeat(511));
        let s = format!("{}é", "Y".repeat(511));
        let d = format!("{}é", "Z".repeat(2047));
        let stats = import_fixture(
            &mut conn,
            &guide(channels, &programme("c", &t, Some(&s), Some(&d))),
            "source",
            1,
            1,
        )
        .unwrap();
        assert_eq!(
            (
                stats.truncated_titles,
                stats.truncated_subtitles,
                stats.truncated_descriptions
            ),
            (1, 1, 1)
        );
        let (stored_title, stored_sub, stored_desc, start, stop): (
            String,
            String,
            String,
            i64,
            i64,
        ) = conn
            .query_row(
                "SELECT title,subtitle,description,start_utc,stop_utc FROM epg_programmes",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(stored_title, "X".repeat(511));
        assert_eq!(stored_sub, "Y".repeat(511));
        assert_eq!(stored_desc, "Z".repeat(2047));
        assert_eq!((start, stop), (1_893_456_000, 1_893_459_600));
        assert_no_temp_marker(&conn);
    }

    #[test]
    fn source_isolation_and_complete_combined_collision_rollback() {
        let mut conn = db();
        conn.execute(
            "INSERT INTO epg_sources(id,name,origin_kind,origin_locator,created_at,updated_at)              VALUES ('second','Other','local_file','/synthetic2',1,1)", [],
        ).unwrap();
        let c = "<channel id='c'><display-name>C</display-name></channel>";
        let combined = programme(
            "c",
            &"T".repeat(513),
            Some(&"S".repeat(513)),
            Some(&"D".repeat(2049)),
        );
        assert_eq!(
            import_fixture(&mut conn, &guide(c, &combined), "source", 1, 1)
                .unwrap()
                .programmes,
            1
        );
        // Another source with the same channel/UTC/title identity is independent.
        let same = programme("c", &"T".repeat(512), None, None);
        assert_eq!(
            import_fixture(&mut conn, &guide(c, &same), "second", 1, 1)
                .unwrap()
                .programmes,
            1
        );
        assert_no_temp_marker(&conn);
        for reverse in [false, true] {
            let (a, b) = if reverse {
                (&same, &combined)
            } else {
                (&combined, &same)
            };
            let failed = import_fixture(&mut conn, &guide(c, &format!("{a}{b}")), "source", 2, 2);
            assert_eq!(failed.unwrap_err(), EpgError::InvalidSchedule);
            assert_no_temp_marker(&conn);
        }
        let per_source: (i64,i64) = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM epg_programmes WHERE source_id='source'),                     (SELECT COUNT(*) FROM epg_programmes WHERE source_id='second')",
            [], |r| Ok((r.get(0)?, r.get(1)?))
        ).unwrap();
        assert_eq!(per_source, (1, 1));
    }
}

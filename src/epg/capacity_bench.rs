// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Opt-in, fabricated XMLTV capacity diagnostics; never downloads provider data.
//! The timings exercise the real parser and SQLite cache implementation.
//! Run only in the dedicated, bounded capacity CI workflow.
use super::{
    cache::replace_guide_snapshot,
    model::{EpgError, GuideChannel, GuideSnapshot, Programme},
    xmltv::{decode_xmltv, ParseLimits},
};
use flate2::{write::GzEncoder, Compression};
use rusqlite::Connection;
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

fn test_directory(scenario: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "broadcaster-epg-capacity-{scenario}-{}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create isolated capacity test directory");
    path
}

fn fixture<W: Write>(out: &mut W, channels: usize, programmes: usize) {
    out.write_all(b"<tv>").unwrap();
    for channel in 0..channels {
        write!(
            out,
            "<channel id='c{channel:05}'><display-name>Channel {channel}</display-name></channel>"
        )
        .unwrap();
    }
    for index in 0..programmes {
        let channel = index % channels;
        let slot = index / channels;
        let day = 24 + slot / 24;
        let hour = slot % 24;
        let end_day = 24 + (slot + 1) / 24;
        let end_hour = (slot + 1) % 24;
        assert!(end_day <= 31, "fixture exceeds October calendar range");
        // Reproducible per-programme variation limits unrealistic gzip ratios.
        let mut state = (index as u64)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(17);
        let mut token = String::with_capacity(32);
        for _ in 0..32 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            token.push(char::from(b"0123456789abcdef"[(state & 15) as usize]));
        }
        write!(
            out,
            "<programme channel='c{channel:05}' start='202610{day:02}{hour:02}0000 +0000' stop='202610{end_day:02}{end_hour:02}0000 +0000'><title>Show {index:07} {token}</title><desc>Sample {token} {token}</desc></programme>"
        )
        .unwrap();
    }
    out.write_all(b"</tv>").unwrap();
}

fn write_gzip_fixture(path: &Path, channels: usize, programmes: usize) {
    let file = File::create(path).expect("create generated XMLTV fixture");
    let mut writer = GzEncoder::new(BufWriter::new(file), Compression::fast());
    fixture(&mut writer, channels, programmes);
    let mut inner = writer.finish().expect("finish generated gzip fixture");
    inner.flush().expect("flush generated gzip fixture");
}

fn file_bytes(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn peak_rss_kib() -> Option<u64> {
    let content = fs::read_to_string("/proc/self/status").ok()?;
    content.lines().find_map(|line| {
        line.strip_prefix("VmHWM:")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

fn disk_db(dir: &Path) -> Connection {
    let conn = Connection::open(dir.join("cache.sqlite3")).unwrap();
    conn.execute_batch(crate::streams::storage::MIGRATION_001)
        .unwrap();
    conn.execute_batch(crate::streams::storage::MIGRATION_002)
        .unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    conn.execute(
        "INSERT INTO epg_sources (id,name,origin_kind,origin_locator,created_at,updated_at) VALUES ('fixture','Synthetic guide','local_file','/fixture',1,1)",
        [],
    )
    .unwrap();
    // SQLite connection remains private to this deterministic temporary directory.
    conn
}

fn report(scenario: &str, stage: &str, elapsed_ms: u128, dir: &Path) {
    let db = dir.join("cache.sqlite3");
    let wal = dir.join("cache.sqlite3-wal");
    let shm = dir.join("cache.sqlite3-shm");
    println!(
        "EPG_CAPACITY scenario={scenario} stage={stage} elapsed_ms={elapsed_ms} peak_rss_kib={:?} db_bytes={} wal_bytes={} shm_bytes={}",
        peak_rss_kib(),
        file_bytes(&db),
        file_bytes(&wal),
        file_bytes(&shm)
    );
}

#[test]
#[ignore = "opt-in capacity benchmark; CI job only"]
fn epg_capacity_de_synthetic() {
    const CHANNELS: usize = 2_000;
    const PROGRAMMES: usize = 100_000;
    let dir = test_directory("de");
    let path = dir.join("fixture.xml.gz");
    let start = Instant::now();
    write_gzip_fixture(&path, CHANNELS, PROGRAMMES);
    let wire_bytes = file_bytes(&path);
    println!(
        "EPG_CAPACITY scenario=de fixture_channels={CHANNELS} fixture_programmes={PROGRAMMES} gzip_bytes={wire_bytes} generator_ms={}",
        start.elapsed().as_millis()
    );
    let cancelled = AtomicBool::new(false);
    let now = Instant::now();
    let snapshot = decode_xmltv(
        BufReader::new(File::open(&path).unwrap()),
        "fixture",
        0,
        true,
        ParseLimits::default(),
        &cancelled,
    )
    .expect("DE-sized compressed fixture should be supported by the baseline parser");
    assert_eq!(snapshot.channels.len(), CHANNELS);
    assert_eq!(snapshot.programmes.len(), PROGRAMMES);
    report("de", "parsed", now.elapsed().as_millis(), &dir);

    let mut conn = disk_db(&dir);
    let now = Instant::now();
    replace_guide_snapshot(&mut conn, &snapshot, &cancelled).unwrap();
    let actual: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM epg_programmes WHERE source_id='fixture'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actual, PROGRAMMES as i64);
    report("de", "sqlite_committed", now.elapsed().as_millis(), &dir);
    drop(conn);
    drop(snapshot);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "opt-in capacity benchmark; CI job only"]
fn epg_capacity_us_baseline_rejection() {
    const CHANNELS: usize = 12_511;
    const PROGRAMMES: usize = 1_158_755;
    let dir = test_directory("us");
    let path = dir.join("fixture.xml.gz");
    let start = Instant::now();
    write_gzip_fixture(&path, CHANNELS, PROGRAMMES);
    println!(
        "EPG_CAPACITY scenario=us fixture_channels={CHANNELS} fixture_programmes={PROGRAMMES} gzip_bytes={} generator_ms={}",
        file_bytes(&path),
        start.elapsed().as_millis()
    );

    let mut conn = disk_db(&dir);
    let cancelled = AtomicBool::new(false);
    let old = GuideSnapshot {
        source_id: "fixture".into(),
        channels: vec![GuideChannel {
            source_id: "fixture".into(),
            channel_id: "old".into(),
            display_name: "Old channel".into(),
        }],
        programmes: vec![Programme {
            source_id: "fixture".into(),
            channel_id: "old".into(),
            start_utc: 0,
            stop_utc: 3600,
            title: "Previous guide".into(),
            subtitle: None,
            description: None,
        }],
        retrieved_utc: 0,
    };
    replace_guide_snapshot(&mut conn, &old, &cancelled).unwrap();
    let now = Instant::now();
    let outcome = decode_xmltv(
        BufReader::new(File::open(&path).unwrap()),
        "fixture",
        1,
        true,
        ParseLimits::default(),
        &cancelled,
    );
    assert_eq!(outcome.unwrap_err(), EpgError::OverLimit);
    report("us", "baseline_rejected", now.elapsed().as_millis(), &dir);
    let (count, title): (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), MIN(title) FROM epg_programmes WHERE source_id='fixture'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((count, title.as_str()), (1, "Previous guide"));
    drop(conn);
    fs::remove_dir_all(dir).unwrap();
}

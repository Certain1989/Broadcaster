// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::{cache::*, model::*};
use rusqlite::{params, Connection};
use std::sync::atomic::{AtomicBool, Ordering};

fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(crate::streams::storage::MIGRATION_001)
        .unwrap();
    conn.execute_batch(crate::streams::storage::MIGRATION_002)
        .unwrap();
    conn
}
fn add_source(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO epg_sources (id,name,origin_kind,origin_locator,created_at,updated_at)
         VALUES (?1,?1,'local_file','/fake/guide.xml',1,1)",
        [id],
    )
    .unwrap();
}
fn programme(source: &str, channel: &str, a: i64, b: i64, title: &str) -> Programme {
    Programme {
        source_id: source.into(),
        channel_id: channel.into(),
        start_utc: a,
        stop_utc: b,
        title: title.into(),
        subtitle: None,
        description: None,
    }
}
fn snapshot(source: &str) -> GuideSnapshot {
    GuideSnapshot {
        source_id: source.into(),
        retrieved_utc: 1_800_000_000,
        channels: vec![GuideChannel {
            source_id: source.into(),
            channel_id: "epg.example".into(),
            display_name: "Fictional Channel".into(),
        }],
        programmes: vec![
            programme(source, "epg.example", 100, 300, "First"),
            programme(source, "epg.example", 300, 600, "Second"),
            programme(source, "epg.example", 86_300, 87_000, "After midnight"),
        ],
    }
}

#[test]
fn live_and_iptv_identities_are_distinct() {
    assert_ne!(
        LinearChannelRef::Live {
            provider_channel_id: "same".into()
        },
        LinearChannelRef::Iptv {
            iptv_channel_id: "same".into()
        },
    );
}

#[test]
fn half_open_now_next_and_conflicts_are_explicit() {
    let mut s = snapshot("one");
    assert_eq!(
        select_programmes(&s.programmes, "epg.example", 100).certainty,
        Certainty::Exact
    );
    assert_eq!(
        select_programmes(&s.programmes, "epg.example", 299).certainty,
        Certainty::Exact
    );
    let at_stop = select_programmes(&s.programmes, "epg.example", 300);
    assert_eq!(at_stop.current.unwrap().title, "Second");
    assert_eq!(
        select_programmes(&s.programmes, "epg.example", 601).certainty,
        Certainty::Missing
    );
    s.programmes.push(programme(
        "one",
        "epg.example",
        200,
        400,
        "Overlapping event",
    ));
    let ambiguous = select_programmes(&s.programmes, "epg.example", 220);
    assert_eq!(ambiguous.certainty, Certainty::Ambiguous);
    assert!(ambiguous.current.is_none());
    assert!(ambiguous.progress_fraction.is_none());
    s.programmes.push(programme(
        "one",
        "epg.example",
        86_300,
        86_990,
        "Same-start next",
    ));
    let next = select_programmes(&s.programmes, "epg.example", 700);
    assert!(next.next_ambiguous);
    assert!(next.next.is_none());
}

#[test]
fn full_schedule_is_replaced_atomically_and_failed_update_keeps_last_good() {
    let mut db = db();
    add_source(&db, "guide-1");
    add_source(&db, "guide-2");
    let cancel = AtomicBool::new(false);
    replace_guide_snapshot(&mut db, &snapshot("guide-1"), &cancel).unwrap();
    replace_guide_snapshot(&mut db, &snapshot("guide-2"), &cancel).unwrap();
    let prev: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM epg_programmes WHERE source_id='guide-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(prev, 3);
    let mut invalid = snapshot("guide-1");
    invalid.programmes[0].stop_utc = invalid.programmes[0].start_utc;
    assert_eq!(
        replace_guide_snapshot(&mut db, &invalid, &cancel),
        Err(EpgError::InvalidSchedule)
    );
    let retained: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM epg_programmes WHERE source_id='guide-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retained, prev);
    cancel.store(true, Ordering::SeqCst);
    assert_eq!(
        replace_guide_snapshot(&mut db, &snapshot("guide-1"), &cancel),
        Err(EpgError::Cancelled)
    );
    let unaffected: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM epg_programmes WHERE source_id='guide-2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unaffected, 3);
}

#[test]
fn cache_window_is_offline_and_includes_left_edge_cross_midnight() {
    let mut db = db();
    add_source(&db, "guide");
    let cancel = AtomicBool::new(false);
    replace_guide_snapshot(&mut db, &snapshot("guide"), &cancel).unwrap();
    let page = read_guide_window(
        &db,
        "guide",
        &["epg.example".into()],
        86_400,
        87_100,
        1_800_000_001,
        &cancel,
    )
    .unwrap();
    assert_eq!(page.programmes.len(), 1);
    assert_eq!(page.programmes[0].title, "After midnight");
    assert_eq!(
        page.coverage,
        Coverage::Partial {
            observed_start_utc: Some(100),
            observed_stop_utc: Some(87_000)
        }
    );
    assert!(!page.truncated);
    // End exclusive: the event with stop=300 does not overlap [300,600).
    let page = read_guide_window(
        &db,
        "guide",
        &["epg.example".into()],
        300,
        600,
        1_800_000_001,
        &cancel,
    )
    .unwrap();
    assert_eq!(page.programmes.len(), 1);
    assert_eq!(page.programmes[0].title, "Second");
    assert!(read_guide_window(&db, "guide", &["epg.example".into()], 10, 10, 0, &cancel).is_err());
    assert!(read_guide_window(
        &db,
        "guide",
        &["epg.example".into()],
        0,
        172_801,
        0,
        &cancel
    )
    .is_err());
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(
        read_guide_window(&db, "guide", &["epg.example".into()], 0, 100, 0, &cancel),
        Err(EpgError::Cancelled),
    ));
}

#[test]
fn exactly_4096_rows_and_one_sentinel_is_bounded() {
    let db = db();
    add_source(&db, "guide-4097");
    for i in 0..64 {
        db.execute(
            "INSERT INTO epg_channels (source_id,channel_id,display_name)
             VALUES ('guide-4097',?1,?1)",
            [format!("channel-{i:02}")],
        )
        .unwrap();
    }
    for i in 0..4097 {
        db.execute(
            "INSERT INTO epg_programmes
             (source_id,channel_id,start_utc,stop_utc,title)
             VALUES ('guide-4097','channel-00',?1,?2,?3)",
            params![i * 10, i * 10 + 5, format!("Event {i:04}")],
        )
        .unwrap();
    }
    let ids = (0..64)
        .map(|i| format!("channel-{i:02}"))
        .collect::<Vec<_>>();
    let cancel = AtomicBool::new(false);
    let page = read_guide_window(&db, "guide-4097", &ids, 0, 48_000, 48_000, &cancel).unwrap();
    assert_eq!(page.selected_guide_channels.len(), 64);
    assert_eq!(page.programmes.len(), 4096);
    assert!(page.truncated);
    assert_eq!(page.programmes[0].start_utc, 0);
    assert_eq!(page.programmes[4095].start_utc, 40_950);
    let limited = read_guide_window(&db, "guide-4097", &ids[0..1], 0, 50, 100, &cancel).unwrap();
    assert_eq!(limited.programmes.len(), 5);
    assert!(!limited.truncated);
}

#[test]
fn disabled_guide_does_not_serve_cached_programmes() {
    let mut db = db();
    add_source(&db, "disabled");
    let cancel = AtomicBool::new(false);
    replace_guide_snapshot(&mut db, &snapshot("disabled"), &cancel).unwrap();
    db.execute("UPDATE epg_sources SET enabled=0 WHERE id='disabled'", [])
        .unwrap();
    let page = read_guide_window(
        &db,
        "disabled",
        &["epg.example".into()],
        1,
        600,
        1_800_000_000,
        &cancel,
    )
    .unwrap();
    assert_eq!(page.coverage, Coverage::Disabled);
    assert!(page.programmes.is_empty());
}

#[test]
fn xmltv_offsets_berlin_dst_entities_and_full_cache() {
    use super::xmltv::{decode_xmltv, parse_timestamp, ParseLimits};
    use std::io::Cursor;
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
    <tv>
      <channel id="fictional.de"><display-name>Fictional &amp; Co</display-name></channel>
      <programme channel="fictional.de" start="20261025013000 +0200" stop="20261025023000 +0100">
        <title>Night &amp; Show</title><desc>Safe literal &lt;script&gt; only</desc>
      </programme>
      <programme channel="fictional.de" start="20261025013000 +0200" stop="20261025023000 +0100">
        <title>Night &amp; Show</title><desc>Safe literal &lt;script&gt; only</desc>
      </programme>
      <programme channel="fictional.de" start="20261025023000 +0100" stop="20261025033000 +0100">
        <title>Morning Show</title>
      </programme>
      <programme channel="fictional.de" start="20261025033000 +0100"><title>Invalid stop, skip</title></programme>
    </tv>"#;
    let now=parse_timestamp("20261025020000 +0200").unwrap();
    let cancel=AtomicBool::new(false);
    let snap=decode_xmltv(
        Cursor::new(xml), "fictional-guide", now, false, ParseLimits::default(), &cancel,
    ).unwrap();
    assert_eq!(snap.channels.len(),1);
    assert_eq!(snap.channels[0].display_name,"Fictional & Co");
    assert_eq!(snap.programmes.len(),2);
    assert_eq!(snap.programmes[0].title,"Night & Show");
    assert_eq!(snap.programmes[0].description.as_deref(),Some("Safe literal <script> only"));
    assert_eq!(snap.programmes[0].stop_utc-snap.programmes[0].start_utc,7_200);
    let mut db=db();
    add_source(&db,"fictional-guide");
    replace_guide_snapshot(&mut db,&snap,&cancel).unwrap();
    let crossing=read_guide_window(
        &db,"fictional-guide",&["fictional.de".into()],
        snap.programmes[0].start_utc+60,snap.programmes[0].stop_utc+60,now,&cancel,
    ).unwrap();
    assert_eq!(crossing.programmes.len(),2);
    assert_eq!(crossing.coverage,Coverage::Partial {
        observed_start_utc:Some(snap.programmes[0].start_utc),
        observed_stop_utc:Some(snap.programmes[1].stop_utc),
    });
}

#[test]
fn malformed_xmltv_and_external_entities_fail_closed() {
    use super::xmltv::{decode_xmltv, ParseLimits};
    use std::io::Cursor;
    let cancel=AtomicBool::new(false);
    let valid=r#"<tv><channel id="fixture"><display-name>Fixture</display-name></channel>
    <programme channel="fixture" start="20261023100000 +0000" stop="20261023110000 +0000">
    <title>Fixture title</title></programme></tv>"#;
    for xml in [
        valid.replace("<tv>","<!DOCTYPE tv [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><tv>"),
        valid.replace("<tv>","<?xml-stylesheet href='https://invalid.example/a.xsl'?><tv>"),
        valid.replace("<tv>","<tv xmlns:xi='http://www.w3.org/2001/XInclude'><xi:include href='file:///etc/passwd'/>"),
        valid.replace("<title>Fixture title</title>","<title>&malicious;</title>"),
        valid.replace("start=\"20261023100000 +0000\"","start=\"20261023100000\""),
        valid.replace("<title>Fixture title</title>","<title>Broken <inner></title>"),
    ] {
        assert!(
            decode_xmltv(Cursor::new(xml), "fictional-guide", 0, false, ParseLimits::default(), &cancel).is_err()
        );
    }
    assert_eq!(
        decode_xmltv(Cursor::new(valid.as_bytes()), "fictional-guide", 0, false, ParseLimits::default(), &cancel)
            .unwrap().programmes.len(),1,
    );
}

#[test]
fn gzip_xmltv_valid_truncated_bomb_and_limits() {
    use super::xmltv::{decode_xmltv, ParseLimits};
    use std::io::{Cursor, Write};
    use flate2::{write::GzEncoder, Compression};
    let cancel=AtomicBool::new(false);
    let xml=b"<tv><channel id='channel'><display-name>Fiction</display-name></channel>\
    <programme channel='channel' start='20261023100000 +0000' stop='20261023110000 +0000'>\
    <title>Show</title></programme></tv>";
    let mut encoder=GzEncoder::new(Vec::new(),Compression::default());
    encoder.write_all(xml).unwrap();
    let compressed=encoder.finish().unwrap();
    let snap=decode_xmltv(Cursor::new(&compressed),"epg",0,true,ParseLimits::default(),&cancel).unwrap();
    assert_eq!(snap.programmes.len(),1);
    assert!(
        decode_xmltv(Cursor::new(&compressed[..compressed.len()-4]),"epg",0,true,ParseLimits::default(),&cancel).is_err()
    );
    let mut encoder=GzEncoder::new(Vec::new(),Compression::best());
    encoder.write_all(&vec![b'X';300_000]).unwrap();
    let bomb=encoder.finish().unwrap();
    assert_eq!(
        decode_xmltv(Cursor::new(bomb),"epg",0,true,ParseLimits::default(),&cancel)
            .unwrap_err(), EpgError::OverLimit,
    );
    let limits=ParseLimits { max_wire_bytes: 8, ..ParseLimits::default() };
    assert_eq!(
        decode_xmltv(Cursor::new(xml),"epg",0,false,limits,&cancel).unwrap_err(),EpgError::OverLimit,
    );
}

#[test]
fn xmltv_calendar_timezone_and_cancel_reject_unqualified_and_impossible() {
    use super::xmltv::{decode_xmltv, parse_timestamp, ParseLimits};
    use std::io::Cursor;
    for invalid in [
        "20261025020000","20261025020000 +1500","20261025020000 +1460",
        "20260230030000 +0100","20261025020000 +1401",
        "20261025020000 +1", "19000101000000 Z",
    ] {
        assert!(parse_timestamp(invalid).is_err(), "{invalid}");
    }
    let base=parse_timestamp("20261025013000 +0200").unwrap();
    let repeated=parse_timestamp("20261025013000 +0100").unwrap();
    assert_eq!(repeated-base,3600);
    let cancel=AtomicBool::new(true);
    let xml="<tv><channel id='fixture'/></tv>";
    assert_eq!(
        decode_xmltv(Cursor::new(xml),"guide",0,false,ParseLimits::default(),&cancel)
            .unwrap_err(), EpgError::Cancelled,
    );
}

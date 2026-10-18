// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Synthetic SQLite fixtures only; never include private playlists or tokenized URLs.
use super::{model::*, storage::*};
use std::{collections::BTreeSet, fs, path::PathBuf, time::Instant};

const NOW: i64 = 1_800_000_000;
struct Fixture {
    store: Storage,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("broadcaster-iptv-{}", uuid::Uuid::new_v4()));
        let path = dir.join("broadcaster-iptv.sqlite3");
        let store = Storage::at(&path);
        store.initialize().expect("init");
        Self { store, path }
    }
    fn conn(&self) -> rusqlite::Connection {
        self.store.connect().expect("conn")
    }
    fn new_source(&self, name: &str, n: usize) -> (Source, ValidatedSnapshot) {
        let id = uuid::Uuid::new_v4().to_string();
        let snapshot = ValidatedSnapshot::new(id.clone(), channels(n)).unwrap();
        let source = self
            .store
            .create_source_with_initial_snapshot(&id, &draft(name), &snapshot, NOW)
            .unwrap();
        (source, snapshot)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}
fn draft(name: &str) -> SourceDraft {
    SourceDraft {
        name: name.to_owned(),
        origin: SourceOrigin {
            kind: OriginKind::RemoteUrl,
            locator: "https://example.invalid/playlist.m3u".into(),
        },
        enabled: true,
        order: 0,
        defaults: SourceDefaults::default(),
    }
}
fn channel(i: usize) -> IncomingChannel {
    IncomingChannel {
        order: i as u32,
        imported_name: format!("Channel {i}"),
        stream_url: format!("https://example.invalid/live/{i}?fake=1"),
        tvg_id: Some(format!("channel-{i}")),
        tvg_name: None,
        group_title: Some(
            if i.is_multiple_of(2) {
                "News"
            } else {
                "Sports"
            }
            .into(),
        ),
        logo_url: None,
        channel_number: None,
        region: None,
        languages: BTreeSet::new(),
        countries: BTreeSet::new(),
        user_agent: None,
        referer: None,
    }
}
fn channels(n: usize) -> Vec<IncomingChannel> {
    (0..n).map(channel).collect()
}
fn snapshot(id: &str, n: usize) -> ValidatedSnapshot {
    ValidatedSnapshot::new(id.to_owned(), channels(n)).unwrap()
}
fn query_all() -> ChannelQuery {
    ChannelQuery {
        limit: 500,
        ..ChannelQuery::default()
    }
}
#[test]
fn exact_migration_v1_with_fk_and_d6_constraints() {
    let f = Fixture::new();
    let conn = f.conn();
    assert_eq!(
        conn.query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        APPLICATION_ID
    );
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    assert_eq!(
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let mut stmt=conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'iptv_%' ORDER BY name").unwrap();
    let tables = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert_eq!(
        tables,
        vec![
            "iptv_channel_imported_facets",
            "iptv_channel_override_facets",
            "iptv_channel_overrides",
            "iptv_channels",
            "iptv_source_default_facets",
            "iptv_sources"
        ]
    );
    let index_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_iptv_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(index_count, 8);
    let (source, _) = f.new_source("Region test", 1);
    assert!(conn
        .execute(
            "UPDATE iptv_sources SET default_region_code='  ' WHERE id=?1",
            [&source.id]
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE iptv_sources SET default_region_label=?1 WHERE id=?2",
            rusqlite::params!["x".repeat(129), source.id]
        )
        .is_err());
    assert!(conn.execute(
        "INSERT INTO iptv_source_default_facets(source_id,facet,value) VALUES(?1,'language','de')",
        ["missing-source-id"]).is_err());
}
#[test]
fn reject_nonempty_alien_future_and_corrupt_database() {
    for (id, version) in [
        (0, 0),
        (APPLICATION_ID, 2),
        (APPLICATION_ID, 0),
        (APPLICATION_ID, 1),
    ] {
        let dir = std::env::temp_dir().join(format!("broadcaster-alien-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = dir.join("db.sqlite");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE alien(x);PRAGMA application_id={id};PRAGMA user_version={version};"
        ))
        .unwrap();
        drop(conn);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(matches!(
            Storage::at(&path).initialize(),
            Err(StorageError::IncompatibleDatabase)
        ));
        assert!(path.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
#[cfg(unix)]
#[test]
fn reject_world_readable_storage() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    fs::set_permissions(&f.path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        f.store.initialize(),
        Err(StorageError::PermissionDenied)
    ));
    assert!(matches!(
        f.store.list_sources(),
        Err(StorageError::PermissionDenied)
    ));
}
#[test]
fn atomic_nonempty_initial_creation_and_reopen() {
    let f = Fixture::new();
    let id = uuid::Uuid::new_v4().to_string();
    assert!(ValidatedSnapshot::new(id.clone(), vec![]).is_err());
    assert_eq!(f.store.list_sources().unwrap().len(), 0);
    let bad = SourceDraft {
        name: "   ".into(),
        ..draft("X")
    };
    assert!(f
        .store
        .create_source_with_initial_snapshot(&id, &bad, &snapshot(&id, 1), NOW)
        .is_err());
    assert_eq!(f.store.list_sources().unwrap().len(), 0);
    let good = f
        .store
        .create_source_with_initial_snapshot(&id, &draft("X"), &snapshot(&id, 2), NOW)
        .unwrap();
    assert_eq!(good.revision, 1);
    assert_eq!(f.store.query_channels(&query_all()).unwrap().total, 2);
    let reopen = Storage::at(&f.path);
    reopen.initialize().unwrap();
    assert_eq!(reopen.get_source(&id).unwrap().name, "X");
    assert_eq!(reopen.query_channels(&query_all()).unwrap().total, 2);
}
#[test]
fn source_crud_revision_and_cascade() {
    let f = Fixture::new();
    let (source, _) = f.new_source("Public example", 3);
    let old_revision = source.revision;
    f.store
        .rename_source(&source.id, "New Name", NOW + 1)
        .unwrap();
    f.store
        .set_source_enabled(&source.id, false, NOW + 2)
        .unwrap();
    assert_eq!(f.store.query_channels(&query_all()).unwrap().total, 0);
    f.store
        .set_source_enabled(&source.id, true, NOW + 3)
        .unwrap();
    f.store.reorder_source(&source.id, 9, NOW + 4).unwrap();
    let updated = f.store.get_source(&source.id).unwrap();
    assert_eq!(updated.revision, old_revision + 4);
    assert_eq!(updated.order, 9);
    let confirmed = ConfirmedSourceDeletion::after_user_confirmation(&updated);
    f.store.delete_source_confirmed(&confirmed).unwrap();
    assert!(matches!(
        f.store.get_source(&source.id),
        Err(StorageError::NotFound)
    ));
    let count: i64 = f
        .conn()
        .query_row("SELECT count(*) FROM iptv_channels", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn source_count_and_atomic_dense_source_move() {
    let f = Fixture::new();
    let (alpha, _) = f.new_source("Alpha", 3);
    let (bravo, _) = f.new_source("Bravo", 2);
    let (charlie, _) = f.new_source("Charlie", 1);
    // All three fixture sources have source_order=0, so the move must
    // normalize a tie without moving unrelated channels across sources.
    assert_eq!(f.store.source_counts(&alpha.id).unwrap(), (3, 3));
    assert_eq!(f.store.source_counts(&bravo.id).unwrap(), (2, 2));
    assert_eq!(f.store.source_counts(&charlie.id).unwrap(), (1, 1));
    let initial: Vec<_> = f
        .store
        .list_sources()
        .unwrap()
        .into_iter()
        .map(|source| source.id)
        .collect();
    assert_eq!(initial.len(), 3);
    let moved = initial[0].clone();
    f.store.move_source(&moved, 1, NOW + 1).unwrap();
    let reordered = f.store.list_sources().unwrap();
    assert_eq!(reordered[0].id, initial[1]);
    assert_eq!(reordered[1].id, initial[0]);
    assert_eq!(reordered[2].id, initial[2]);
    assert_eq!(
        reordered
            .iter()
            .map(|source| source.order)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(f.store.source_counts(&alpha.id).unwrap(), (3, 3));
    assert_eq!(f.store.query_channels(&query_all()).unwrap().total, 6);
    assert!(matches!(
        f.store.move_source(&moved, 0, NOW + 2),
        Err(StorageError::InvalidInput)
    ));
    assert!(matches!(
        f.store.move_source("missing", -1, NOW + 2),
        Err(StorageError::NotFound)
    ));
}

#[test]
fn source_defaults_multi_facet_manual_unknown_and_editor_provenance() {
    let f = Fixture::new();
    let (source, _) = f.new_source("News", 1);
    let defaults = SourceDefaults {
        languages: ["de".to_owned(), "en".to_owned()].into_iter().collect(),
        countries: ["DE".to_owned(), "AT".to_owned()].into_iter().collect(),
        region: Some(Region {
            code: Some("Bavaria".into()),
            label: None,
        }),
    };
    f.store
        .set_source_defaults(&source.id, &defaults, NOW + 1)
        .unwrap();
    let id = f.store.query_channels(&query_all()).unwrap().items[0]
        .id
        .clone();
    let detail = f.store.get_channel_detail(&id).unwrap();
    assert_eq!(detail.provenance.languages, FieldOrigin::SourceFallback);
    assert_eq!(detail.effective.languages.len(), 2);
    assert_eq!(detail.effective.countries.len(), 2);
    assert_eq!(detail.effective.region, defaults.region);
    let overrides = ChannelOverrides {
        languages: SetOverride {
            mode: FacetMode::Replace,
            values: ["fr".to_owned(), "de".to_owned()].into_iter().collect(),
        },
        countries: SetOverride {
            mode: FacetMode::Unknown,
            values: BTreeSet::new(),
        },
        group: GroupOverride {
            mode: FacetMode::Unknown,
            value: None,
        },
        display_name: Some("User Channel".into()),
        ..ChannelOverrides::default()
    };
    f.store
        .set_channel_overrides(&id, &overrides, NOW + 2)
        .unwrap();
    let detail = f.store.get_channel_detail(&id).unwrap();
    assert_eq!(detail.effective.visible_name, "User Channel");
    assert_eq!(detail.provenance.languages, FieldOrigin::Manual);
    assert_eq!(detail.provenance.countries, FieldOrigin::Unknown);
    assert!(detail.effective.countries.is_empty());
    assert!(detail.effective.group.is_none());
    let opts = f
        .store
        .query_filter_options(&query_all(), FilterDimension::Language)
        .unwrap();
    assert_eq!(opts.choices.len(), 2);
    assert!(opts.choices.iter().all(|opt| opt.count == 1));
    let opts = f
        .store
        .query_filter_options(&query_all(), FilterDimension::Country)
        .unwrap();
    assert_eq!(
        opts.choices,
        vec![FacetChoice {
            value: FacetValue::Unknown,
            count: 1
        }]
    );
    let mut q = query_all();
    q.group = FacetSelection::Unknown;
    q.language = FacetSelection::Value("fr".into());
    q.country = FacetSelection::Unknown;
    assert_eq!(f.store.query_channels(&q).unwrap().total, 1);
    f.store.clear_channel_overrides(&id).unwrap();
    let restored = f.store.get_channel_detail(&id).unwrap();
    assert_eq!(restored.provenance.languages, FieldOrigin::SourceFallback);
    assert!(!restored.effective.has_user_overrides);
}
#[test]
fn suspicious_reduction_revision_and_locator_atomicity() {
    let f = Fixture::new();
    let (source, _) = f.new_source("Twenty", 20);
    let small = snapshot(&source.id, 4);
    let preview = f.store.preview_refresh(&small).unwrap();
    assert_eq!(preview.old_present, 20);
    assert_eq!(preview.new_present, 4);
    assert!(preview.requires_confirmation);
    assert_eq!(
        f.store
            .commit_refresh(&small, &preview, &RefreshDecision::NotRequired, NOW + 1)
            .unwrap_err(),
        StorageError::ApprovalRequired
    );
    let origin = SourceOrigin {
        kind: OriginKind::RemoteUrl,
        locator: "https://example.invalid/replacement.m3u".into(),
    };
    assert_eq!(
        f.store
            .replace_source_origin_and_snapshot(
                &small,
                &preview,
                &RefreshDecision::NotRequired,
                origin.clone(),
                NOW + 1
            )
            .unwrap_err(),
        StorageError::ApprovalRequired
    );
    assert_eq!(
        f.store.get_source(&source.id).unwrap().origin.locator,
        source.origin.locator
    );
    assert_eq!(f.store.query_channels(&query_all()).unwrap().total, 20);
    let approve = preview.explicit_approval();
    f.store
        .replace_source_origin_and_snapshot(&small, &preview, &approve, origin.clone(), NOW + 2)
        .unwrap();
    assert_eq!(
        f.store.get_source(&source.id).unwrap().origin.locator,
        origin.locator
    );
    assert_eq!(f.store.query_channels(&query_all()).unwrap().total, 4);
    assert_eq!(
        f.store
            .commit_refresh(&small, &preview, &approve, NOW + 3)
            .unwrap_err(),
        StorageError::StaleRevision
    );
}
#[test]
fn exact_seventy_five_percent_decline_not_required() {
    let f = Fixture::new();
    let (s, _) = f.new_source("Threshold", 20);
    let snap = snapshot(&s.id, 5);
    let preview = f.store.preview_refresh(&snap).unwrap();
    assert!(!preview.requires_confirmation);
    f.store
        .commit_refresh(&snap, &preview, &RefreshDecision::NotRequired, NOW + 1)
        .unwrap();
    assert_eq!(f.store.query_channels(&query_all()).unwrap().total, 5);
}
#[test]
fn stable_identity_token_change_tombstone_return_and_purge() {
    let f = Fixture::new();
    let (s, _) = f.new_source("Stable", 2);
    let old = f.store.query_channels(&query_all()).unwrap().items;
    let stable = old[0].id.clone();
    let mut entries = channels(2);
    entries.reverse();
    entries[1].stream_url = "https://example.invalid/changed?token=fake".into();
    entries[0].order = 0;
    entries[1].order = 1;
    let new = ValidatedSnapshot::new(s.id.clone(), entries).unwrap();
    let preview = f.store.preview_refresh(&new).unwrap();
    f.store
        .commit_refresh(&new, &preview, &RefreshDecision::NotRequired, NOW + 1)
        .unwrap();
    let detail = f.store.get_channel_detail(&stable).unwrap();
    assert!(detail.imported.stream_url.contains("changed"));
    let absent = ValidatedSnapshot::new(s.id.clone(), vec![channel(1)]).unwrap();
    let preview = f.store.preview_refresh(&absent).unwrap();
    f.store
        .commit_refresh(&absent, &preview, &RefreshDecision::NotRequired, NOW + 2)
        .unwrap();
    assert_eq!(f.store.query_channels(&query_all()).unwrap().total, 1);
    assert_eq!(
        f.store
            .purge_expired_missing(NOW + 2 + 89 * 24 * 3600)
            .unwrap(),
        0
    );
    let revived = ValidatedSnapshot::new(s.id.clone(), vec![channel(0), channel(1)]).unwrap();
    let preview = f.store.preview_refresh(&revived).unwrap();
    f.store
        .commit_refresh(&revived, &preview, &RefreshDecision::NotRequired, NOW + 4)
        .unwrap();
    assert_eq!(
        f.store.get_channel_detail(&stable).unwrap().effective.id,
        stable
    );
}
#[test]
fn rejected_bad_values_and_sanitized_errors() {
    assert_eq!(
        normalize_facet("pt-br", FacetDimension::Language).unwrap(),
        "pt-BR"
    );
    assert_eq!(
        normalize_facet("de", FacetDimension::Country).unwrap(),
        "DE"
    );
    assert!(normalize_facet("Deutschland", FacetDimension::Country).is_err());
    assert!(normalize_facet("ZZ", FacetDimension::Country).is_err());
    assert!(normalize_facet("a-bad_bad", FacetDimension::Language).is_err());
    let mut bad = channel(0);
    bad.stream_url = "file:///etc/passwd".into();
    assert!(bad.validate().is_err());
    let mut edits = ChannelOverrides::default();
    edits.languages.mode = FacetMode::Replace;
    assert!(edits.validate().is_err());
    assert!(!format!("{}", StorageError::InvalidInput).contains("example.invalid"));
}

#[test]
fn sql_failure_rolls_back_catalogue_and_new_locator() {
    let f = Fixture::new();
    let (source, _) = f.new_source("Rollback", 3);
    let before = f.store.query_channels(&query_all()).unwrap();
    let revised = snapshot(&source.id, 2);
    let preview = f.store.preview_refresh(&revised).unwrap();
    f.conn()
        .execute_batch(
            "CREATE TRIGGER deny_catalogue_update
             BEFORE UPDATE ON iptv_channels
             BEGIN SELECT RAISE(ABORT, 'synthetic rollback test'); END;",
        )
        .unwrap();
    let changed_origin = SourceOrigin {
        kind: OriginKind::RemoteUrl,
        locator: "https://example.invalid/replacement.m3u".into(),
    };
    assert_eq!(
        f.store
            .replace_source_origin_and_snapshot(
                &revised,
                &preview,
                &RefreshDecision::NotRequired,
                changed_origin,
                NOW + 1,
            )
            .unwrap_err(),
        StorageError::StorageFailure
    );
    assert_eq!(
        f.store.get_source(&source.id).unwrap().revision,
        source.revision
    );
    assert_eq!(
        f.store.get_source(&source.id).unwrap().origin.locator,
        source.origin.locator
    );
    let after = f.store.query_channels(&query_all()).unwrap();
    assert_eq!(before.total, after.total);
    assert_eq!(
        before.items.iter().map(|c| &c.id).collect::<Vec<_>>(),
        after.items.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

#[test]
fn recorded_failure_keeps_good_snapshot_and_revision() {
    let f = Fixture::new();
    let (source, _) = f.new_source("Failure", 2);
    let before = f.store.query_channels(&query_all()).unwrap();
    f.store
        .record_refresh_failure(&source.id, ErrorCode::Reauthorization, NOW + 20)
        .unwrap();
    let after = f.store.get_source(&source.id).unwrap();
    assert_eq!(after.revision, source.revision);
    assert_eq!(after.origin.locator, source.origin.locator);
    assert_eq!(after.last_error_code.as_deref(), Some("reauthorization"));
    assert_eq!(after.last_success_at, source.last_success_at);
    assert_eq!(
        f.store.query_channels(&query_all()).unwrap().total,
        before.total
    );
}

#[test]
fn ninety_day_purge_preserves_user_corrections() {
    let f = Fixture::new();
    let (source, _) = f.new_source("Retention", 3);
    let items = f.store.query_channels(&query_all()).unwrap().items;
    let retained = items[1].id.clone();
    let removed = items[2].id.clone();
    let edit = ChannelOverrides {
        display_name: Some("Keep this corrected channel".into()),
        ..ChannelOverrides::default()
    };
    f.store
        .set_channel_overrides(&retained, &edit, NOW)
        .unwrap();
    let one = ValidatedSnapshot::new(source.id.clone(), vec![channel(0)]).unwrap();
    let preview = f.store.preview_refresh(&one).unwrap();
    f.store
        .commit_refresh(&one, &preview, &RefreshDecision::NotRequired, NOW + 1)
        .unwrap();
    assert_eq!(
        f.store
            .purge_expired_missing(NOW + 1 + 90 * 24 * 3600 - 1)
            .unwrap(),
        0
    );
    assert_eq!(
        f.store
            .purge_expired_missing(NOW + 1 + 90 * 24 * 3600)
            .unwrap(),
        1
    );
    assert!(matches!(
        f.store.get_channel_detail(&removed),
        Err(StorageError::NotFound)
    ));
    assert_eq!(
        f.store
            .get_channel_detail(&retained)
            .unwrap()
            .effective
            .visible_name,
        "Keep this corrected channel"
    );
}

#[test]
fn stale_manual_facets_never_override_auto_metadata() {
    let f = Fixture::new();
    let (source, _) = f.new_source("Stale Facets", 1);
    let defaults = SourceDefaults {
        languages: ["de".to_owned()].into_iter().collect(),
        ..SourceDefaults::default()
    };
    f.store
        .set_source_defaults(&source.id, &defaults, NOW + 1)
        .unwrap();
    let channel_id = f.store.query_channels(&query_all()).unwrap().items[0]
        .id
        .clone();
    f.conn()
        .execute(
            "INSERT INTO iptv_channel_override_facets(channel_id,facet,value)
             VALUES(?1,'language','en')",
            [&channel_id],
        )
        .unwrap();
    let effective = f.store.get_channel_detail(&channel_id).unwrap().effective;
    assert_eq!(effective.languages, ["de".to_owned()].into_iter().collect());
    let choices = f
        .store
        .query_filter_options(&query_all(), FilterDimension::Language)
        .unwrap();
    assert_eq!(
        choices.choices,
        vec![FacetChoice {
            value: FacetValue::Known("de".into()),
            count: 1,
        }]
    );
    f.store.clear_channel_overrides(&channel_id).unwrap();
    let stray: i64 = f
        .conn()
        .query_row(
            "SELECT count(*) FROM iptv_channel_override_facets WHERE channel_id=?1",
            [&channel_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stray, 0);
}

#[test]
fn dependent_facets_count_each_channel_once() {
    let f = Fixture::new();
    let id = uuid::Uuid::new_v4().to_string();
    let mut entries = channels(3);
    entries[0].languages = ["de".to_owned(), "en".to_owned()].into_iter().collect();
    entries[1].languages = ["de".to_owned(), "en".to_owned()].into_iter().collect();
    let snap = ValidatedSnapshot::new(id.clone(), entries).unwrap();
    f.store
        .create_source_with_initial_snapshot(&id, &draft("Multiple"), &snap, NOW)
        .unwrap();
    let options = f
        .store
        .query_filter_options(&query_all(), FilterDimension::Language)
        .unwrap();
    assert!(options.choices.contains(&FacetChoice {
        value: FacetValue::Known("de".into()),
        count: 2,
    }));
    assert!(options.choices.contains(&FacetChoice {
        value: FacetValue::Known("en".into()),
        count: 2,
    }));
    assert!(options.choices.contains(&FacetChoice {
        value: FacetValue::Unknown,
        count: 1,
    }));
    let mut query = query_all();
    query.language = FacetSelection::Value("de".into());
    query.group = FacetSelection::Value("News".into());
    let groups = f
        .store
        .query_filter_options(&query, FilterDimension::Group)
        .unwrap();
    assert_eq!(
        groups.choices,
        vec![
            FacetChoice {
                value: FacetValue::Known("News".into()),
                count: 1,
            },
            FacetChoice {
                value: FacetValue::Known("Sports".into()),
                count: 1,
            },
        ]
    );
    let mut page = query_all();
    page.limit = 1;
    page.offset = 1;
    let result = f.store.query_channels(&page).unwrap();
    assert_eq!(result.total, 3);
    assert_eq!(result.items.len(), 1);
}

#[test]
fn source_scoped_identity_and_stale_preview_barrier() {
    let f = Fixture::new();
    let (left, _) = f.new_source("Left", 1);
    let (right, _) = f.new_source("Right", 1);
    let original = f.store.query_channels(&query_all()).unwrap().items;
    assert_ne!(original[0].id, original[1].id);
    let refreshed = snapshot(&left.id, 1);
    let old_preview = f.store.preview_refresh(&refreshed).unwrap();
    f.store.rename_source(&left.id, "Changed", NOW + 1).unwrap();
    assert_eq!(
        f.store
            .commit_refresh(
                &refreshed,
                &old_preview,
                &RefreshDecision::NotRequired,
                NOW + 2,
            )
            .unwrap_err(),
        StorageError::StaleRevision
    );
    assert_eq!(
        f.store.get_source(&right.id).unwrap().revision,
        right.revision
    );
    assert_eq!(f.store.query_channels(&query_all()).unwrap().total, 2);
}

#[test]
fn readonly_database_checks_cannot_mutate_alien_files() {
    let f = Fixture::new();
    let before = fs::read(&f.path).unwrap();
    // An initialized database must never be downgraded or reset on reopen.
    f.store.initialize().unwrap();
    assert_eq!(fs::read(&f.path).unwrap(), before);
    assert!(ValidatedSnapshot::new(uuid::Uuid::new_v4().to_string(), Vec::new()).is_err());
}

#[test]
fn synthetic_100_1000_10000_query_measurements() {
    for n in [100, 1_000, 10_000] {
        let f = Fixture::new();
        let start = Instant::now();
        let (_s, _) = f.new_source("Benchmark", n);
        let insert_time = start.elapsed();
        let start = Instant::now();
        let page = f.store.query_channels(&query_all()).unwrap();
        let query_time = start.elapsed();
        assert_eq!(page.total, n as u64);
        let start = Instant::now();
        let options = f
            .store
            .query_filter_options(&query_all(), FilterDimension::Group)
            .unwrap();
        let options_time = start.elapsed();
        assert_eq!(
            options.choices.iter().map(|v| v.count).sum::<u64>(),
            n as u64
        );
        // The normal test harness captures eprintln. Write directly to the
        // CI job's stderr to retain actual, synthetic-only timings.
        let info = format!(
            "IPTV-STORAGE-PERF arch={} n={n} create={insert_time:?} page={query_time:?} facet={options_time:?} rss_kib={:?}\n",
            std::env::consts::ARCH,
            current_rss_kib()
        );
        #[cfg(target_os = "linux")]
        {
            use std::io::Write;
            let mut stderr = fs::OpenOptions::new()
                .write(true)
                .open("/dev/stderr")
                .unwrap();
            stderr.write_all(info.as_bytes()).unwrap();
        }
        #[cfg(not(target_os = "linux"))]
        eprint!("{info}");
    }
}

#[cfg(target_os = "linux")]
fn current_rss_kib() -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}
#[cfg(not(target_os = "linux"))]
fn current_rss_kib() -> Option<u64> {
    None
}

#[test]
fn channel_rank_uses_same_effective_filters_and_sort_as_paged_query() {
    let f = Fixture::new();
    let (source, _) = f.new_source("Rank fixtures", 10_000);
    let mut q = query_all();
    // Rank is global even if the caller holds only a tiny/late page.
    q.limit = 1;
    q.offset = 9_999;
    let last = f.store.query_channels(&q).unwrap().items.pop().unwrap();
    assert_eq!(f.store.channel_position(&q, &last.id).unwrap(), Some(9_999));
    q.sort = SortOrder::Name;
    let ranked = f.store.channel_position(&q, &last.id).unwrap().unwrap();
    q.offset = u32::try_from(ranked).unwrap();
    assert_eq!(f.store.query_channels(&q).unwrap().items[0].id, last.id);
    q.group = FacetSelection::Value("News".into());
    let rank_in_group = f.store.channel_position(&q, &last.id).unwrap();
    // Even-numbered entries are News; odd entries are Sports.
    assert_eq!(rank_in_group, None);
    q.group = FacetSelection::Unknown;
    assert_eq!(f.store.channel_position(&q, &last.id).unwrap(), None);
    q.group = FacetSelection::Any;
    f.store
        .set_source_enabled(&source.id, false, NOW + 1)
        .unwrap();
    assert_eq!(f.store.channel_position(&q, &last.id).unwrap(), None);
    assert_eq!(
        f.store
            .channel_position(&q, &uuid::Uuid::new_v4().to_string())
            .unwrap(),
        None
    );
}

#[test]
fn logo_references_protect_other_and_disabled_sources() {
    let f = Fixture::new();
    let (first, _) = f.new_source("First playlist", 2);
    let (second, _) = f.new_source("Other playlist", 1);
    let shared = "https://example.invalid/shared.png";
    let unique = "https://example.invalid/unique.png";
    {
        let conn = f.conn();
        conn.execute(
            "UPDATE iptv_channels SET logo_url=?1 WHERE source_id=?2",
            rusqlite::params![shared, second.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE iptv_channels SET logo_url=?1 WHERE source_id=?2 AND playlist_order=0",
            rusqlite::params![shared, first.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE iptv_channels SET logo_url=?1 WHERE source_id=?2 AND playlist_order=1",
            rusqlite::params![unique, first.id],
        )
        .unwrap();
    }
    let shared_key = super::logo::cache_name(shared);
    let unique_key = super::logo::cache_name(unique);
    assert_eq!(
        f.store.referenced_logo_cache_names().unwrap(),
        std::collections::HashSet::from([shared_key.clone(), unique_key.clone()])
    );

    // Disabling/hiding a source does not unsubscribe it or delete its logos.
    f.store
        .set_source_enabled(&second.id, false, NOW + 1)
        .unwrap();
    assert!(f
        .store
        .referenced_logo_cache_names()
        .unwrap()
        .contains(&shared_key));

    // Only the unique logo may be removed when the first source is deleted.
    f.store
        .delete_source_confirmed(&ConfirmedSourceDeletion::after_user_confirmation(&first))
        .unwrap();
    assert_eq!(
        f.store.referenced_logo_cache_names().unwrap(),
        std::collections::HashSet::from([shared_key.clone()])
    );

    // A channel omitted from a committed snapshot becomes non-present.
    // Retained tombstone metadata must not indefinitely pin its logo.
    f.conn()
        .execute(
            "UPDATE iptv_channels SET is_present=0, missing_since=?1 WHERE source_id=?2",
            rusqlite::params![NOW + 2, second.id],
        )
        .unwrap();
    assert!(f.store.referenced_logo_cache_names().unwrap().is_empty());
}

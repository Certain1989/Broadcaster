// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Connected R07 source/group/language/country focus and filter intersection.
//! Use real SQLite and XTest; source locators remain reserved `.invalid` URLs.
use super::c1_mapped_native_diagnostic::{find_view, inject, spin};
use crate::{
    application::TvApplication,
    streams::{
        model::{
            ChannelQuery, IncomingChannel, OriginKind, SourceDefaults, SourceDraft, SourceOrigin,
            ValidatedSnapshot,
        },
        service::StreamsService,
        storage::Storage,
    },
};
use adw::{gio, glib, gtk, prelude::*};
use std::collections::BTreeSet;

fn row(order: u32, name: &str, group: &str, lang: &str, country: &str) -> IncomingChannel {
    IncomingChannel {
        order,
        imported_name: name.to_owned(),
        stream_url: format!("https://example.invalid/r07-facet/{order}-{name}.m3u8"),
        tvg_id: Some(format!("r07-{name}-{order}")),
        tvg_name: None,
        group_title: Some(group.to_owned()),
        logo_url: None,
        channel_number: None,
        region: None,
        languages: BTreeSet::from([lang.to_owned()]),
        countries: BTreeSet::from([country.to_owned()]),
        user_agent: None,
        referer: None,
    }
}

fn fixture_source(
    store: &Storage,
    name: &str,
    order: u32,
    entries: Vec<IncomingChannel>,
) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let draft = SourceDraft {
        name: name.to_owned(),
        origin: SourceOrigin {
            kind: OriginKind::RemoteUrl,
            locator: format!("https://example.invalid/r07-facets-{order}.m3u"),
        },
        enabled: true,
        order,
        defaults: SourceDefaults::default(),
    };
    store
        .create_source_with_initial_snapshot(
            &id,
            &draft,
            &ValidatedSnapshot::new(id.clone(), entries).unwrap(),
            1_800_000_000,
        )
        .unwrap();
    id
}

fn filter_choice(drop: &gtk::DropDown, term: &str) -> u32 {
    let list = drop.model().expect("mapped actual filter model");
    (0..list.n_items())
        .find(|index| {
            list.item(*index)
                .and_then(|obj| obj.downcast::<gtk::StringObject>().ok())
                .is_some_and(|obj| obj.string().contains(term))
        })
        .unwrap_or_else(|| panic!("R07 filter option not found: {term}"))
}

fn choose_by_keys(drop: &gtk::DropDown, item: u32) {
    assert!(drop.is_mapped() && drop.is_sensitive());
    assert!(
        drop.grab_focus(),
        "actual GTK filter receives initial remote focus"
    );
    // Start from GTK's actual final popup option. The popup initially
    // highlights a different item for a zero-based synthetic "All" row;
    // using Home + Down counts is not stable across GTK popup layouts.
    // End then Up targets the requested visible row unambiguously.
    inject("space");
    inject("End");
    let len = drop.model().expect("dropdown list").n_items();
    for _ in (item + 1)..len {
        inject("Up");
    }
    inject("Return");
    assert_eq!(
        drop.selected(),
        item,
        "physical dropdown choice and model agree"
    );
}

fn run(width: i32, height: i32, wrong_id: bool) {
    let home = std::env::var("XDG_DATA_HOME").expect("isolated native facet data dir");
    assert!(home.contains("broadcaster-c4-fixture-"));
    let store = Storage::new();
    store.initialize().expect("disposable SQLite database");
    let a = fixture_source(
        &store,
        "R07 Source A",
        0,
        vec![
            row(0, "A News", "News", "de", "DE"),
            row(1, "A Culture", "Culture", "de", "DE"),
        ],
    );
    let b = fixture_source(
        &store,
        "R07 Source B",
        1,
        vec![
            row(0, "B French News", "News", "fr", "FR"),
            row(1, "B French Culture", "Culture", "fr", "FR"),
            row(2, "B German News", "News", "de", "DE"),
        ],
    );
    let complete = store.query_channels(&ChannelQuery::default()).unwrap();
    assert_eq!(complete.total, 5);
    let target = complete
        .items
        .iter()
        .find(|row| row.source_id == b && row.visible_name == "B German News")
        .expect("source-qualified target channel")
        .id
        .clone();
    let service = glib::MainContext::default()
        .block_on(StreamsService::with_storage(store.clone()))
        .expect("real SQLite-backed service");
    adw::init().expect("GTK X11 display");
    gio::resources_register(
        &gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap(),
    );
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("real app registered");
    let win = crate::window::TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    spin();
    fn stack(widget: &gtk::Widget) -> Option<adw::ViewStack> {
        if let Ok(stack) = widget.clone().downcast::<adw::ViewStack>() {
            return Some(stack);
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if let Some(result) = stack(&next) {
                return Some(result);
            }
            child = next.next_sibling();
        }
        None
    }
    stack(win.upcast_ref())
        .expect("actual main navigation stack")
        .set_visible_child_name("streams");
    let view = find_view(win.upcast_ref()).expect("real My Channels view");
    view.install_remote_action_test_service(service, false);
    let wait_count = |expected: u32, reason: &str| {
        for _ in 0..45 {
            spin();
            if view.remote_action_test_visible_count() == expected {
                return;
            }
        }
        panic!("R07 facet {reason}: expected {expected} GTK rows");
    };
    wait_count(5, "loaded all source-qualified SQLite channels");
    let targets = view.toolbar_focus_targets();
    let controls: Vec<gtk::DropDown> = [1, 2, 4, 5]
        .into_iter()
        .map(|slot| targets[slot].clone().downcast::<gtk::DropDown>().unwrap())
        .collect();
    let (source, group, language, country) =
        (&controls[0], &controls[1], &controls[2], &controls[3]);
    // B07: real source menu and reverse, preserving the loaded source identity.
    choose_by_keys(source, filter_choice(source, "R07 Source B"));
    wait_count(3, "source B");
    let only_b = store
        .query_channels(&ChannelQuery {
            source_id: Some(b.clone()),
            ..ChannelQuery::default()
        })
        .unwrap();
    assert_eq!(only_b.total, 3);
    choose_by_keys(source, 0);
    wait_count(5, "restore all sources");
    choose_by_keys(source, filter_choice(source, "R07 Source B"));
    wait_count(3, "return to source B");
    // B10: intersect group + language + country through real GTK controls.
    choose_by_keys(group, filter_choice(group, "News"));
    wait_count(2, "source B and News");
    choose_by_keys(country, filter_choice(country, "DE"));
    wait_count(1, "source B News DE");
    choose_by_keys(language, filter_choice(language, "de"));
    wait_count(1, "source B News DE de");
    let selected = view.remote_action_test_selected_id();
    if wrong_id {
        assert_eq!(
            selected.as_deref(),
            Some(a.as_str()),
            "R07_FACETS_EXPECTED_NEGATIVE_ORACLE: unrelated playlist UUID cannot own filtered row",
        );
    }
    assert_eq!(
        selected.as_deref(),
        Some(target.as_str()),
        "one correct bound source-qualified row"
    );
    // The explicit Clear Filters button must restore the complete result
    // catalogue and every filter's visible selection without a direct setter.
    let clear = targets[7].clone().downcast::<gtk::Button>().unwrap();
    assert!(clear.grab_focus(), "B10 native Clear Filters focus");
    // Use physical Space so the shared Return injector does not move X11
    // input focus onto an unmapped dropdown popup after it has closed.
    inject("space");
    wait_count(5, "Clear Filters returns all five rows");
    for (name, control) in [
        ("source", source),
        ("group", group),
        ("language", language),
        ("country", country),
    ] {
        assert_eq!(control.selected(), 0, "B10 Clear Filters resets {name}");
    }
    // Then reapply and undo individual facets via physical keys as a
    // separate reversible path, rather than relying on Clear alone.
    choose_by_keys(source, filter_choice(source, "R07 Source B"));
    wait_count(3, "source B after Clear");
    choose_by_keys(group, filter_choice(group, "News"));
    wait_count(2, "group after Clear");
    choose_by_keys(country, filter_choice(country, "DE"));
    wait_count(1, "country after Clear");
    choose_by_keys(language, filter_choice(language, "de"));
    wait_count(1, "language after Clear");
    // All four filter states must have a physical inverse and must not mutate SQLite.
    choose_by_keys(language, 0);
    choose_by_keys(country, 0);
    choose_by_keys(group, 0);
    choose_by_keys(source, 0);
    wait_count(5, "complete native filter inversion");
    assert_eq!(
        store
            .query_channels(&ChannelQuery::default())
            .unwrap()
            .total,
        5
    );
    assert_eq!(view.remote_action_test_visible_count(), 5);
    win.destroy();
}

#[test]
#[ignore = "R07 native source and facet XTest with two real playlists, 640x480"]
fn r07_native_facets_640_480() {
    run(640, 480, false);
}

#[test]
#[ignore = "R07 native source and facet XTest with two real playlists, 1280x800"]
fn r07_native_facets_1280_800() {
    run(1280, 800, false);
}

#[test]
#[ignore = "R07 negative foreign source UUID in real faceted results"]
fn r07_native_facets_negative() {
    run(640, 480, true);
}

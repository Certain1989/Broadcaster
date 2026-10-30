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

    // B07/D05: expand the source picker beyond the initial two-source
    // boundary. Every new source owns a different SQLite UUID; physical
    // keyboard selection must still resolve the final mapped source and
    // return to the preceding source on the same focused dropdown.
    let mut extra = Vec::new();
    for index in 0..8_u32 {
        let label = format!("R07 Extra Source {index:02}");
        let id = fixture_source(
            &store,
            &label,
            index + 2,
            vec![row(index, &format!("Extra {index}"), "News", "de", "DE")],
        );
        extra.push(id);
    }
    view.source_mutated();
    wait_count(13, "expanded source dropdown after SQLite source mutation");
    assert_eq!(
        source.model().expect("source popup model").n_items(),
        11,
        "B07 all + two base + eight extra source choices",
    );
    let last = filter_choice(source, "R07 Extra Source 07");
    choose_by_keys(source, last);
    wait_count(1, "B07 last source only");
    assert_eq!(
        view.remote_action_test_selected_id(),
        store
            .query_channels(&ChannelQuery {
                source_id: Some(extra[7].clone()),
                ..ChannelQuery::default()
            })
            .unwrap()
            .items
            .first()
            .map(|row| row.id.clone()),
        "B07 final source dropdown preserves identity",
    );
    choose_by_keys(source, filter_choice(source, "R07 Source B"));
    wait_count(3, "B07 source return from last bounded item");
    let owner =
        gtk::prelude::GtkWindowExt::focus(&win).expect("B07 mapped focus after source return");
    assert!(
        source.has_focus() || owner.is_ancestor(source.upcast_ref::<gtk::Widget>()),
        "B07 focus must remain inside source dropdown after async return: {}",
        owner.type_().name()
    );

    // D05: actual mapped toolbar and footer directional reversibility.
    // Spatial layout is responsive; do not assert an invented fixed adjacent
    // control after a GTK source popover is dismissed.
    inject("Escape");
    assert!(source.grab_focus(), "D05 focused source toolbar origin");
    inject("Down");
    let down = gtk::prelude::GtkWindowExt::focus(&win).expect("D05 Down must leave mapped owner");
    assert!(
        down.is_mapped() && targets.iter().any(|w| down == *w || down.is_ancestor(w)),
        "D05 Down must remain in actual toolbar rather than escape the page"
    );
    inject("Up");
    let back = gtk::prelude::GtkWindowExt::focus(&win).expect("D05 reverse focus");
    assert!(
        source.has_focus() || back.is_ancestor(source.upcast_ref::<gtk::Widget>()),
        "D05 reverse Up must restore source portal"
    );
    assert!(clear.grab_focus(), "D05 populated footer entry");
    inject("Down");
    let footer = gtk::prelude::GtkWindowExt::focus(&win).expect("D05 native footer Down owner");
    assert!(
        footer.is_mapped()
            && (targets
                .iter()
                .any(|w| footer == *w || footer.is_ancestor(w))
                || view.contains_results_focus(&footer)),
        "D05 footer Down must stay on the current page's actions or results"
    );
    inject("Up");
    let returned = gtk::prelude::GtkWindowExt::focus(&win)
        .expect("D05 inverse from populated result or footer");
    assert!(
        returned.is_mapped()
            && (targets
                .iter()
                .any(|w| returned == *w || returned.is_ancestor(w))
                || view.contains_results_focus(&returned)),
        "D05 Up may return to another mapped header action but not a detached row"
    );

    // F05: start a native text Search debounce while a source/group/country/
    // language intersection is active, then mutate the real SQLite source
    // catalogue and reload before the pending search commits. It must not
    // resurrect a stale result or switch UUID across independent sources.
    choose_by_keys(source, filter_choice(source, "R07 Source B"));
    choose_by_keys(group, filter_choice(group, "News"));
    choose_by_keys(country, filter_choice(country, "DE"));
    choose_by_keys(language, filter_choice(language, "de"));
    wait_count(1, "F05 four filters intersect before concurrent reload");
    let sort = targets[3].clone().downcast::<gtk::DropDown>().unwrap();
    choose_by_keys(&sort, 1);
    assert_eq!(sort.selected(), 1, "F05 real sort changed by physical key");
    wait_count(1, "F05 sorted filter intersection remains source-qualified");
    let search = targets[0].clone().downcast::<gtk::SearchEntry>().unwrap();
    assert!(search.grab_focus());
    for key in ["g", "e", "r", "m", "a", "n"] {
        inject(key);
    }
    assert_eq!(
        search.text().as_str(),
        "german",
        "F05 real XTest search typing"
    );
    // Concurrent non-network change: third source appears while the 250ms
    // Search debounce and prior async filter generations are pending.
    let concurrent = fixture_source(
        &store,
        "R07 Concurrent Source",
        20,
        vec![row(0, "Concurrent German News", "News", "de", "DE")],
    );
    assert_ne!(concurrent, b);
    view.source_mutated();
    wait_count(1, "F05 post-source-change search generation");
    assert_eq!(
        view.remote_action_test_selected_id().as_deref(),
        Some(target.as_str()),
        "F05 latest search generation never takes concurrent source UUID",
    );
    let focused_search =
        gtk::prelude::GtkWindowExt::focus(&win).expect("F05 async search focused widget");
    assert!(
        search.has_focus() || focused_search.is_ancestor(search.upcast_ref::<gtk::Widget>()),
        "F05 SearchEntry or its GtkText child retains focus after rebind: {}",
        focused_search.type_().name()
    );
    assert!(clear.grab_focus());
    inject("space");
    wait_count(14, "F05 clear after concurrent real catalogue reload");
    assert!(search.text().is_empty(), "F05 clear removes debounced text");
    for control in [source, group, language, country] {
        assert_eq!(control.selected(), 0, "F05 no stale filter after clearing");
    }
    assert_eq!(sort.selected(), 0, "F05 Clear Filters resets sort too");
    assert_eq!(
        store
            .query_channels(&ChannelQuery::default())
            .unwrap()
            .total,
        14,
        "F05 no channel deletions or cross-source writes",
    );
    // Repeat a filter after the rebind to detect stale GtkDropDown handles.
    choose_by_keys(source, filter_choice(source, "R07 Source B"));
    wait_count(3, "F05 source dropdown remains actionable after rebind");
    choose_by_keys(source, 0);
    wait_count(14, "F05 final inverse");

    // B08/B09 adjacent OSK integration: these two dimensions are GTK
    // DropDown selectors, not editable fields. Test their physical
    // keyboard inverses above and verify that the actual shared My Channels
    // search OSK does not mutate either selector when Back cancels.
    crate::settings::TvSettings::get()
        .set_boolean("osk-enabled", true)
        .expect("isolated in-memory GSettings keyboard preference");
    spin();
    let keyboard = targets[11].clone().downcast::<gtk::Button>().unwrap();
    assert!(
        keyboard.is_mapped(),
        "facet search keyboard launcher enabled"
    );
    assert!(keyboard.grab_focus());
    inject("space");
    fn osk_modal(root: &gtk::Widget) -> Option<adw::Dialog> {
        if let Ok(dialog) = root.clone().downcast::<adw::Dialog>() {
            if dialog.has_css_class("osk-production") && dialog.is_mapped() {
                return Some(dialog);
            }
        }
        let mut child = root.first_child();
        while let Some(next) = child {
            if let Some(dialog) = osk_modal(&next) {
                return Some(dialog);
            }
            child = next.next_sibling();
        }
        None
    }
    let mut opened = false;
    for _ in 0..15 {
        spin();
        if osk_modal(win.upcast_ref()).is_some() {
            opened = true;
            break;
        }
    }
    assert!(
        opened,
        "B08/B09 shared search OSK opens from mapped launcher"
    );
    inject("XF86Back");
    for _ in 0..15 {
        spin();
        if osk_modal(win.upcast_ref()).is_none() {
            break;
        }
    }
    assert!(
        osk_modal(win.upcast_ref()).is_none(),
        "Back cancels nested OSK"
    );
    let restored_search =
        gtk::prelude::GtkWindowExt::focus(&win).expect("OSK Back actual Search owner");
    assert!(
        search.has_focus() || restored_search.is_ancestor(search.upcast_ref::<gtk::Widget>()),
        "OSK Back restores Search or its focused inner text widget: {}",
        restored_search.type_().name(),
    );
    assert!(
        search.text().is_empty(),
        "OSK Cancel cannot commit stale search"
    );
    assert_eq!(country.selected(), 0, "OSK cannot change country choice");
    assert_eq!(language.selected(), 0, "OSK cannot change language choice");
    crate::settings::TvSettings::get()
        .set_boolean("osk-enabled", false)
        .unwrap();
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

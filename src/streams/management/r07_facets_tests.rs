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
    // Searchable GTK DropDown popups initially focus GtkText in SearchEntry.
    // Move into their real option list before applying physical End/Up keys.
    if drop.enables_search() {
        inject("Tab");
    }
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

// B07-B09: open the actual GTK searchable DropDown popup and type into its
// native SearchEntry with XTest. Do not confuse the main channels SearchEntry
// or the global OSK with the popup's focused local search.
fn r07_popup_search_and_cancel(drop: &gtk::DropDown, win: &crate::window::TvWindow, keys: &[&str]) {
    assert!(
        drop.enables_search(),
        "this production selector must expose a searchable popup"
    );
    let selected_before = drop.selected();
    assert!(
        drop.grab_focus(),
        "selector is actually mapped before search"
    );
    inject("space");
    fn search(root: &gtk::Widget) -> Option<gtk::SearchEntry> {
        if let Ok(entry) = root.clone().downcast::<gtk::SearchEntry>() {
            if entry.is_mapped() && entry.ancestor(gtk::Popover::static_type()).is_some() {
                return Some(entry);
            }
        }
        let mut child = root.first_child();
        while let Some(next) = child {
            if let Some(entry) = search(&next) {
                return Some(entry);
            }
            child = next.next_sibling();
        }
        None
    }
    let mut entry = None;
    for _ in 0..20 {
        spin();
        entry = search(win.upcast_ref()).or_else(|| search(drop.upcast_ref()));
        if entry.is_some() {
            break;
        }
    }
    let entry = entry.expect("native GtkDropDown popup must expose a mapped, searchable GTK entry");
    let owner = gtk::prelude::GtkWindowExt::focus(win).expect("searchable popup focus");
    assert!(
        entry.has_focus() || owner.is_ancestor(&entry),
        "native keyboard focus must enter its popup SearchEntry"
    );
    for key in keys {
        inject(key);
    }
    assert_eq!(
        entry.text().chars().count(),
        keys.len(),
        "real XTest keys must enter popup search, not change main page query"
    );
    inject("Escape");
    spin();
    assert!(
        !entry.is_mapped(),
        "native Escape must dismiss searched popup"
    );
    assert_eq!(
        drop.selected(),
        selected_before,
        "popup cancel cannot commit an unintended facet filter"
    );
    let owner = gtk::prelude::GtkWindowExt::focus(win).expect("reversible popup focus");
    assert!(
        drop.has_focus() || owner.is_ancestor(drop),
        "closing popup restores actual dropdown focus"
    );
}

fn run(width: i32, height: i32, wrong_id: bool, popup_only: bool, wrong_facet: bool) {
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
    for (name, control) in [
        ("source", source),
        ("country", country),
        ("language", language),
    ] {
        assert!(
            control.enables_search(),
            "B07-B09 {name} must have native popup search enabled"
        );
    }
    if popup_only {
        r07_popup_search_and_cancel(source, &win, &["e", "x", "t", "r", "a"]);
        r07_popup_search_and_cancel(country, &win, &["f", "r"]);
        r07_popup_search_and_cancel(language, &win, &["d", "e"]);
        // B08/B09: select actual country/language popup results, not only
        // cancel their search or manipulate DropDown selected properties.
        // These two facets are native searchable GTK selectors, not OSK inputs.
        fn choose_popup_search(
            control: &gtk::DropDown,
            win: &crate::window::TvWindow,
            keys: &[&str],
            target_label: &str,
        ) {
            assert!(control.grab_focus(), "native facet search owner");
            inject("space");
            fn popup_entry(root: &gtk::Widget) -> Option<gtk::SearchEntry> {
                if let Ok(entry) = root.clone().downcast::<gtk::SearchEntry>() {
                    if entry.is_mapped() && entry.ancestor(gtk::Popover::static_type()).is_some() {
                        return Some(entry);
                    }
                }
                let mut child = root.first_child();
                while let Some(next) = child {
                    if let Some(entry) = popup_entry(&next) {
                        return Some(entry);
                    }
                    child = next.next_sibling();
                }
                None
            }
            let entry = (0..30)
                .find_map(|_| {
                    spin();
                    popup_entry(win.upcast_ref()).or_else(|| popup_entry(control.upcast_ref()))
                })
                .expect("native facet popup entry mapped");
            for key in keys {
                inject(key);
            }
            assert_eq!(entry.text().chars().count(), keys.len());
            inject("Down");
            inject("Return");
            assert_eq!(
                control.selected(),
                filter_choice(control, target_label),
                "physical facet popup search must select the intended visible label"
            );
        }
        choose_popup_search(country, &win, &["f", "r"], "FR");
        wait_count(2, "B08 physical searched French country");
        choose_popup_search(language, &win, &["f", "r"], "fr");
        wait_count(2, "B09 physical searched French language intersection");
        let french = store
            .query_channels(&ChannelQuery {
                source_id: Some(b.clone()),
                ..ChannelQuery::default()
            })
            .unwrap()
            .items
            .into_iter()
            .find(|item| item.visible_name == "B French News")
            .expect("French SQLite row identity");
        let selected_id = view.remote_action_test_selected_id();
        if wrong_facet {
            assert_eq!(
                selected_id.as_deref(),
                Some(target.as_str()),
                "R07_POPUP_FACET_EXPECTED_NEGATIVE: German source UUID cannot satisfy FR/fr"
            );
        }
        assert_eq!(
            selected_id.as_deref(),
            Some(french.id.as_str()),
            "B08/B09 real selected French country and language must retain SQLite UUID"
        );
        choose_by_keys(language, 0);
        wait_count(2, "B09 native language inverse leaves French country");
        choose_by_keys(country, 0);
        wait_count(
            5,
            "B08 native country inverse returns complete SQLite catalogue",
        );
        // Now exercise B07 over the expanded catalogue with no lingering facets.
        // B07 requires selecting a *middle* source using a substring that
        // does not begin its visible label. The two-source fixture used by
        // popup cancellation alone cannot demonstrate real search filtering.
        let mut extra = Vec::new();
        for index in 0..8_u32 {
            let id = fixture_source(
                &store,
                &format!("R07 Extra Source {index:02}"),
                index + 2,
                vec![row(index, &format!("Extra {index}"), "News", "de", "DE")],
            );
            extra.push(id);
        }
        view.source_mutated();
        wait_count(13, "searchable middle-source catalogue loaded");
        let middle_index = filter_choice(source, "R07 Extra Source 03");
        assert!(source.grab_focus(), "B07 physical source search origin");
        inject("space");
        fn popup_entry(root: &gtk::Widget) -> Option<gtk::SearchEntry> {
            if let Ok(entry) = root.clone().downcast::<gtk::SearchEntry>() {
                if entry.is_mapped() && entry.ancestor(gtk::Popover::static_type()).is_some() {
                    return Some(entry);
                }
            }
            let mut child = root.first_child();
            while let Some(next) = child {
                if let Some(entry) = popup_entry(&next) {
                    return Some(entry);
                }
                child = next.next_sibling();
            }
            None
        }
        let entry = (0..30)
            .find_map(|_| {
                spin();
                popup_entry(win.upcast_ref()).or_else(|| popup_entry(source.upcast_ref()))
            })
            .expect("B07 native popup search entry is mapped");
        for key in [
            "t", "r", "a", "space", "S", "o", "u", "r", "c", "e", "space", "0", "3",
        ] {
            inject(key);
        }
        assert_eq!(
            entry.text().as_str(),
            "tra source 03",
            "physical substring search text"
        );
        inject("Down");
        inject("Return");
        for _ in 0..45 {
            spin();
            if view.remote_action_test_visible_count() == 1 {
                break;
            }
        }
        assert_eq!(
            source.selected(),
            middle_index,
            "B07 searched middle source index"
        );
        wait_count(1, "B07 searched middle source maps exactly one channel");
        let middle_row = store
            .query_channels(&ChannelQuery {
                source_id: Some(extra[3].clone()),
                ..ChannelQuery::default()
            })
            .unwrap()
            .items[0]
            .id
            .clone();
        assert_eq!(
            view.remote_action_test_selected_id().as_deref(),
            Some(middle_row.as_str()),
            "B07 searchable popup must commit the middle source's SQLite UUID"
        );
        let foreign_row = store
            .query_channels(&ChannelQuery {
                source_id: Some(b.clone()),
                ..ChannelQuery::default()
            })
            .unwrap()
            .items[0]
            .id
            .clone();
        if wrong_id {
            assert_eq!(
                middle_row, foreign_row,
                "R07_POPUP_MIDDLE_EXPECTED_NEGATIVE: foreign Source B UUID cannot own searched source"
            );
        }
        let clear = targets[7].clone().downcast::<gtk::Button>().unwrap();
        assert!(clear.grab_focus());
        inject("space");
        wait_count(13, "B07 physical Clear restores all source rows");
        assert_eq!(
            source.selected(),
            0,
            "B07 physical Clear restores All sources"
        );
        choose_by_keys(source, filter_choice(source, "R07 Source B"));
        wait_count(3, "B07 physical inverse selects distinct Source B");
        choose_by_keys(source, 0);
        wait_count(13, "B07 inverse restores every source");
        win.destroy();
        return;
    }
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
    run(640, 480, false, false, false);
}

#[test]
#[ignore = "R07 native source and facet XTest with two real playlists, 1280x800"]
fn r07_native_facets_1280_800() {
    run(1280, 800, false, false, false);
}

#[test]
#[ignore = "R07 negative foreign source UUID in real faceted results"]
fn r07_native_facets_negative() {
    run(640, 480, true, false, false);
}

// R07 F06–F11: large real SQLite catalogue through the production virtual
// GtkListView, with mapped XTest boundary travel and page recycling. Each
// size runs in a fresh test subprocess with an isolated application data dir.
// A model count or test-helper-selected card alone never constitutes a
// physical remote focus PASS.
fn r07_native_large_catalogue(total: u32, width: i32, height: i32, negative: bool) {
    let home = std::env::var("XDG_DATA_HOME").expect("R07 isolated scaling data directory");
    assert!(home.contains("broadcaster-c4-fixture-"));
    let storage = Storage::new();
    storage.initialize().expect("real scale SQLite schema");
    let start_seed = std::time::Instant::now();
    let source_id = fixture_source(
        &storage,
        "R07 scale source",
        0,
        (0..total)
            .map(|n| row(n, &format!("Scale channel {n:05}"), "Long", "de", "DE"))
            .collect(),
    );
    assert_eq!(
        storage
            .query_channels(&ChannelQuery::default())
            .unwrap()
            .total,
        total as u64
    );
    let expected_at = |index| {
        storage
            .query_channels(&ChannelQuery {
                offset: index,
                limit: 1,
                ..ChannelQuery::default()
            })
            .expect("specific SQLite page")
            .items
            .first()
            .expect("known channel position")
            .id
            .clone()
    };
    let first_id = expected_at(0);
    let middle_id = expected_at(total / 2);
    let last_id = expected_at(total - 1);
    let seed_elapsed = start_seed.elapsed();
    if total >= 10_000 {
        assert!(
            seed_elapsed < std::time::Duration::from_secs(30),
            "R07 large SQLite seed exceeds 30s fixture boundary"
        );
    }
    let service = glib::MainContext::default()
        .block_on(StreamsService::with_storage(storage.clone()))
        .expect("real service over 15k bounded catalogue");
    adw::init().expect("GTK display");
    gio::resources_register(
        &gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap(),
    );
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("R07 registered app");
    let win = crate::window::TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    spin();
    fn find_stack(widget: &gtk::Widget) -> Option<adw::ViewStack> {
        if let Ok(stack) = widget.clone().downcast::<adw::ViewStack>() {
            return Some(stack);
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            if let Some(stack) = find_stack(&item) {
                return Some(stack);
            }
            child = item.next_sibling();
        }
        None
    }
    find_stack(win.upcast_ref())
        .unwrap()
        .set_visible_child_name("streams");
    let view = find_view(win.upcast_ref()).expect("real virtual My Channels list");
    view.install_remote_action_test_service(service, false);
    for _ in 0..60 {
        spin();
        if view.remote_action_test_visible_count() == total {
            break;
        }
    }
    assert_eq!(view.remote_action_test_visible_count(), total);
    assert!(view.focus_first_result(), "mapped list gains initial focus");
    spin();
    assert_eq!(
        view.remote_action_test_selected_id().as_deref(),
        Some(first_id.as_str())
    );
    let start_nav = std::time::Instant::now();
    // GtkListView's built-in physical End and Home movement is intentional:
    // do not substitute selecting position total-1 with a helper.
    inject("End");
    for _ in 0..65 {
        spin();
        if view.remote_action_test_selected_id().as_deref() == Some(last_id.as_str()) {
            break;
        }
    }
    assert_eq!(
        view.remote_action_test_selected_id().as_deref(),
        Some(last_id.as_str()),
        "XTest End must select real last SQLite row"
    );
    let owner = gtk::prelude::GtkWindowExt::focus(&win).expect("last row focus owner");
    assert!(owner.is_mapped() && view.contains_results_focus(&owner));
    inject("Home");
    for _ in 0..65 {
        spin();
        if view.remote_action_test_selected_id().as_deref() == Some(first_id.as_str()) {
            break;
        }
    }
    assert_eq!(
        view.remote_action_test_selected_id().as_deref(),
        Some(first_id.as_str()),
        "XTest Home must select the first source row"
    );
    // Physical PageDown from first to middle, using physical Up correction.
    // No test-only programmatic selection substitutes for remote navigation.
    use crate::keyboard::mapped::r07_osk_press;
    let mut all_ids = Vec::with_capacity(total as usize);
    for offset in (0..total).step_by(500) {
        let page = storage
            .query_channels(&ChannelQuery {
                offset,
                limit: (total - offset).min(500),
                ..ChannelQuery::default()
            })
            .expect("bounded genuine SQLite page");
        all_ids.extend(page.items);
    }
    assert_eq!(all_ids.len(), total as usize);
    let by_id = all_ids
        .iter()
        .enumerate()
        .map(|(index, channel)| (channel.id.clone(), index as u32))
        .collect::<std::collections::HashMap<_, _>>();
    let selected_position = || {
        let id = view
            .remote_action_test_selected_id()
            .expect("selected UUID");
        *by_id
            .get(&id)
            .expect("focus must remain on real SQLite UUID")
    };
    let mut jump_count = 0;
    while selected_position() < total / 2 {
        let previous = selected_position();
        r07_osk_press("Next");
        jump_count += 1;
        // Large GtkListView jumps can schedule asynchronous page hydration.
        // Keep asserting the *real selected SQLite UUID* after a bounded
        // GTK-cycle wait, never synthesize a cursor advance.
        for _ in 0..65 {
            if selected_position() > previous {
                break;
            }
            spin();
        }
        assert!(
            selected_position() > previous,
            "real PageDown stalled at {previous}/{total}"
        );
        assert!(jump_count < total / 2 + 50, "bounded PageDown travel");
    }
    let mut up_count = 0;
    while selected_position() > total / 2 {
        r07_osk_press("Up");
        up_count += 1;
        assert!(up_count < 100, "bounded native middle correction");
    }
    assert_eq!(
        view.remote_action_test_selected_id().as_deref(),
        Some(middle_id.as_str()),
        "physical PageDown/Up must reach exact middle source UUID"
    );
    r07_osk_press("Down");
    r07_osk_press("Up");
    assert_eq!(
        view.remote_action_test_selected_id().as_deref(),
        Some(middle_id.as_str()),
        "physical middle inverse must preserve UUID"
    );
    eprintln!("R07_SCALE_PHYSICAL_PATH total={total} pages={jump_count} up={up_count}");
    r07_osk_press("End");
    for _ in 0..65 {
        if view.remote_action_test_selected_id().as_deref() == Some(last_id.as_str()) {
            break;
        }
        spin();
    }
    assert_eq!(
        view.remote_action_test_selected_id().as_deref(),
        Some(last_id.as_str()),
        "physical End from middle must hydrate last UUID"
    );
    r07_osk_press("Home");
    for _ in 0..65 {
        if view.remote_action_test_selected_id().as_deref() == Some(first_id.as_str()) {
            break;
        }
        spin();
    }
    assert_eq!(
        view.remote_action_test_selected_id().as_deref(),
        Some(first_id.as_str()),
        "physical Home from last must restore first UUID"
    );
    assert_eq!(
        storage
            .query_channels(&ChannelQuery::default())
            .unwrap()
            .total,
        total as u64,
        "native list navigation must not mutate SQLite"
    );
    let elapsed = start_nav.elapsed();
    // Full physical middle/boundary traversal, not a model-only selection,
    // must stay within a measured ceiling even at the 15k source limit.
    if total >= 10_000 {
        assert!(
            elapsed < std::time::Duration::from_secs(240),
            "R07 10k/15k physical navigation exceeded the 240s latency ceiling"
        );
    }
    let rss_kib = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|line| line.starts_with("VmRSS:"))
                .map(str::to_string)
        })
        .and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        .expect("Linux process RSS");
    eprintln!(
        "R07_SCALE_MEASURE total={total} source={source_id} seed_ms={} nav_ms={} rss_kib={rss_kib}",
        seed_elapsed.as_millis(),
        elapsed.as_millis()
    );
    assert!(
        rss_kib < 900_000,
        "unexpected >900MiB resident large catalog"
    );
    if negative {
        assert_eq!(
            view.remote_action_test_selected_id().as_deref(),
            Some(last_id.as_str()),
            "R07_SCALE_EXPECTED_NEGATIVE_ORACLE: a physical Up must preserve middle UUID"
        );
    }
    win.destroy();
}

#[test]
#[ignore = "R07 284 real SQLite rows, mapped X11 navigation"]
fn r07_native_scale_284() {
    r07_native_large_catalogue(284, 640, 480, false);
}
#[test]
#[ignore = "R07 1484 real SQLite rows, mapped X11 navigation"]
fn r07_native_scale_1484() {
    r07_native_large_catalogue(1484, 1280, 800, false);
}
#[test]
#[ignore = "R07 10000 real SQLite rows, mapped X11 navigation"]
fn r07_native_scale_10000() {
    r07_native_large_catalogue(10_000, 640, 480, false);
}
#[test]
#[ignore = "R07 15000 real SQLite rows, mapped X11 navigation"]
fn r07_native_scale_15000() {
    r07_native_large_catalogue(15_000, 1280, 800, false);
}
#[test]
#[ignore = "R07 intentional false large-catalogue selected identity"]
fn r07_native_scale_negative() {
    r07_native_large_catalogue(284, 640, 480, true);
}

// R07 F10/E10: a real recycled row disappears while its focused Play
// descendant is active.  The source is removed using the same revision-bound
// confirmed deletion contract as Source Manager, then the live view reloads
// asynchronously.  No fake list model or test-only rebind replaces SQLite.
fn r07_native_recycle_removed_source(width: i32, height: i32, negative: bool) {
    use crate::streams::storage::ConfirmedSourceDeletion;
    let home = std::env::var("XDG_DATA_HOME").expect("isolated native recycled-row data dir");
    assert!(home.contains("broadcaster-c4-fixture-"));
    let store = Storage::new();
    store.initialize().expect("real disposable SQLite database");
    let surviving = fixture_source(
        &store,
        "R07 surviving source",
        0,
        (0..144)
            .map(|index| row(index, &format!("Survivor {index:03}"), "Keep", "de", "DE"))
            .collect(),
    );
    let disappearing = fixture_source(
        &store,
        "R07 vanishing source",
        1,
        (0..140)
            .map(|index| row(index, &format!("Vanishing {index:03}"), "Gone", "en", "GB"))
            .collect(),
    );
    let initial = store.query_channels(&ChannelQuery::default()).unwrap();
    assert_eq!(initial.total, 284);
    // ChannelQuery::default() returns one paginated slice even when total is
    // 284; derive the actual final ID from an explicit bounded SQLite offset.
    let disappearing_last = store
        .query_channels(&ChannelQuery {
            offset: 283,
            limit: 1,
            ..ChannelQuery::default()
        })
        .unwrap()
        .items
        .first()
        .unwrap()
        .id
        .clone();
    let survive_first = initial.items.first().unwrap().id.clone();
    let service = glib::MainContext::default()
        .block_on(StreamsService::with_storage(store.clone()))
        .expect("SQLite service");
    adw::init().expect("GTK X11 display");
    gio::resources_register(
        &gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap(),
    );
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("real registered application");
    let win = crate::window::TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    spin();
    fn stack(root: &gtk::Widget) -> Option<adw::ViewStack> {
        if let Ok(found) = root.clone().downcast::<adw::ViewStack>() {
            return Some(found);
        }
        let mut next = root.first_child();
        while let Some(child) = next {
            if let Some(found) = stack(&child) {
                return Some(found);
            }
            next = child.next_sibling();
        }
        None
    }
    stack(win.upcast_ref())
        .unwrap()
        .set_visible_child_name("streams");
    let view = find_view(win.upcast_ref()).unwrap();
    view.install_remote_action_test_service(service, false);
    for _ in 0..50 {
        spin();
        if view.remote_action_test_visible_count() == 284 {
            break;
        }
    }
    assert_eq!(view.remote_action_test_visible_count(), 284);
    assert!(view.focus_first_result());
    inject("End");
    for _ in 0..55 {
        spin();
        if view.remote_action_test_selected_id().as_deref() == Some(disappearing_last.as_str()) {
            break;
        }
    }
    assert_eq!(
        view.remote_action_test_selected_id().as_deref(),
        Some(disappearing_last.as_str()),
        "real physical End must hydrate the disappearing source's final row"
    );
    let previous_owner = gtk::prelude::GtkWindowExt::focus(&win).unwrap();
    assert!(view.contains_results_focus(&previous_owner));
    inject("Return");
    // Enter Play through the genuine row activation, not a test helper.
    let play_owner = gtk::prelude::GtkWindowExt::focus(&win).expect("expanded action focus");
    assert!(play_owner.is_mapped() && view.contains_results_focus(&play_owner));
    let token = ConfirmedSourceDeletion::after_user_confirmation(
        &store
            .get_source(&disappearing)
            .expect("actual source revision"),
    );
    store
        .delete_source_confirmed(&token)
        .expect("confirmed source disappears from actual SQLite");
    assert_eq!(
        store
            .query_channels(&ChannelQuery::default())
            .unwrap()
            .total,
        144
    );
    view.reload();
    for _ in 0..75 {
        spin();
        let id = view.remote_action_test_selected_id();
        if view.remote_action_test_visible_count() == 144
            && id.as_deref().is_some_and(|id| id != disappearing_last)
        {
            break;
        }
    }
    assert_eq!(view.remote_action_test_visible_count(), 144);
    let selected = view
        .remote_action_test_selected_id()
        .expect("rebound selected channel");
    assert!(
        store
            .query_channels(&ChannelQuery {
                limit: 200,
                ..ChannelQuery::default()
            })
            .unwrap()
            .items
            .iter()
            .any(|row| row.id == selected && row.source_id == surviving),
        "F10 rebind must select a surviving source-qualified SQLite channel"
    );
    let focus = gtk::prelude::GtkWindowExt::focus(&win).expect("rebind retains focus");
    assert!(
        focus.is_mapped() && view.contains_results_focus(&focus),
        "F10 recycled row must leave a mapped current-results focus owner"
    );
    // E10: after the actual disappearing-row rebind, physically re-enter the
    // selected surviving card, walk Right ten times, and reject focus escape
    // into another source, the global switcher or an unmapped recycled child.
    inject("Return");
    let entered = gtk::prelude::GtkWindowExt::focus(&win).expect("rebound card action");
    view.assert_remote_action_focus_owner(&entered, &selected);
    for _ in 0..10 {
        inject("Right");
        let owner = gtk::prelude::GtkWindowExt::focus(&win).expect("E10 focus owner");
        assert!(owner.is_mapped() && owner.is_sensitive());
        view.assert_remote_action_focus_owner(&owner, &selected);
    }
    inject("XF86Back");
    let back = gtk::prelude::GtkWindowExt::focus(&win).expect("E10 Back row owner");
    assert!(back.is_mapped() && view.contains_results_focus(&back));
    inject("Home");
    for _ in 0..55 {
        spin();
        if view.remote_action_test_selected_id().as_deref() == Some(survive_first.as_str()) {
            break;
        }
    }
    assert_eq!(
        view.remote_action_test_selected_id().as_deref(),
        Some(survive_first.as_str()),
        "F10 physical Home must still reach the first surviving source row"
    );
    assert!(store.get_source(&disappearing).is_err());
    assert_eq!(
        store
            .query_channels(&ChannelQuery::default())
            .unwrap()
            .total,
        144
    );
    if negative {
        assert_eq!(
            selected, disappearing_last,
            "R07_F10_EXPECTED_NEGATIVE_ORACLE: removed source UUID cannot retain selected focus"
        );
        unreachable!("wrong removed-source owner oracle unexpectedly survived");
    }
    eprintln!(
        "R07_F10_REBIND_PASS viewport={width}x{height} initial=284 after=144 selected={selected}"
    );
    win.destroy();
}

#[test]
#[ignore = "R07 B07-B09 searchable native GtkDropDown popup compact"]
fn r07_native_popup_search_640_480() {
    run(640, 480, false, true, false);
}
#[test]
#[ignore = "R07 B07-B09 searchable native GtkDropDown popup TV"]
fn r07_native_popup_search_1280_800() {
    run(1280, 800, false, true, false);
}
#[test]
#[ignore = "R07 deliberately false searchable popup selection oracle"]
fn r07_native_popup_search_negative() {
    run(640, 480, true, true, false);
}

#[test]
#[ignore = "R07 B08/B09 intentionally false French country/language source UUID"]
fn r07_native_popup_facet_negative() {
    run(640, 480, false, true, true);
}

#[test]
#[ignore = "R07 F10 native real source removal, focused 284-row rebind compact"]
fn r07_native_recycle_removed_source_640_480() {
    r07_native_recycle_removed_source(640, 480, false);
}
#[test]
#[ignore = "R07 F10 native real source removal, focused 284-row rebind TV"]
fn r07_native_recycle_removed_source_1280_800() {
    r07_native_recycle_removed_source(1280, 800, false);
}
#[test]
#[ignore = "R07 F10 deliberately wrong deleted-source focus identity"]
fn r07_native_recycle_removed_source_negative() {
    r07_native_recycle_removed_source(640, 480, true);
}

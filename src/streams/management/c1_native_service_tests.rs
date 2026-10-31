// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Real manager, private SQLite service, and physical XTest keys (not signal emission).
use super::c1_mapped_native_diagnostic::{find_view, inject, spin};
use super::*;
use crate::{application::TvApplication, streams::storage::Storage};
use rusqlite::params;
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    dir: PathBuf,
    storage: Storage,
    ids: Vec<String>,
}
impl Fixture {
    fn new(count: usize) -> Self {
        let dir = std::env::temp_dir().join(format!("broadcaster-c1-{}", uuid::Uuid::new_v4()));
        let storage = Storage::at(dir.join("test.sqlite3"));
        storage.initialize().expect("private SQLite schema");
        let mut ids = Vec::new();
        {
            let conn = storage.connect().expect("validated schema");
            for i in 0..count {
                let id = uuid::Uuid::new_v4().to_string();
                let path = dir.join(format!("playlist-{i}.m3u"));
                fs::write(
                    &path,
                    "#EXTM3U\n#EXTINF:-1,Fixture\nhttps://example.invalid/stream\n",
                )
                .expect("fabricated local M3U");
                conn.execute(
                    "INSERT INTO iptv_sources (id,name,origin_kind,origin_locator,enabled,source_order,created_at,updated_at) \
                     VALUES (?1,?2,'local_file',?3,1,?4,12345,12345)",
                    params![id, format!("Fixture {i}"), path.to_string_lossy().as_ref(), i as i64],
                ).expect("real database source");
                ids.push(id);
            }
        }
        Self { dir, storage, ids }
    }
    fn enabled(&self, id: &str) -> bool {
        self.storage
            .connect()
            .unwrap()
            .query_row(
                "SELECT enabled FROM iptv_sources WHERE id=?1",
                [id],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn until(label: &str, mut predicate: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(10);
    while Instant::now() < limit {
        spin();
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("native C1 deadline: {label}");
}
fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    fn visit(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
        out.push(w.clone());
        let mut child = w.first_child();
        while let Some(c) = child {
            visit(&c, out);
            child = c.next_sibling();
        }
    }
    let mut all = Vec::new();
    visit(root, &mut all);
    all
}
fn manager_dialog(win: &crate::window::TvWindow) -> Option<adw::Dialog> {
    descendants(win.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<adw::Dialog>().ok())
        .find(|d| d.has_css_class("broadcaster-source-manager"))
}
struct Controls {
    row: gtk::ListBoxRow,
    enabled: gtk::Switch,
    buttons: Vec<gtk::Button>,
}
fn controls(list: &gtk::ListBox, index: i32) -> Option<Controls> {
    let row = list.row_at_index(index)?;
    let items = descendants(row.upcast_ref());
    let enabled = items
        .iter()
        .find_map(|w| w.clone().downcast::<gtk::Switch>().ok())?;
    let buttons = items
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .collect::<Vec<_>>();
    assert_eq!(buttons.len(), 7, "seven real row actions");
    Some(Controls {
        row,
        enabled,
        buttons,
    })
}
fn open(
    view: &TvStreamsView,
    win: &crate::window::TvWindow,
    count: usize,
) -> (adw::Dialog, gtk::ListBox) {
    assert!(
        view.c1_test_manage_button().grab_focus(),
        "real toolbar manager entry"
    );
    inject("Return");
    until("real manager rows loaded", || {
        manager_dialog(win).is_some_and(|d| d.is_mapped())
            && view.c1_test_manager_list().is_some_and(|l| {
                (0..count).all(|i| l.row_at_index(i as i32).is_some())
                    && l.row_at_index(count as i32).is_none()
            })
    });
    (
        manager_dialog(win).unwrap(),
        view.c1_test_manager_list().unwrap(),
    )
}
fn back(dialog: &adw::Dialog, row: Option<&gtk::ListBoxRow>) {
    if let Some(row) = row {
        inject("XF86Back");
        assert!(row.has_focus(), "first physical Back -> owning row");
        assert!(dialog.is_mapped(), "first Back keeps manager open");
    }
    inject("XF86Back");
    until("second physical Back closes manager", || {
        !dialog.is_mapped()
    });
    assert!(dialog.parent().is_none(), "modal removed after Back");
}

fn run(count: usize, width: i32, height: i32, negative: bool) {
    adw::init().expect("native display");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let fixture = Fixture::new(count);
    let service = glib::MainContext::default()
        .block_on(StreamsService::with_storage(fixture.storage.clone()))
        .expect("real SQLite service");

    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("application register");
    let win = crate::window::TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    spin();
    let stack = descendants(win.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<adw::ViewStack>().ok())
        .expect("real window page stack");
    stack.set_visible_child_name("streams");
    spin();
    let view = find_view(win.upcast_ref()).expect("real My Channels view");
    view.c1_install_test_service(service);

    let (dialog, list) = open(&view, &win, count);
    if count == 0 {
        assert!(list.row_at_index(0).is_none(), "real empty manager");
        back(&dialog, None);
        win.destroy();
        return;
    }
    let first = controls(&list, 0).unwrap();
    let last = controls(&list, count as i32 - 1).unwrap();
    assert!(!first.buttons[5].is_sensitive(), "first Move Up disabled");
    assert!(!last.buttons[6].is_sensitive(), "last Move Down disabled");
    assert!(first.row.grab_focus(), "row portal");
    inject("Return");
    assert!(
        first.enabled.has_focus(),
        "physical OK enters Switch, not activation"
    );
    assert!(first.enabled.is_active(), "portal did not mutate source");
    assert_eq!(view.manager_focus(), Some((fixture.ids[0].clone(), 0, 0)));
    if negative {
        assert_eq!(
            view.manager_focus().map(|(_, slot, _)| slot),
            Some(MANAGER_ROW),
            "C1_EXPECTED_NEGATIVE_ORACLE: a real Switch focus cannot be portal slot 8"
        );
        unreachable!("test oracle did not reject incorrect expectation");
    }
    // Real source row: immediate inverse, then stale-hop invalidation after
    // unrelated external focus on Rename and Delete.
    assert!(first.buttons[1].grab_focus(), "Defaults");
    spin();
    inject("Down");
    assert!(first.buttons[4].has_focus(), "Defaults -> Delete");
    inject("Up");
    assert!(first.buttons[1].has_focus(), "Down/Up is reciprocal");
    assert!(first.buttons[1].grab_focus(), "prime stale Defaults hop");
    spin();
    inject("Down");
    assert!(first.buttons[4].has_focus());
    assert!(first.buttons[2].grab_focus(), "external Rename focus");
    spin();
    assert!(first.buttons[4].grab_focus(), "external Delete focus");
    spin();
    inject("Up");
    assert!(
        first.buttons[0].has_focus(),
        "R01-F01: fresh Delete -> Refresh, not stale Defaults"
    );
    back(&dialog, Some(&first.row));

    let (dialog, list) = open(&view, &win, count);
    let index = if count == 3 { 1 } else { 0 };
    let target = controls(&list, index).unwrap();
    assert!(target.row.grab_focus());
    inject("Return");
    assert!(target.enabled.has_focus());
    assert!(target.enabled.is_active());
    inject("space");
    let id = fixture.ids[index as usize].clone();
    until(
        "Switch persisted with restored same-UUID semantic slot",
        || {
            if fixture.enabled(&id) {
                return false;
            }
            controls(&list, index).is_some_and(|c| {
                !c.enabled.is_active()
                    && c.enabled.has_focus()
                    && view.manager_focus() == Some((id.clone(), 0, index))
            })
        },
    );
    until("old GTK row unmaps after async switch rebuild", || {
        !target.enabled.is_mapped()
    });
    for other in fixture.ids.iter().filter(|other| *other != &id) {
        assert!(fixture.enabled(other), "wrong-row source was mutated");
    }
    let reopened = glib::MainContext::default()
        .block_on(StreamsService::with_storage(fixture.storage.clone()))
        .expect("fresh SQLite service reopen");
    assert!(
        !glib::MainContext::default()
            .block_on(reopened.source(&id))
            .unwrap()
            .enabled,
        "R01-F02 persisted Switch must stay disabled after service reopen"
    );
    // AS04/AS05: perform the *inverse* native Switch change on the newly
    // rebound widget. The same persisted UUID and semantic Switch slot must
    // be retained across both asynchronous rebuilds and a fresh service read.
    let current = controls(&list, index).unwrap();
    assert!(current.enabled.has_focus(), "rebound switch retains focus");
    inject("space");
    until("Switch ON again with persisted row focus", || {
        fixture.enabled(&id)
            && controls(&list, index).is_some_and(|c| {
                c.enabled.is_active()
                    && c.enabled.has_focus()
                    && view.manager_focus() == Some((id.clone(), 0, index))
            })
    });
    until("old GTK Switch unmaps after inverse async rebuild", || {
        !current.enabled.is_mapped()
    });
    let reopened = glib::MainContext::default()
        .block_on(StreamsService::with_storage(fixture.storage.clone()))
        .expect("SQLite service reopened after inverse toggle");
    assert!(
        glib::MainContext::default()
            .block_on(reopened.source(&id))
            .unwrap()
            .enabled,
        "R07 AS04/AS05 source enabled state must survive roundtrip"
    );
    let restored = controls(&list, index).unwrap();
    back(&dialog, Some(&restored.row));
    win.destroy();
}

// R07: prove that actual physical D-pad events traverse *different* persisted
// sources in the real manager, rather than just entering a test-selected row.
fn r07_assert_source_row(
    view: &TvStreamsView,
    win: &crate::window::TvWindow,
    row: &gtk::ListBoxRow,
    id: &str,
    index: i32,
) {
    assert!(row.is_mapped(), "R07 source row {index} remains mapped");
    assert!(
        row.has_focus(),
        "R07 physical key must focus source row {index}"
    );
    let owner = gtk::prelude::GtkWindowExt::focus(win).expect("R07 window focus owner");
    assert_eq!(
        owner,
        row.clone().upcast::<gtk::Widget>(),
        "R07 focus must own the actual indexed source row"
    );
    assert_eq!(
        view.manager_focus(),
        Some((id.to_owned(), MANAGER_ROW, index)),
        "R07 focus record must match the persisted source UUID and row"
    );
}

fn r07_native_source_rows(width: i32, height: i32, negative: bool) {
    adw::init().expect("native display");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let fixture = Fixture::new(3);
    let service = glib::MainContext::default()
        .block_on(StreamsService::with_storage(fixture.storage.clone()))
        .expect("real three-source SQLite service");
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("application register");
    let win = crate::window::TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    spin();
    let stack = descendants(win.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<adw::ViewStack>().ok())
        .expect("real window page stack");
    stack.set_visible_child_name("streams");
    spin();
    let view = find_view(win.upcast_ref()).expect("real My Channels view");
    view.c1_install_test_service(service);

    // The existing C1 test already verifies physical Return on the real
    // manager launcher. Seed only its first row, not the middle/last.
    let (dialog, list) = open(&view, &win, 3);
    let rows = [0, 1, 2].map(|index| controls(&list, index).expect("bound source row"));
    assert!(rows[0].row.grab_focus(), "initial source-row portal");
    spin();
    for (index, row) in rows.iter().enumerate() {
        r07_assert_source_row(&view, &win, &row.row, &fixture.ids[index], index as i32);
        if index != 2 {
            inject("Down");
        }
    }
    if negative {
        assert_eq!(
            view.manager_focus(),
            Some((fixture.ids[1].clone(), MANAGER_ROW, 1)),
            "R07_EXPECTED_NEGATIVE_ORACLE: third source cannot masquerade as second"
        );
        unreachable!("R07 source identity negative oracle unexpectedly survived");
    }

    for index in (0..2).rev() {
        inject("Up");
        r07_assert_source_row(
            &view,
            &win,
            &rows[index].row,
            &fixture.ids[index],
            index as i32,
        );
    }
    // Enter real actions without clicking a switch, and return physically to
    // the row; source identity must remain stable across both action rows.
    inject("Return");
    assert!(rows[0].enabled.has_focus(), "source 0 Switch portal");
    assert_eq!(view.manager_focus(), Some((fixture.ids[0].clone(), 0, 0)));
    inject("Right");
    assert!(rows[0].buttons[0].has_focus(), "source 0 Refresh");
    inject("Right");
    assert!(rows[0].buttons[1].has_focus(), "source 0 Defaults");
    inject("Left");
    assert!(rows[0].buttons[0].has_focus(), "Defaults -> Refresh");
    inject("Left");
    assert!(rows[0].enabled.has_focus(), "Refresh -> Switch");
    inject("Left");
    r07_assert_source_row(&view, &win, &rows[0].row, &fixture.ids[0], 0);

    inject("Down");
    r07_assert_source_row(&view, &win, &rows[1].row, &fixture.ids[1], 1);
    inject("Return");
    assert!(rows[1].enabled.has_focus(), "source 1 must own Switch");
    assert_eq!(view.manager_focus(), Some((fixture.ids[1].clone(), 0, 1)));
    assert!(
        fixture.ids.iter().all(|id| fixture.enabled(id)),
        "pure navigation never mutates persisted enabled state"
    );
    inject("XF86Back");
    r07_assert_source_row(&view, &win, &rows[1].row, &fixture.ids[1], 1);
    back(&dialog, None);
    win.destroy();
}

// R07 AS03/AS15: traverse the entire two-line, real three-source GTK
// action grid without activating Delete/Refresh/Move. Disabled first/last
// movement edges are skipped by real XTest keys, not helper focus seeding.
fn r07_native_source_grid(width: i32, height: i32, negative: bool) {
    adw::init().expect("native GTK display");
    let resource = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
        .expect("application resources");
    gio::resources_register(&resource);
    let fixture = Fixture::new(3);
    let service = glib::MainContext::default()
        .block_on(StreamsService::with_storage(fixture.storage.clone()))
        .expect("private SQLite source service");
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered app");
    let win = crate::window::TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    spin();
    let stack = descendants(win.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<adw::ViewStack>().ok())
        .expect("real application page stack");
    stack.set_visible_child_name("streams");
    spin();
    let view = find_view(win.upcast_ref()).expect("actual My Channels view");
    view.c1_install_test_service(service);
    let (dialog, list) = open(&view, &win, 3);
    let rows = [0, 1, 2].map(|i| controls(&list, i).expect("bound SQLite source"));
    assert!(rows[0].row.grab_focus());
    spin();
    for (i, r) in rows.iter().enumerate() {
        r07_assert_source_row(&view, &win, &r.row, &fixture.ids[i], i as i32);
        inject("Return");
        assert!(r.enabled.has_focus(), "Return enters same-row Switch");
        // Upper line: Switch, Refresh, Defaults, Rename; all non-destructive
        // until explicit activation. Walk forward, back and across lines.
        for b in r.buttons.iter().take(3) {
            inject("Right");
            assert!(b.has_focus(), "upper right must stay with source {i}");
        }
        inject("Right");
        assert!(r.buttons[2].has_focus(), "upper-right edge does not escape");
        inject("Down");
        let lower_edge = if i == 2 { 5 } else { 6 };
        assert!(
            r.buttons[lower_edge].has_focus(),
            "lower right enabled edge"
        );
        if negative {
            assert!(
                r.buttons[5].has_focus(),
                "R07_AS03_EXPECTED_NEGATIVE_ORACLE: disabled first Move Up must not own focus"
            );
            unreachable!("source-grid wrong-edge negative unexpectedly survived");
        }
        inject("Left");
        if i == 1 {
            assert!(r.buttons[5].has_focus(), "middle Move Down -> Move Up");
            inject("Left");
        }
        assert!(
            r.buttons[4].has_focus(),
            "nearest lower predecessor is Delete"
        );
        inject("Left");
        assert!(r.buttons[3].has_focus(), "Delete Left returns to Replace");
        inject("Left");
        r07_assert_source_row(&view, &win, &r.row, &fixture.ids[i], i as i32);
        if i < 2 {
            inject("Down");
        }
    }
    for i in (0..2).rev() {
        inject("Up");
        r07_assert_source_row(&view, &win, &rows[i].row, &fixture.ids[i], i as i32);
    }
    assert!(
        fixture.ids.iter().all(|id| fixture.enabled(id)),
        "pure grid navigation cannot toggle sources"
    );
    let actual_order: Vec<String> = fixture
        .storage
        .connect()
        .unwrap()
        .prepare("SELECT id FROM iptv_sources ORDER BY source_order")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        actual_order, fixture.ids,
        "grid traversal must not reorder any sources"
    );
    inject("XF86Back");
    until("manager dismissed after real row Back", || {
        !dialog.is_mapped()
    });
    win.destroy();
}

#[test]
#[ignore = "R07 AS03/AS15 actual source-manager two-line D-pad graph at 640"]
fn r07_native_source_grid_640_480() {
    r07_native_source_grid(640, 480, false);
}

#[test]
#[ignore = "R07 AS03/AS15 actual source-manager two-line D-pad graph at 1280"]
fn r07_native_source_grid_1280_800() {
    r07_native_source_grid(1280, 800, false);
}

#[test]
#[ignore = "R07 AS03 wrong disabled movement edge must fail"]
fn r07_native_source_grid_negative_edge() {
    r07_native_source_grid(640, 480, true);
}

#[test]
#[ignore = "R07 genuine XTest source-row traversal, disposable SQLite"]
fn r07_native_source_rows_640_480() {
    r07_native_source_rows(640, 480, false);
}

#[test]
#[ignore = "R07 genuine XTest source-row traversal, disposable SQLite"]
fn r07_native_source_rows_1280_800() {
    r07_native_source_rows(1280, 800, false);
}

#[test]
#[ignore = "R07 deliberately wrong source UUID/index must fail"]
fn r07_native_negative_row_identity() {
    r07_native_source_rows(640, 480, true);
}

// R07 AS12/AS13: actual 3-source SQLite order changes from native D-pad,
// with row/action focus restored by source UUID after each async rebuild.
// This complements, rather than replaces, C1's switch and Back tests.
fn r07_native_source_reorder(width: i32, height: i32, negative: bool) {
    adw::init().expect("native GTK display");
    let resource = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
        .expect("resource");
    gio::resources_register(&resource);
    let fixture = Fixture::new(3);
    let service = glib::MainContext::default()
        .block_on(StreamsService::with_storage(fixture.storage.clone()))
        .expect("real SQLite source service");
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>).expect("register");
    let win = crate::window::TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    spin();
    let stack = descendants(win.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<adw::ViewStack>().ok())
        .expect("real application page stack");
    stack.set_visible_child_name("streams");
    spin();
    let view = find_view(win.upcast_ref()).expect("real My Channels");
    view.c1_install_test_service(service);
    let (dialog, list) = open(&view, &win, 3);
    let id = fixture.ids[1].clone();
    let orders = || {
        let conn = fixture.storage.connect().unwrap();
        let mut sql = conn
            .prepare("SELECT id FROM iptv_sources ORDER BY source_order")
            .unwrap();
        sql.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(orders(), fixture.ids);
    let middle = controls(&list, 1).expect("bound middle source");
    assert!(middle.row.grab_focus());
    spin();
    inject("Return");
    assert!(middle.enabled.has_focus(), "physical OK enters Switch");
    inject("Right");
    assert!(middle.buttons[0].has_focus(), "Right enters real Refresh");
    inject("Down");
    assert!(middle.buttons[4].has_focus(), "Down enters real Delete row");
    inject("Right");
    assert!(
        middle.buttons[5].has_focus(),
        "Right enters enabled Move Up"
    );
    inject("Return");
    until("source moved up in real SQLite", || {
        orders()
            == [
                fixture.ids[1].clone(),
                fixture.ids[0].clone(),
                fixture.ids[2].clone(),
            ]
            && view
                .manager_focus()
                .is_some_and(|(source, _, index)| source == id && index == 0)
    });
    if negative {
        assert_eq!(
            orders()[0],
            fixture.ids[0],
            "R07_AS12_EXPECTED_NEGATIVE_ORACLE: unchanged source order"
        );
        unreachable!("R07 reorder negative must fail");
    }
    let first = controls(&list, 0).expect("rebound moved source");
    assert!(!middle.row.is_mapped(), "old GTK row unmapped after rebind");
    assert_eq!(view.manager_focus().unwrap().0, id);
    assert!(first.row.grab_focus(), "rebound source row");
    spin();
    inject("Return");
    assert!(first.enabled.has_focus());
    inject("Right");
    assert!(first.buttons[0].has_focus());
    inject("Down");
    assert!(
        first.buttons[4].has_focus(),
        "real Delete as lower row entry"
    );
    inject("Right");
    assert!(
        first.buttons[6].has_focus(),
        "disabled Move Up skipped to enabled Move Down"
    );
    inject("Return");
    until(
        "source moved back down in SQLite and focus identity restored",
        || {
            orders() == fixture.ids
                && view
                    .manager_focus()
                    .is_some_and(|(source, _, index)| source == id && index == 1)
        },
    );
    for id in &fixture.ids {
        assert!(
            fixture.enabled(id),
            "navigation/reorder must not toggle source"
        );
    }
    let rebound = controls(&list, 1).expect("restored middle source");
    back(&dialog, Some(&rebound.row));
    win.destroy();
}

#[test]
#[ignore = "R07 mapped SQLite manager Move Up and Move Down 640x480"]
fn r07_native_source_reorder_640_480() {
    r07_native_source_reorder(640, 480, false);
}

#[test]
#[ignore = "R07 mapped SQLite manager Move Up and Move Down 1280x800"]
fn r07_native_source_reorder_1280_800() {
    r07_native_source_reorder(1280, 800, false);
}

#[test]
#[ignore = "R07 deliberately false source order after physical Move Up"]
fn r07_native_source_reorder_negative_order() {
    r07_native_source_reorder(640, 480, true);
}

#[test]
#[ignore = "real GTK/XTest and SQLite service"]
fn c1_native_manager_0_640_480() {
    run(0, 640, 480, false);
}
#[test]
#[ignore = "real GTK/XTest and SQLite service"]
fn c1_native_manager_1_640_480() {
    run(1, 640, 480, false);
}
#[test]
#[ignore = "real GTK/XTest and SQLite service"]
fn c1_native_manager_3_640_480() {
    run(3, 640, 480, false);
}
#[test]
#[ignore = "real GTK/XTest and SQLite service"]
fn c1_native_manager_0_1280_800() {
    run(0, 1280, 800, false);
}
#[test]
#[ignore = "real GTK/XTest and SQLite service"]
fn c1_native_manager_1_1280_800() {
    run(1, 1280, 800, false);
}
#[test]
#[ignore = "real GTK/XTest and SQLite service"]
fn c1_native_manager_3_1280_800() {
    run(3, 1280, 800, false);
}
#[test]
#[ignore = "intentionally broken real GTK oracle; CI must see RED"]
fn c1_native_negative_oracle() {
    run(1, 640, 480, true);
}

// R07 AS16: differentiate real remote Back, native Escape, and remote Home
// across three fresh manager modal sessions, with no source mutation.
fn r07_native_source_modal_semantics(width: i32, height: i32, negative: bool) {
    adw::init().expect("native GTK display");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let fixture = Fixture::new(3);
    let service = glib::MainContext::default()
        .block_on(StreamsService::with_storage(fixture.storage.clone()))
        .expect("source-backed SQLite service");
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered app");
    let win = crate::window::TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    spin();
    let stack = descendants(win.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<adw::ViewStack>().ok())
        .expect("real application stack");
    stack.set_visible_child_name("streams");
    spin();
    let view = find_view(win.upcast_ref()).expect("actual My Channels view");
    view.c1_install_test_service(service);

    // Back from the action moves to the owning source row; a second Back
    // dismisses the dialog but leaves the original Streams page selected.
    let (back_dialog, back_list) = open(&view, &win, 3);
    let row = controls(&back_list, 1).expect("middle source");
    assert!(row.row.grab_focus());
    spin();
    inject("Return");
    assert!(row.enabled.has_focus(), "real middle source Switch");
    inject("XF86Back");
    r07_assert_source_row(&view, &win, &row.row, &fixture.ids[1], 1);
    assert!(back_dialog.is_mapped(), "first Back cannot close manager");
    inject("XF86Back");
    until("second Back dismisses source manager", || {
        !back_dialog.is_mapped()
    });
    assert_eq!(stack.visible_child_name().as_deref(), Some("streams"));

    // Escape is a GTK modal dismiss, not a global remote Home command.
    let (escape_dialog, escape_list) = open(&view, &win, 3);
    assert!(controls(&escape_list, 0).unwrap().row.grab_focus());
    spin();
    inject("Escape");
    until("Escape dismisses only manager modal", || {
        !escape_dialog.is_mapped()
    });
    assert_eq!(stack.visible_child_name().as_deref(), Some("streams"));

    // The actual remote Home key dismisses the modal and navigates to the
    // home Mediathek page. Distinguish it from Back and Escape explicitly.
    let (home_dialog, home_list) = open(&view, &win, 3);
    assert!(controls(&home_list, 2).unwrap().row.grab_focus());
    spin();
    inject("XF86HomePage");
    until("remote Home closes source modal", || {
        !home_dialog.is_mapped()
    });
    assert_eq!(stack.visible_child_name().as_deref(), Some("mediathek"));
    if negative {
        assert_eq!(
            stack.visible_child_name().as_deref(),
            Some("streams"),
            "R07_AS16_EXPECTED_NEGATIVE_ORACLE: Home must not behave like Back"
        );
        unreachable!("R07 AS16 deliberate wrong Home oracle unexpectedly survived");
    }
    assert!(fixture.ids.iter().all(|id| fixture.enabled(id)));
    win.destroy();
}

#[test]
#[ignore = "R07 AS16 real source modal Back Escape Home 640x480"]
fn r07_native_source_modal_640_480() {
    r07_native_source_modal_semantics(640, 480, false);
}

#[test]
#[ignore = "R07 AS16 real source modal Back Escape Home 1280x800"]
fn r07_native_source_modal_1280_800() {
    r07_native_source_modal_semantics(1280, 800, false);
}

#[test]
#[ignore = "R07 AS16 deliberately false Home equals Back oracle"]
fn r07_native_source_modal_negative_home() {
    r07_native_source_modal_semantics(640, 480, true);
}

// Source-manager R07 batch: exercise the real mapped dialog sequence rather
// than invoking service callbacks or constructing detached buttons.
fn source_sheet(win: &crate::window::TvWindow, title: &str) -> Option<adw::Dialog> {
    descendants(win.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::Dialog>().ok())
        .find(|dialog| dialog.is_mapped() && dialog.title() == title)
}

fn source_sheet_button(dialog: &adw::Dialog, text: &str) -> gtk::Button {
    descendants(dialog.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some(text))
        .unwrap_or_else(|| panic!("R07 mapped {text} action in {}", dialog.title()))
}

fn source_sheet_entries(dialog: &adw::Dialog) -> Vec<gtk::Entry> {
    descendants(dialog.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Entry>().ok())
        .collect()
}

fn source_action(
    win: &crate::window::TvWindow,
    list: &gtk::ListBox,
    index: i32,
    slot: usize,
    title: &str,
) -> adw::Dialog {
    let action = controls(list, index)
        .expect("source row before action")
        .buttons[slot]
        .clone();
    assert!(
        action.is_mapped() && action.grab_focus(),
        "mapped source action {slot}"
    );
    inject("Return");
    until(title, || source_sheet(win, title).is_some());
    source_sheet(win, title).expect("actual mapped dialog")
}

fn press_sheet(dialog: &adw::Dialog, name: &str) {
    let action = source_sheet_button(dialog, name);
    assert!(
        action.is_mapped() && action.grab_focus(),
        "mapped native {name}"
    );
    inject("Return");
}

fn source_fixture_window(
    count: usize,
    width: i32,
    height: i32,
) -> (
    Fixture,
    crate::window::TvWindow,
    TvStreamsView,
    adw::Dialog,
    gtk::ListBox,
) {
    adw::init().expect("native GTK display");
    let res = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
        .expect("application GResource");
    gio::resources_register(&res);
    let fixture = Fixture::new(count);
    let service = glib::MainContext::default()
        .block_on(StreamsService::with_storage(fixture.storage.clone()))
        .expect("real SQLite-backed source service");
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered native application");
    let win = crate::window::TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    spin();
    let stack = descendants(win.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::ViewStack>().ok())
        .expect("actual category stack");
    stack.set_visible_child_name("streams");
    spin();
    let view = find_view(win.upcast_ref()).expect("actual My Channels category");
    view.c1_install_test_service(service);
    let (dialog, list) = open(&view, &win, count);
    (fixture, win, view, dialog, list)
}

fn native_source_editors(width: i32, height: i32, negative: bool) {
    let (fixture, win, view, manager, list) = source_fixture_window(3, width, height);
    let first_id = fixture.ids[0].clone();
    let middle_id = fixture.ids[1].clone();
    let last_id = fixture.ids[2].clone();

    // AS09: source-qualified Rename has a validation failure that leaves
    // SQLite untouched, followed by a real save and asynchronously rebound
    // same-row action.  The physical key activates the actual dialog button.
    let rename = source_action(&win, &list, 0, 2, "Rename Source");
    let name = source_sheet_entries(&rename)
        .into_iter()
        .next()
        .expect("name entry");
    assert_eq!(name.text().as_str(), "Fixture 0");
    name.set_text("");
    press_sheet(&rename, "Save");
    assert!(rename.is_mapped(), "empty rename rejected by real dialog");
    assert_eq!(
        fixture.storage.get_source(&first_id).unwrap().name,
        "Fixture 0"
    );
    name.set_text("R07 native renamed source");
    press_sheet(&rename, "Save");
    until("source Rename persists and closes editor", || {
        !rename.is_mapped()
            && fixture
                .storage
                .get_source(&first_id)
                .is_ok_and(|source| source.name == "R07 native renamed source")
    });
    until("source Rename restores mapped same-source focus", || {
        controls(&list, 0).is_some_and(|row| {
            row.buttons[2].has_focus()
                || row.row.has_focus()
                || gtk::prelude::GtkWindowExt::focus(&win)
                    .is_some_and(|owner| owner.is_mapped() && row.row.is_ancestor(&owner))
        })
    });
    assert_eq!(
        fixture.storage.get_source(&middle_id).unwrap().name,
        "Fixture 1"
    );
    if negative {
        assert_eq!(
            fixture.storage.get_source(&middle_id).unwrap().name,
            "R07 native renamed source",
            "R07_SOURCE_DIALOG_EXPECTED_NEGATIVE_ORACLE: rename may not mutate another UUID"
        );
    }

    // AS08: Defaults form is the production long-form scroll/Expander
    // with typed region fields. Invalid oversized code must not write;
    // valid code+label persists to a new SQLite connection and rebinds.
    let defaults = source_action(&win, &list, 1, 1, "Edit Source Defaults");
    let fields = source_sheet_entries(&defaults);
    assert!(fields.len() >= 2, "real advanced region code/name fields");
    let code = &fields[fields.len() - 2];
    let label = &fields[fields.len() - 1];
    code.set_text(&"A".repeat(65));
    press_sheet(&defaults, "Save");
    assert!(
        defaults.is_mapped(),
        "oversized region must not close defaults editor"
    );
    assert!(fixture
        .storage
        .get_source(&middle_id)
        .unwrap()
        .defaults
        .region
        .is_none());
    code.set_text("DE-BY");
    label.set_text("Bavaria");
    press_sheet(&defaults, "Save");
    until("persist Defaults and return to same source", || {
        !defaults.is_mapped()
            && fixture.storage.get_source(&middle_id).is_ok_and(|src| {
                src.defaults.region.as_ref().is_some_and(|region| {
                    region.code.as_deref() == Some("DE-BY")
                        && region.label.as_deref() == Some("Bavaria")
                })
            })
            && controls(&list, 1).is_some_and(|row| row.buttons[1].has_focus())
    });
    assert!(fixture
        .storage
        .get_source(&first_id)
        .unwrap()
        .defaults
        .region
        .is_none());

    // AS11 and AS10: Replace chooser must offer local/remote paths; an
    // invalid non-HTTP remote address must not alter a local source or
    // reach the network. Cancel returns to the populated source manager.
    let replace = source_action(&win, &list, 2, 3, "Replace Playlist");
    let local = source_sheet_button(&replace, "Reselect Local File");
    assert!(
        local.is_mapped() && local.is_sensitive(),
        "local chooser offered"
    );
    press_sheet(&replace, "Replace HTTP(S) URL");
    until("remote replace URL editor", || {
        source_sheet(&win, "Replace Playlist URL").is_some()
    });
    let replace_url = source_sheet(&win, "Replace Playlist URL").unwrap();
    let input = source_sheet_entries(&replace_url)
        .into_iter()
        .next()
        .expect("URL entry");
    input.set_text("file:///local-only.m3u");
    press_sheet(&replace_url, "Replace");
    assert!(replace_url.is_mapped(), "invalid URL retained in form");
    assert_eq!(
        fixture.storage.get_source(&last_id).unwrap().origin.kind,
        OriginKind::LocalFile,
        "invalid URL cannot mutate persisted local source"
    );
    press_sheet(&replace_url, "Cancel");
    until("URL editor Cancel", || !replace_url.is_mapped());
    assert!(manager.is_mapped());
    assert_eq!(
        fixture.storage.get_source(&last_id).unwrap().name,
        "Fixture 2"
    );

    // The source's nonnetwork Refresh remains a separate asynchronous
    // contract. These editor checks must not depend on whether the fixture
    // local file is sandbox-authorized by the runtime.
    let refresh = controls(&list, 0).unwrap().buttons[0].clone();
    assert!(refresh.is_mapped() && refresh.is_sensitive());
    assert!(fixture.storage.get_source(&first_id).is_ok());
    manager.close();
    until("source manager tear down", || !manager.is_mapped());
    view.c1_test_manage_button().grab_focus();
    win.destroy();
}

fn native_source_delete_ordinal(width: i32, height: i32, index: i32, negative: bool) {
    // Each ordinal runs on an independent 3-source SQLite fixture: a
    // removed row must neither mutate nor steal focus from a neighbour.
    let (fixture, win, _view, manager, list) = source_fixture_window(3, width, height);
    let removed_id = fixture.ids[index as usize].clone();
    let before = fixture
        .storage
        .get_source(&removed_id)
        .expect("source exists");
    let delete = source_action(&win, &list, index, 4, "Delete Source?");
    press_sheet(&delete, "Keep Source");
    until("Keep Source dismissed without mutation", || {
        !delete.is_mapped()
    });
    assert!(fixture.storage.get_source(&removed_id).is_ok());
    let delete = source_action(&win, &list, index, 4, "Delete Source?");
    press_sheet(&delete, "Delete Permanently");
    until("actual source delete and two-row rebind", || {
        !delete.is_mapped()
            && fixture.storage.get_source(&removed_id).is_err()
            && list.row_at_index(2).is_none()
            && list.row_at_index(1).is_some()
    });
    assert!(!before.name.is_empty());
    for surviving in &fixture.ids {
        if *surviving != removed_id {
            assert!(
                fixture.storage.get_source(surviving).is_ok(),
                "wrong UUID removed"
            );
        }
    }
    until("deleted row yields visible mapped manager focus", || {
        gtk::prelude::GtkWindowExt::focus(&win).is_some_and(|owner| owner.is_mapped())
    });
    assert!(manager.is_mapped(), "manager survives native deletion");
    if negative {
        assert!(
            fixture.storage.get_source(&removed_id).is_ok(),
            "R07_SOURCE_DELETE_EXPECTED_NEGATIVE_ORACLE: confirmed removal must delete exact UUID"
        );
    }
    manager.close();
    until("manager removed after source deletion", || {
        !manager.is_mapped()
    });
    win.destroy();
}

// Connected AS08/AS09/AS11/C06 regression: operate the production K2 OSK
// through mapped launcher/buttons, not by emitting signal handlers. Every
// source ID is real SQLite and each modal belongs to an actual manager row.
fn osk_for_source_field(win: &crate::window::TvWindow, entry: &gtk::Entry) -> adw::Dialog {
    let parent = entry.parent().expect("editable row around real field");
    let launcher = descendants(&parent)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|button| {
            button.icon_name().as_deref() == Some("input-keyboard-symbolic") && button.is_mapped()
        })
        .expect("mapped real OSK launcher beside editable");
    assert!(launcher.grab_focus(), "physical OSK launcher focus");
    inject("Return");
    until("real K2 OSK opened", || {
        source_sheet(win, "On-screen keyboard").is_some()
    });
    let osk = source_sheet(win, "On-screen keyboard").unwrap();
    assert!(osk.has_css_class("osk-production"));
    osk
}

fn osk_key(osk: &adw::Dialog, key: &str) {
    press_sheet(osk, key);
    spin();
}

fn native_source_osk_forms(width: i32, height: i32, negative: bool) {
    crate::settings::TvSettings::get()
        .set_boolean("osk-enabled", true)
        .expect("test GSettings enables OSK");
    let (fixture, win, _view, manager, list) = source_fixture_window(3, width, height);
    let [first, middle, last] = [
        fixture.ids[0].clone(),
        fixture.ids[1].clone(),
        fixture.ids[2].clone(),
    ];

    // Rename: Cancel an edited draft without mutating the source, then
    // re-open and type a new name using actual K2 key buttons/Done.
    let rename = source_action(&win, &list, 0, 2, "Rename Source");
    let name = source_sheet_entries(&rename)[0].clone();
    let osk = osk_for_source_field(&win, &name);
    osk_key(&osk, "Clear");
    osk_key(&osk, "q");
    let preview = source_sheet_entries(&osk)[0].clone();
    assert_eq!(preview.text().as_str(), "q");
    osk_key(&osk, "←");
    assert_eq!(preview.position(), 0, "real OSK caret left");
    osk_key(&osk, "→");
    assert_eq!(preview.position(), 1, "real OSK caret right");
    osk_key(&osk, "Cancel");
    until("K2 Rename cancel", || !osk.is_mapped());
    assert_eq!(
        name.text().as_str(),
        "Fixture 0",
        "OSK Cancel preserves draft origin"
    );
    assert_eq!(
        fixture.storage.get_source(&first).unwrap().name,
        "Fixture 0"
    );
    let osk = osk_for_source_field(&win, &name);
    osk_key(&osk, "Clear");
    osk_key(&osk, "q");
    osk_key(&osk, "w");
    osk_key(&osk, "Done");
    until("K2 Rename commits editable", || {
        !osk.is_mapped() && name.text().as_str() == "qw"
    });
    press_sheet(&rename, "Save");
    until("K2 Rename SQLite save and focus restoration", || {
        !rename.is_mapped()
            && fixture
                .storage
                .get_source(&first)
                .is_ok_and(|s| s.name == "qw")
            && gtk::prelude::GtkWindowExt::focus(&win).is_some_and(|f| f.is_mapped())
    });
    assert_eq!(
        fixture.storage.get_source(&middle).unwrap().name,
        "Fixture 1"
    );
    if negative {
        assert_eq!(
            fixture.storage.get_source(&middle).unwrap().name,
            "qw",
            "R07_SOURCE_OSK_EXPECTED_NEGATIVE_ORACLE: OSK Rename must not target foreign UUID"
        );
    }

    // Advanced Defaults is a real expanded, scrollable form. Physical OSK
    // Done applies a separate draft to region code; native Save persists.
    let defaults = source_action(&win, &list, 1, 1, "Edit Source Defaults");
    let entries = source_sheet_entries(&defaults);
    let code = entries[entries.len() - 2].clone();
    let region_label = entries[entries.len() - 1].clone();
    assert!(code.grab_focus(), "advanced Defaults code focusable");
    spin();
    assert!(code.is_mapped(), "expanded real Defaults region code");
    let osk = osk_for_source_field(&win, &code);
    osk_key(&osk, "Clear");
    osk_key(&osk, "d");
    osk_key(&osk, "e");
    osk_key(&osk, "Done");
    until("K2 region draft committed", || {
        !osk.is_mapped() && code.text().as_str() == "de"
    });
    region_label.set_text("Germany");
    press_sheet(&defaults, "Save");
    until("K2 source region persisted", || {
        !defaults.is_mapped()
            && fixture.storage.get_source(&middle).is_ok_and(|s| {
                s.defaults.region.as_ref().is_some_and(|r| {
                    r.code.as_deref() == Some("de") && r.label.as_deref() == Some("Germany")
                })
            })
    });

    // Replace URL: K2's URL validator rejects a non-HTTP draft before any
    // network call or SQLite mutation, and Back/Cancel cannot commit it.
    let replace = source_action(&win, &list, 2, 3, "Replace Playlist");
    press_sheet(&replace, "Replace HTTP(S) URL");
    until("K2 Replace URL editor", || {
        source_sheet(&win, "Replace Playlist URL").is_some()
    });
    let url = source_sheet(&win, "Replace Playlist URL").unwrap();
    let entry = source_sheet_entries(&url)[0].clone();
    let osk = osk_for_source_field(&win, &entry);
    let preview = source_sheet_entries(&osk)[0].clone();
    preview.set_text("file:///blocked.m3u");
    osk_key(&osk, "Done");
    assert!(osk.is_mapped(), "K2 rejects non-HTTP URL without saving");
    assert!(
        entry.text().is_empty(),
        "failed URL validator cannot write origin"
    );
    osk_key(&osk, "Cancel");
    until("K2 URL cancel", || !osk.is_mapped());
    assert!(entry.text().is_empty());
    press_sheet(&url, "Cancel");
    until("K2 replacement URL editor closed", || !url.is_mapped());
    assert_eq!(
        fixture.storage.get_source(&last).unwrap().origin.kind,
        OriginKind::LocalFile
    );
    assert!(
        manager.is_mapped(),
        "source manager remains after K2 cancel"
    );
    manager.close();
    until("source manager closed after K2", || !manager.is_mapped());
    win.destroy();
}

#[test]
#[ignore = "R07 real source Rename Defaults Replace K2 OSK physical compact"]
fn r07_native_source_osk_640_480() {
    native_source_osk_forms(640, 480, false);
}

#[test]
#[ignore = "R07 real source Rename Defaults Replace K2 OSK physical TV"]
fn r07_native_source_osk_1280_800() {
    native_source_osk_forms(1280, 800, false);
}

#[test]
#[ignore = "R07 deliberate foreign UUID after real K2 source Rename"]
fn r07_native_source_osk_negative() {
    native_source_osk_forms(640, 480, true);
}

#[test]
#[ignore = "R07 source manager connected Rename/Defaults/Replace/Refresh, 640x480 XTest"]
fn r07_native_source_editors_640_480() {
    native_source_editors(640, 480, false);
}

#[test]
#[ignore = "R07 source manager connected Rename/Defaults/Replace/Refresh, 1280x800 XTest"]
fn r07_native_source_editors_1280_800() {
    native_source_editors(1280, 800, false);
}

#[test]
#[ignore = "R07 negative wrong UUID during mapped source Rename"]
fn r07_native_source_editors_negative() {
    native_source_editors(640, 480, true);
}

#[test]
#[ignore = "R07 physical first-source Keep/Delete and focus fallback compact"]
fn r07_native_source_delete_first_640_480() {
    native_source_delete_ordinal(640, 480, 0, false);
}
#[test]
#[ignore = "R07 physical middle-source Keep/Delete and focus fallback compact"]
fn r07_native_source_delete_middle_640_480() {
    native_source_delete_ordinal(640, 480, 1, false);
}
#[test]
#[ignore = "R07 physical last-source Keep/Delete and focus fallback compact"]
fn r07_native_source_delete_last_640_480() {
    native_source_delete_ordinal(640, 480, 2, false);
}
#[test]
#[ignore = "R07 physical first-source Keep/Delete and focus fallback TV"]
fn r07_native_source_delete_first_1280_800() {
    native_source_delete_ordinal(1280, 800, 0, false);
}
#[test]
#[ignore = "R07 physical middle-source Keep/Delete and focus fallback TV"]
fn r07_native_source_delete_middle_1280_800() {
    native_source_delete_ordinal(1280, 800, 1, false);
}
#[test]
#[ignore = "R07 physical last-source Keep/Delete and focus fallback TV"]
fn r07_native_source_delete_last_1280_800() {
    native_source_delete_ordinal(1280, 800, 2, false);
}
#[test]
#[ignore = "R07 wrong persisted source deletion negative control"]
fn r07_native_source_deletions_negative() {
    native_source_delete_ordinal(640, 480, 1, true);
}

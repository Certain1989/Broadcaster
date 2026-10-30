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

// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Integrated R07 physical XTest regressions for the real Live/Mediathek
//! toolbar entry and reverse route. Deliberately isolated GTK processes.
use super::*;
use std::{thread, time::Duration};

fn settle() {
    for _ in 0..20 {
        for _ in 0..64 {
            if !glib::MainContext::default().pending() {
                break;
            }
            glib::MainContext::default().iteration(false);
        }
        thread::sleep(Duration::from_millis(16));
    }
}

fn inject(key: &str, initial: bool) {
    // A real X11 key event to the mapped app window, never direct router
    // invocation or gtk_widget_grab_focus substituted for a D-pad event.
    const SCRIPT: &str = r#"
import ctypes as c,sys
x,t=c.CDLL('libX11.so.6'),c.CDLL('libXtst.so.6')
x.XOpenDisplay.argtypes=[c.c_char_p];x.XOpenDisplay.restype=c.c_void_p
d=x.XOpenDisplay(None);assert d
x.XDefaultRootWindow.argtypes=[c.c_void_p];x.XDefaultRootWindow.restype=c.c_ulong
x.XQueryTree.argtypes=[c.c_void_p,c.c_ulong,c.POINTER(c.c_ulong),c.POINTER(c.c_ulong),c.POINTER(c.POINTER(c.c_ulong)),c.POINTER(c.c_uint)]
x.XQueryTree.restype=c.c_int
root=x.XDefaultRootWindow(d)
a,b,n=c.c_ulong(),c.c_ulong(),c.c_uint();ch=c.POINTER(c.c_ulong)()
assert x.XQueryTree(d,root,c.byref(a),c.byref(b),c.byref(ch),c.byref(n))
assert n.value
x.XSetInputFocus.argtypes=[c.c_void_p,c.c_ulong,c.c_int,c.c_ulong]
if sys.argv[2]=='initial':x.XSetInputFocus(d,ch[n.value-1],1,0)
x.XFree.argtypes=[c.c_void_p];x.XFree(c.cast(ch,c.c_void_p))
x.XStringToKeysym.argtypes=[c.c_char_p];x.XStringToKeysym.restype=c.c_ulong
x.XKeysymToKeycode.argtypes=[c.c_void_p,c.c_ulong];x.XKeysymToKeycode.restype=c.c_uint
code=x.XKeysymToKeycode(d,x.XStringToKeysym(sys.argv[1].encode()));assert code
t.XTestFakeKeyEvent.argtypes=[c.c_void_p,c.c_uint,c.c_int,c.c_ulong]
assert t.XTestFakeKeyEvent(d,code,1,0) and t.XTestFakeKeyEvent(d,code,0,0)
x.XFlush.argtypes=[c.c_void_p];x.XFlush(d);x.XCloseDisplay.argtypes=[c.c_void_p];x.XCloseDisplay(d)
"#;
    let mode = if initial { "initial" } else { "keep" };
    let output = std::process::Command::new("python3")
        .args(["-c", SCRIPT, key, mode])
        .output()
        .expect("native XTest injector");
    assert!(
        output.status.success(),
        "XTest {key}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    settle();
}

fn focused_inside(win: &TvWindow, target: &gtk::Widget, label: &str) {
    let owner = win
        .focused_widget()
        .expect("R07 actual GTK window focus owner");
    assert!(
        owner.is_mapped() && owner.is_visible() && owner.is_sensitive(),
        "R07 {label}: focused widget must be mapped, visible and sensitive"
    );
    assert!(
        TvWindow::is_within(&owner, target),
        "R07 {label}: unexpected focused widget type {}",
        owner.type_().name()
    );
}

fn run(page: &str, width: i32, height: i32, negative: bool) {
    adw::init().expect("native GTK display");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("real app register");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name(page);
    if page == "live" {
        win.imp().live_view.show_toolbar_for_test();
        win.imp().live_view.install_r07_native_channel_rows();
    }
    settle();

    let page_widget: &gtk::Widget = if page == "live" {
        win.imp().live_view.upcast_ref()
    } else {
        win.imp().mediathek_view.upcast_ref()
    };
    let targets = if page == "live" {
        win.imp().live_view.toolbar_focus_targets()
    } else {
        win.imp().mediathek_view.toolbar_focus_targets()
    };
    let search = &targets[0];
    assert!(search.is_mapped(), "R07 mapped Search");
    assert!(search.grab_focus(), "R07 seed focus on real Search");
    settle();
    focused_inside(&win, search, "initial search");

    inject("Down", true);
    let after_down = win.focused_widget().expect("R07 Down focus owner");
    if negative {
        assert!(
            TvWindow::is_within(&after_down, search),
            "R07_EXPECTED_NEGATIVE_ORACLE: successful Down must leave Search"
        );
        unreachable!("R07 deliberately wrong focus assertion did not fail");
    }
    if page == "live" {
        let list = TvWindow::find_descendant(page_widget, &|widget| widget.is::<gtk::ListBox>())
            .and_then(|widget| widget.downcast::<gtk::ListBox>().ok())
            .expect("actual mapped Live ListBox");
        let first = list.row_at_index(0).expect("real first Live row");
        focused_inside(&win, first.upcast_ref(), "Live Search -> first row");
        assert!(
            first.has_focus(),
            "R07 E01: first Live card row itself must own focus"
        );
        assert_eq!(
            win.imp().live_view.r07_native_channel_id(0).as_deref(),
            Some("r07-live-0")
        );
        for index in 1..5 {
            inject("Down", false);
            let next = list.row_at_index(index).expect("native mapped Live row");
            assert_eq!(
                win.imp().live_view.r07_native_channel_id(index),
                Some(format!("r07-live-{index}")),
                "R07 must focus the correctly identified real Live channel card"
            );
            focused_inside(&win, next.upcast_ref(), "Live real next row");
            assert!(
                next.has_focus(),
                "R07 E01: Down must focus the Live row, not an inner button"
            );
        }
        for index in (0..4).rev() {
            inject("Up", false);
            let previous = list.row_at_index(index).expect("real previous Live row");
            focused_inside(&win, previous.upcast_ref(), "Live real previous row");
            assert!(
                previous.has_focus(),
                "R07 E01: Up must focus the previous Live row directly"
            );
        }
    } else {
        assert!(
            targets
                .iter()
                .skip(1)
                .any(|target| TvWindow::is_within(&after_down, target)),
            "R07 Mediathek Search Down must reach a different mapped toolbar control"
        );
    }
    inject("Up", false);
    focused_inside(&win, search, "Down/Up must return to Search");

    inject("Right", false);
    focused_inside(&win, &targets[1], "Search Right -> Keyboard");
    inject("Left", false);
    focused_inside(&win, search, "Keyboard Left -> Search");
    win.destroy();
}

#[test]
#[ignore = "mapped GTK/XTest with isolated display and GSettings"]
fn r07_native_live_search_640_480() {
    run("live", 640, 480, false);
}
#[test]
#[ignore = "mapped GTK/XTest with isolated display and GSettings"]
fn r07_native_live_search_1280_800() {
    run("live", 1280, 800, false);
}
#[test]
#[ignore = "mapped GTK/XTest with isolated display and GSettings"]
fn r07_native_mediathek_search_640_480() {
    run("mediathek", 640, 480, false);
}
#[test]
#[ignore = "mapped GTK/XTest with isolated display and GSettings"]
fn r07_native_mediathek_search_1280_800() {
    run("mediathek", 1280, 800, false);
}
#[test]
#[ignore = "deliberately wrong focus owner must fail via XTest"]
fn r07_native_negative_live_search_stuck() {
    run("live", 640, 480, true);
}

// E02: use the real Live card model and a physical key to enter Play.
// The Play button is never activated: pressing OK *again* would start mpv.
fn run_live_play_portals(width: i32, height: i32, entry_key: &str, negative: bool) {
    adw::init().expect("native GTK display");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("real app register");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name("live");
    let live = &win.imp().live_view;
    live.show_toolbar_for_test();
    live.install_r07_native_channel_rows();
    settle();
    let page: &gtk::Widget = live.upcast_ref();
    let list = TvWindow::find_descendant(page, &|w| w.is::<gtk::ListBox>())
        .and_then(|w| w.downcast::<gtk::ListBox>().ok())
        .expect("real Live ListBox");
    let search = &live.toolbar_focus_targets()[0];
    assert!(search.grab_focus());
    settle();
    inject("Down", true);
    let mut current = 0;
    for index in [0, 2, 4] {
        while current < index {
            inject("Down", false);
            current += 1;
        }
        let row = list.row_at_index(index).expect("mapped real LiveCard");
        assert!(
            row.has_focus(),
            "E02 source row must own native focus before entry"
        );
        assert_eq!(
            live.r07_native_channel_id(index),
            Some(format!("r07-live-{index}")),
            "E02 wrong channel before entry"
        );
        inject(entry_key, false);
        let owner = win.focused_widget().expect("E02 actual window focus owner");
        assert!(
            owner.is_mapped() && owner.is_sensitive() && owner.is_visible(),
            "E02 Play owner must be mapped, sensitive, visible"
        );
        let play = owner
            .clone()
            .downcast::<gtk::Button>()
            .expect("E02 Play Button");
        assert_eq!(
            play.action_name().as_deref(),
            Some("card.play"),
            "E02 {entry_key} must focus primary Play, not a row or unrelated control"
        );
        assert_eq!(
            owner.ancestor(gtk::ListBoxRow::static_type()),
            Some(row.clone().upcast::<gtk::Widget>()),
            "E02 focus must belong to the current source-qualified card"
        );
        if negative {
            assert_eq!(
                live.r07_native_channel_id(index),
                Some("r07-live-wrong".to_owned()),
                "R07_E02_EXPECTED_NEGATIVE_ORACLE: Play cannot be attributed to another source"
            );
            unreachable!("E02 wrong-source identity oracle unexpectedly survived");
        }
        inject("Left", false);
        assert!(
            row.has_focus(),
            "E02 native Left must return to same LiveCard"
        );
        // Reset this disposable row's reveal state without starting playback;
        // subsequent movement must be a genuine injected Down key.
        live.r07_collapse_test_channel(index);
        settle();
        assert!(row.has_focus(), "E02 reset must preserve current row focus");
    }
    win.destroy();
}

#[test]
#[ignore = "mapped real Live cards/XTest: native OK Play entry at 640x480"]
fn r07_native_live_play_ok_640_480() {
    run_live_play_portals(640, 480, "Return", false);
}

#[test]
#[ignore = "mapped real Live cards/XTest: native OK Play entry at 1280x800"]
fn r07_native_live_play_ok_1280_800() {
    run_live_play_portals(1280, 800, "Return", false);
}

#[test]
#[ignore = "mapped real Live cards/XTest: native Right Play entry at 640x480"]
fn r07_native_live_play_right_640_480() {
    run_live_play_portals(640, 480, "Right", false);
}

#[test]
#[ignore = "mapped real Live cards/XTest: native Right Play entry at 1280x800"]
fn r07_native_live_play_right_1280_800() {
    run_live_play_portals(1280, 800, "Right", false);
}

#[test]
#[ignore = "E02 wrong source identity must be rejected after real Play entry"]
fn r07_native_live_play_negative_source_identity() {
    run_live_play_portals(640, 480, "Return", true);
}

// D01/D02/D07: combine the Live controls, first/middle/last real rows,
// expanded Play, real category switcher and Home in one physical journey.
// This deliberately tests the complete navigation connection rather than
// giving PASS credit for isolated toolbar-only or row-only fixtures.
fn run_live_complete_graph(width: i32, height: i32, wrong_row: bool) {
    adw::init().expect("native GTK X11 display");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered real application");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    win.imp().stack.set_visible_child_name("live");
    let live = &win.imp().live_view;
    live.show_toolbar_for_test();
    live.install_r07_native_channel_rows();
    settle();
    let list = TvWindow::find_descendant(live.upcast_ref(), &|w| w.is::<gtk::ListBox>())
        .and_then(|w| w.downcast::<gtk::ListBox>().ok())
        .expect("actual Live ListBox");
    let search = live.toolbar_focus_targets()[0].clone();
    let keyboard = live.toolbar_focus_targets()[1].clone();
    assert!(search.is_mapped() && search.grab_focus());
    settle();
    inject("Right", true);
    focused_inside(&win, &keyboard, "D01/D02 real keyboard right");
    inject("Left", false);
    focused_inside(&win, &search, "D01/D02 Search inverse");
    inject("Down", false);
    let first = list.row_at_index(0).expect("mapped first Live row");
    focused_inside(&win, first.upcast_ref(), "D01/D02 Search to first result");
    for index in 0..5 {
        if index > 0 {
            inject("Down", false);
        }
        let row = list.row_at_index(index).expect("complete five-card list");
        assert!(row.has_focus() && row.is_mapped());
        assert_eq!(
            live.r07_native_channel_id(index),
            Some(format!("r07-live-{index}"))
        );
        if index == 4 && wrong_row {
            assert_eq!(
                live.r07_native_channel_id(index).as_deref(),
                Some("r07-live-wrong"),
                "R07_D01_D02_EXPECTED_NEGATIVE_ORACLE: last Live row source identity",
            );
        }
    }
    let last = list.row_at_index(4).unwrap();
    // Physical OK enters Play but never starts mpv; Left returns to the same row.
    inject("Return", false);
    let play = win.focused_widget().expect("D01/D02 real focused Play");
    assert!(play.is_mapped() && TvWindow::is_within(&play, last.upcast_ref()));
    assert_eq!(
        play.downcast_ref::<gtk::Button>()
            .and_then(|button| button.action_name())
            .as_deref(),
        Some("card.play"),
    );
    inject("Left", false);
    assert!(last.has_focus(), "D01/D02 Play Left returns last Live row");
    live.r07_collapse_test_channel(4);
    settle();
    for index in (0..4).rev() {
        inject("Up", false);
        let row = list.row_at_index(index).unwrap();
        assert!(row.has_focus(), "D01/D02 reverse returns row {index}");
    }
    inject("Up", false);
    focused_inside(&win, &search, "D01/D02 first row Up to Search");
    inject("Up", false);
    let switcher = TvWindow::find_descendant(win.upcast_ref(), &|w| {
        (w.is::<adw::ViewSwitcher>() || w.is::<adw::ViewSwitcherBar>()) && w.is_mapped()
    })
    .expect("mapped real responsive switcher");
    let selected = win.focused_widget().expect("focused Live switcher");
    focused_inside(&win, &switcher, "D07 Live Search Up to switcher");
    assert!(selected.is_mapped());
    // An actual Home event from the focused switcher returns to Mediathek.
    // GTK may keep focus on the newly selected mapped switcher tab; the
    // existing D07 gate independently covers Down into Mediathek Search.
    inject("XF86HomePage", false);
    assert_eq!(
        win.imp().stack.visible_child_name().as_deref(),
        Some("mediathek"),
        "D01/D02 Home switches to Mediathek",
    );
    let home_owner = win.focused_widget().expect("D01/D02 Home focus owner");
    assert!(
        home_owner.is_mapped()
            && home_owner.is_sensitive()
            && (TvWindow::is_within(&home_owner, &switcher)
                || TvWindow::is_within(&home_owner, win.imp().mediathek_view.upcast_ref(),)),
        "D01/D02 Home focus must remain on a mapped Mediathek or switcher control",
    );
    win.destroy();
}

#[test]
#[ignore = "R07 physical full loaded Live Search, rows, Play, categories and Home at 640x480"]
fn r07_native_live_full_graph_640_480() {
    run_live_complete_graph(640, 480, false);
}

#[test]
#[ignore = "R07 physical full loaded Live Search, rows, Play, categories and Home at 1280x800"]
fn r07_native_live_full_graph_1280_800() {
    run_live_complete_graph(1280, 800, false);
}

#[test]
#[ignore = "R07 deliberate wrong last real Live-card identity"]
fn r07_native_live_full_graph_negative_identity() {
    run_live_complete_graph(640, 480, true);
}

// Test-only native cross-component evidence; retained as one R07 module.
include!("r07_mediathek_batch_tests.rs");

include!("r07_painted_focus_tests.rs");

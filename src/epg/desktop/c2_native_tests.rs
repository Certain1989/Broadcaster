// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Opt-in native C2 GUI probes: real toolbar launch, disposable SQLite and
//! physical XTest keys. Each named case runs in its own process.
use super::*;
use crate::{
    application::TvApplication,
    epg::sources::GuideSourceRegistry,
    streams::{service::StreamsService, storage::Storage, view::TvStreamsView},
};
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

fn spin() {
    for _ in 0..20 {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        thread::sleep(Duration::from_millis(16));
    }
}

fn inject(key: &str) {
    // Isolated CI X11 backend: genuine key events, never emit::clicked().
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
    let mode = if key == "Return" { "initial" } else { "keep" };
    let output = std::process::Command::new("python3")
        .args(["-c", SCRIPT, key, mode])
        .output()
        .expect("real XTest injection");
    assert!(
        output.status.success(),
        "XTest {key}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    spin();
}

fn until(name: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        spin();
        if ready() {
            return;
        }
    }
    panic!("C2 native deadline: {name}");
}
fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    fn visit(w: &gtk::Widget, result: &mut Vec<gtk::Widget>) {
        result.push(w.clone());
        let mut next = w.first_child();
        while let Some(c) = next {
            visit(&c, result);
            next = c.next_sibling();
        }
    }
    let mut result = Vec::new();
    visit(root, &mut result);
    result
}
fn find_view(widget: &gtk::Widget) -> Option<TvStreamsView> {
    descendants(widget)
        .into_iter()
        .find_map(|w| w.downcast::<TvStreamsView>().ok())
}
fn find_dialog(widget: &gtk::Widget, css: &str) -> Option<adw::Dialog> {
    descendants(widget)
        .into_iter()
        .filter_map(|w| w.downcast::<adw::Dialog>().ok())
        .find(|d| d.has_css_class(css) && d.is_mapped())
}
fn named_button(widget: &gtk::Widget, name: &str) -> Option<gtk::Button> {
    descendants(widget)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.label().as_deref() == Some(name))
}
fn row(dialog: &adw::Dialog, name: &str) -> Option<gtk::Box> {
    descendants(dialog.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .find(|label| label.text().starts_with(&format!("{name} —")))
        .and_then(|label| label.parent())
        .and_then(|w| w.downcast::<gtk::Box>().ok())
}
fn first_entry(dialog: &adw::Dialog) -> gtk::Entry {
    descendants(dialog.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Entry>().ok())
        .expect("actual editor entry")
}
fn assert_remote_focus(win: &crate::window::TvWindow, button: &gtk::Button, route: &str) {
    assert!(button.is_mapped(), "{route}: mapped destination");
    assert!(button.has_focus(), "{route}: expected native focus owner");
    let focused = gtk::prelude::GtkWindowExt::focus(win);
    assert!(
        focused.is_some_and(|widget| widget == button.clone().upcast::<gtk::Widget>()),
        "{route}: GtkWindow focus owner differs from target"
    );
}

fn guide_action(dialog: &adw::Dialog, name: &str, label: &str) -> gtk::Button {
    named_button(
        row(dialog, name).expect("live guide UUID row").upcast_ref(),
        label,
    )
    .expect("named semantic action")
}

fn source_enabled(storage: &Storage, id: &str) -> bool {
    let conn = storage.connect().unwrap();
    conn.query_row("SELECT enabled FROM epg_sources WHERE id=?1", [id], |r| {
        r.get::<_, bool>(0)
    })
    .expect("reopened SQLite enabled bit")
}

struct Fixture {
    home: PathBuf,
    storage: Storage,
    ids: Vec<String>,
}
impl Fixture {
    fn new(count: usize) -> Self {
        let home = std::env::temp_dir().join(format!("broadcaster-c2-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&home).unwrap();
        std::env::set_var("XDG_DATA_HOME", &home);
        let storage = Storage::new();
        storage.initialize().expect("private initialized SQLite");
        let registry = GuideSourceRegistry::new(storage.clone());
        let ids = (0..count).map(|i| {
            let file = home.join(format!("fabricated-{i}.xml"));
            fs::write(&file, "<?xml version=\"1.0\"?><tv><channel id=\"fixture\"><display-name>Fixture</display-name></channel></tv>").unwrap();
            let origin = SourceOrigin::selected_local_file(gio::File::for_path(&file).uri().to_string()).unwrap();
            registry.add_with_schedule(&format!("Guide {i}"), &origin, false)
                .expect("consented local guide source").id
        }).collect();
        Self { home, storage, ids }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn run(count: usize, width: i32, height: i32, negative: bool) {
    let fixture = Fixture::new(count);
    adw::init().expect("native GTK");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    let stream_service = glib::MainContext::default()
        .block_on(StreamsService::with_storage(fixture.storage.clone()))
        .unwrap();
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>).unwrap();
    let win = crate::window::TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    spin();
    let stack = descendants(win.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<adw::ViewStack>().ok())
        .expect("real app stack");
    stack.set_visible_child_name("streams");
    spin();
    let view = find_view(win.upcast_ref()).expect("real My Channels page");
    view.c1_install_test_service(stream_service);
    assert!(
        view.c1_test_manage_button().grab_focus(),
        "real Manage Sources toolbar"
    );
    inject("Return");
    until("source manager visible", || {
        find_dialog(win.upcast_ref(), "broadcaster-source-manager").is_some()
    });
    let manager = find_dialog(win.upcast_ref(), "broadcaster-source-manager").unwrap();
    let guides =
        named_button(manager.upcast_ref(), "Programme Guides").expect("actual nested launcher");
    assert!(guides.grab_focus());
    inject("Return");
    until("programme guide manager visible", || {
        find_dialog(win.upcast_ref(), "broadcaster-guide-manager").is_some()
    });
    let mut guide = find_dialog(win.upcast_ref(), "broadcaster-guide-manager").unwrap();
    until("source-backed guide rows loaded", || {
        if count == 0 {
            return descendants(guide.upcast_ref()).iter().any(|w| {
                w.clone()
                    .downcast::<gtk::Label>()
                    .ok()
                    .is_some_and(|l| l.text() == "No programme guides configured")
            });
        }
        (0..count).all(|i| row(&guide, &format!("Guide {i}")).is_some())
    });
    assert!(
        guide.is_mapped(),
        "actual modal at the requested display geometry"
    );
    assert!(
        named_button(guide.upcast_ref(), "Add URL").is_some(),
        "real Add URL action"
    );
    if count == 0 {
        let add = named_button(guide.upcast_ref(), "Add URL").unwrap();
        assert!(add.grab_focus());
        inject("Return");
        until("add modal", || {
            named_button(win.upcast_ref(), "Save Guide").is_some()
        });
        let save = named_button(win.upcast_ref(), "Save Guide").unwrap();
        assert!(save.grab_focus());
        inject("Return"); // invalid blank URL: must never attempt a fetch
        assert!(
            save.is_mapped(),
            "invalid URL stays in the editor without a network call"
        );
        inject("XF86Back");
        until("add dismissed only", || !save.is_mapped());
        assert!(
            guide.is_mapped() && manager.is_mapped(),
            "nested Back must not touch parent or page"
        );
    } else {
        // Only the initial row focus is set directly. Every action-to-action
        // transition below uses actual XTest arrows and asserts GtkWindow focus.
        let first_default = guide_action(&guide, "Guide 0", "Use as Shared Default");
        assert!(
            first_default.grab_focus(),
            "enter first source via real widget"
        );
        inject("Right");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 0", "Refresh"),
            "first default -> Refresh",
        );
        inject("Down");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 0", "Remove"),
            "first Refresh -> Remove",
        );
        inject("Down");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 1", "Use as Shared Default"),
            "first Remove -> second UUID",
        );
        inject("Up");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 0", "Remove"),
            "second -> first reciprocal",
        );
        inject("Down");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 1", "Use as Shared Default"),
            "first -> second again",
        );
        inject("Right");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 1", "Refresh"),
            "second default -> Refresh",
        );
        inject("Down");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 1", "Remove"),
            "second Refresh -> Remove",
        );
        inject("Down");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 2", "Use as Shared Default"),
            "second Remove -> third UUID",
        );
        inject("Up");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 1", "Remove"),
            "third -> second reciprocal",
        );
        inject("Down");
        inject("Right");
        inject("Down");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 2", "Remove"),
            "last guide Remove",
        );
        inject("Down");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 2", "Remove"),
            "last row Down boundary stays within dialog",
        );
        inject("Up");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 2", "Refresh"),
            "last Remove -> Refresh reciprocal",
        );

        let source_id = fixture.ids[1].clone();
        let row1 = row(&guide, "Guide 1").unwrap();
        let edit = named_button(row1.upcast_ref(), "Edit").unwrap();
        assert!(edit.grab_focus(), "second guide action");
        inject("Return");
        until("settings modal", || {
            named_button(win.upcast_ref(), "Apply").is_some()
        });
        let apply = named_button(win.upcast_ref(), "Apply").unwrap();
        assert!(
            apply.is_mapped() && apply.grab_focus(),
            "Apply must be mapped at 640x480"
        );
        let editor = apply
            .ancestor(adw::Dialog::static_type())
            .unwrap()
            .downcast::<adw::Dialog>()
            .unwrap();
        let name = first_entry(&editor);
        let launcher = descendants(editor.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.icon_name().as_deref() == Some("input-keyboard-symbolic"))
            .expect("native OSK launcher");
        assert!(launcher.grab_focus());
        inject("Return");
        until("nested OSK", || {
            find_dialog(win.upcast_ref(), "osk-production").is_some()
        });
        let osk = find_dialog(win.upcast_ref(), "osk-production").unwrap();
        inject("XF86HomePage");
        until("Home closes only OSK", || !osk.is_mapped());
        assert!(
            editor.is_mapped() && guide.is_mapped(),
            "Home must not fire behind the OSK"
        );
        assert!(launcher.grab_focus());
        inject("Return");
        until("reopened OSK", || {
            find_dialog(win.upcast_ref(), "osk-production").is_some()
        });
        let osk = find_dialog(win.upcast_ref(), "osk-production").unwrap();
        let draft = first_entry(&osk);
        draft.set_text("Must not save");
        let cancel = named_button(osk.upcast_ref(), "Cancel").expect("OSK Cancel");
        assert!(cancel.grab_focus());
        inject("Return");
        until("Cancel dismisses only OSK", || !osk.is_mapped());
        assert_eq!(
            name.text().as_str(),
            "Guide 1",
            "cancelled OSK draft must not mutate editor"
        );
        assert!(
            editor.is_mapped() && guide.is_mapped(),
            "Cancel cannot act on background page"
        );
        assert!(launcher.grab_focus());
        inject("Return");
        until("OSK reopens after cancel", || {
            find_dialog(win.upcast_ref(), "osk-production").is_some()
        });
        let osk = find_dialog(win.upcast_ref(), "osk-production").unwrap();
        let draft = first_entry(&osk);
        draft.set_text("Updated Guide 1");
        let done = named_button(osk.upcast_ref(), "Done").expect("OSK Done");
        assert!(done.grab_focus());
        inject("Return");
        until("Done closes topmost OSK", || !osk.is_mapped());
        assert_eq!(
            name.text().as_str(),
            "Updated Guide 1",
            "OSK Done updated only origin editor"
        );
        let enabled = descendants(editor.upcast_ref())
            .into_iter()
            .find_map(|w| w.downcast::<gtk::CheckButton>().ok())
            .expect("Enabled checkbox");
        assert!(enabled.grab_focus());
        inject("space"); // native toggle
        assert!(!enabled.is_active(), "native Enabled checkbox toggle");
        let hours = descendants(editor.upcast_ref())
            .into_iter()
            .find_map(|w| w.downcast::<gtk::SpinButton>().ok())
            .expect("interval");
        assert!(hours.grab_focus());
        let before_hours = hours.value_as_int();
        inject("Right");
        let decrease =
            named_button(editor.upcast_ref(), "− 1 hour").expect("accessible decrease action");
        assert!(decrease.has_focus(), "Right enters interval adjustment");
        inject("Right");
        let increase =
            named_button(editor.upcast_ref(), "+ 1 hour").expect("accessible increase action");
        assert!(increase.has_focus(), "Right reaches interval increase");
        inject("Return");
        assert_eq!(
            hours.value_as_int(),
            before_hours + 1,
            "OK adjusts interval via D-pad"
        );
        assert!(hours.grab_focus());
        inject("Down"); // reachable Apply, not scroller
        assert!(apply.has_focus(), "D-pad down from interval reaches Apply");
        inject("Return");
        until("committed and repainted", || {
            !editor.is_mapped() && row(&guide, "Updated Guide 1").is_some()
        });
        let persisted = GuideSourceRegistry::new(fixture.storage.clone())
            .get(&source_id)
            .unwrap();
        assert!(!persisted.enabled && persisted.name == "Updated Guide 1");
        assert_eq!(
            persisted.refresh_interval_seconds,
            i64::from(before_hours + 1) * 3600,
            "explicit D-pad interval change persisted with same source UUID"
        );
        assert!(
            !source_enabled(&fixture.storage, &source_id),
            "persisted after reopened SQLite"
        );
        let new_row = row(&guide, "Updated Guide 1").unwrap();
        let edit = named_button(new_row.upcast_ref(), "Edit").unwrap();
        assert!(
            !row1.is_mapped(),
            "repaint really destroyed the previous row"
        );
        assert_remote_focus(
            &win,
            &edit,
            "same UUID semantic Edit restored by async repaint",
        );
        inject("Up");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Updated Guide 1", "Use as Shared Default"),
            "disabled Edit -> Shared Default",
        );
        inject("Right");
        if negative {
            assert_remote_focus(
                &win,
                &guide_action(&guide, "Updated Guide 1", "Refresh"),
                "C2_EXPECTED_NEGATIVE_ORACLE: disabled Refresh would incorrectly own focus",
            );
            unreachable!("incorrect disabled-target oracle unexpectedly passed");
        }
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Updated Guide 1", "Edit"),
            "disabled Default Right skips Refresh to Edit",
        );
        inject("Left");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Updated Guide 1", "Use as Shared Default"),
            "disabled Edit Left reverses to Default",
        );
        inject("Down");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Updated Guide 1", "Edit"),
            "disabled Default Down -> Edit",
        );
        inject("Right");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Updated Guide 1", "Remove"),
            "disabled Edit -> Remove",
        );
        inject("Up");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Updated Guide 1", "Use as Shared Default"),
            "disabled Remove Up skips Refresh",
        );
        // Rename reorders guides alphabetically: Updated Guide 1 is now the
        // last UUID, after Guide 2. Traverse the live ordering, not old row indices.
        inject("Up");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 2", "Remove"),
            "disabled last Default -> prior sorted UUID Remove",
        );
        inject("Down");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Updated Guide 1", "Use as Shared Default"),
            "prior sorted Remove -> disabled last Default, not Refresh",
        );
        inject("Up");
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 2", "Remove"),
            "disabled last Default -> prior Remove reciprocal",
        );
        assert!(
            !named_button(new_row.upcast_ref(), "Refresh")
                .unwrap()
                .is_sensitive(),
            "disabled guide must skip Refresh"
        );
        // Shared default is an explicit confirmation and must be reversible.
        let original = row(&guide, "Guide 0").expect("surviving first guide");
        let set_default = named_button(original.upcast_ref(), "Use as Shared Default").unwrap();
        assert!(set_default.grab_focus());
        inject("Return");
        until("shared-default confirmation", || {
            named_button(win.upcast_ref(), "Confirm Shared Default").is_some()
        });
        let confirm = named_button(win.upcast_ref(), "Confirm Shared Default").unwrap();
        assert!(confirm.grab_focus());
        inject("Return");
        until("shared guide persisted", || {
            let conn = fixture.storage.connect().unwrap();
            conn.query_row(
                "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap()
                == Some(fixture.ids[0].clone())
        });
        // AG01: set must survive closing this *real GTK manager*, then
        // re-opening a newly constructed manager backed by the same SQLite.
        let close_for_reload = named_button(guide.upcast_ref(), "Close").unwrap();
        assert!(close_for_reload.grab_focus());
        inject("XF86Back");
        until("first-default manager fully closed", || !guide.is_mapped());
        assert!(
            manager.is_mapped(),
            "nested close must retain parent manager"
        );
        assert!(guides.grab_focus());
        inject("Return");
        until("first-default new guide manager mapped", || {
            find_dialog(win.upcast_ref(), "broadcaster-guide-manager").is_some()
        });
        guide = find_dialog(win.upcast_ref(), "broadcaster-guide-manager").unwrap();
        until("first-default recovered in reloaded UI", || {
            row(&guide, "Guide 0").is_some()
                && named_button(guide.upcast_ref(), "Clear Shared Default")
                    .is_some_and(|button| button.is_mapped())
        });
        assert_eq!(
            fixture
                .storage
                .connect()
                .unwrap()
                .query_row(
                    "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
                    [],
                    |row| row.get::<_, Option<String>>(0),
                )
                .unwrap(),
            Some(fixture.ids[0].clone()),
            "first shared default survives a fresh GTK manager and SQLite reopen"
        );
        let clear = named_button(guide.upcast_ref(), "Clear Shared Default").unwrap();
        assert!(clear.grab_focus());
        inject("Return");
        until("clear-default confirmation", || {
            descendants(win.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<adw::Dialog>().ok())
                .any(|d| {
                    d.is_mapped()
                        && named_button(d.upcast_ref(), "Clear Shared Default").is_some()
                        && named_button(d.upcast_ref(), "Cancel").is_some()
                })
        });
        let confirmation = descendants(win.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<adw::Dialog>().ok())
            .find(|d| {
                d.is_mapped()
                    && named_button(d.upcast_ref(), "Cancel").is_some()
                    && named_button(d.upcast_ref(), "Clear Shared Default").is_some()
            })
            .expect("real clear-default confirmation");
        let confirm_clear =
            named_button(confirmation.upcast_ref(), "Clear Shared Default").unwrap();
        assert!(confirm_clear.grab_focus());
        inject("Return");
        until("shared default cleared", || {
            let conn = fixture.storage.connect().unwrap();
            conn.query_row(
                "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap()
            .is_none()
        });
        // R07 AG01: repeat real default->confirmation->SQLite persistence
        // on the *last* guide, not only the first. Both edges must support
        // Set/Clear with no stale default after guide-list rebuilding.
        let last = guide_action(&guide, "Guide 2", "Use as Shared Default");
        assert!(last.grab_focus(), "last real guide default action");
        inject("Return");
        until("last guide default confirmation", || {
            named_button(win.upcast_ref(), "Confirm Shared Default").is_some()
        });
        let last_confirm = named_button(win.upcast_ref(), "Confirm Shared Default").unwrap();
        assert!(last_confirm.grab_focus());
        inject("Return");
        until(
            "last guide persisted in independently opened SQLite connection",
            || {
                let conn = fixture.storage.connect().unwrap();
                conn.query_row(
                    "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
                    [],
                    |row| row.get::<_, Option<String>>(0),
                )
                .unwrap()
                    == Some(fixture.ids[2].clone())
            },
        );
        // The last-guide default gets the same true-close/reopen round-trip.
        let close_last = named_button(guide.upcast_ref(), "Close").unwrap();
        assert!(close_last.grab_focus());
        inject("XF86Back");
        until("last-default manager fully closed", || !guide.is_mapped());
        assert!(manager.is_mapped());
        assert!(guides.grab_focus());
        inject("Return");
        until("last-default new guide manager mapped", || {
            find_dialog(win.upcast_ref(), "broadcaster-guide-manager").is_some()
        });
        guide = find_dialog(win.upcast_ref(), "broadcaster-guide-manager").unwrap();
        until("last-default recovered in reloaded UI", || {
            row(&guide, "Guide 2").is_some()
                && named_button(guide.upcast_ref(), "Clear Shared Default")
                    .is_some_and(|button| button.is_mapped())
        });
        assert_eq!(
            fixture
                .storage
                .connect()
                .unwrap()
                .query_row(
                    "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
                    [],
                    |row| row.get::<_, Option<String>>(0),
                )
                .unwrap(),
            Some(fixture.ids[2].clone()),
            "last shared default survives a fresh GTK manager and SQLite reopen"
        );
        let clear_last = named_button(guide.upcast_ref(), "Clear Shared Default")
            .expect("last guide clear action");
        assert!(clear_last.grab_focus());
        inject("Return");
        until("last guide clear confirmation", || {
            descendants(win.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<adw::Dialog>().ok())
                .any(|d| {
                    d.is_mapped()
                        && named_button(d.upcast_ref(), "Clear Shared Default").is_some()
                        && named_button(d.upcast_ref(), "Cancel").is_some()
                })
        });
        let clear_dialog = descendants(win.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<adw::Dialog>().ok())
            .find(|d| {
                d.is_mapped()
                    && named_button(d.upcast_ref(), "Clear Shared Default").is_some()
                    && named_button(d.upcast_ref(), "Cancel").is_some()
            })
            .expect("real last guide clear confirmation");
        let confirm_last_clear =
            named_button(clear_dialog.upcast_ref(), "Clear Shared Default").unwrap();
        assert!(confirm_last_clear.grab_focus());
        inject("Return");
        until("last guide cleared in reopened SQLite connection", || {
            let conn = fixture.storage.connect().unwrap();
            conn.query_row(
                "SELECT shared_guide_source_id FROM epg_settings WHERE singleton=1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap()
            .is_none()
        });
        assert!(
            guide.is_mapped() && manager.is_mapped(),
            "default inverse stays in guide modal"
        );
        // Remove is protected by Keep/Remove confirmation; Keep is not destructive.
        let third = row(&guide, "Guide 2").unwrap();
        let remove = named_button(third.upcast_ref(), "Remove").unwrap();
        assert!(remove.grab_focus());
        inject("Return");
        until("remove confirmation", || {
            named_button(win.upcast_ref(), "Keep Guide").is_some()
        });
        let keep = named_button(win.upcast_ref(), "Keep Guide").unwrap();
        assert!(keep.grab_focus());
        inject("Return");
        until("Keep closes confirmation", || !keep.is_mapped());
        assert!(
            GuideSourceRegistry::new(fixture.storage.clone())
                .get(&fixture.ids[2])
                .is_ok(),
            "Keep must leave saved guide untouched"
        );
        let third = row(&guide, "Guide 2").unwrap();
        assert!(named_button(third.upcast_ref(), "Remove")
            .unwrap()
            .grab_focus());
        inject("Return");
        until("destructive confirmation", || {
            named_button(win.upcast_ref(), "Remove Permanently").is_some()
        });
        let permanently = named_button(win.upcast_ref(), "Remove Permanently").unwrap();
        assert!(permanently.grab_focus());
        inject("Return");
        until("third guide removed from SQLite and UI", || {
            row(&guide, "Guide 2").is_none()
                && GuideSourceRegistry::new(fixture.storage.clone())
                    .get(&fixture.ids[2])
                    .is_err()
        });
        assert!(
            guide.is_mapped() && manager.is_mapped(),
            "remove confirms inside guide manager, not behind it"
        );
        until("removed-row focus fallback", || {
            guide_action(&guide, "Guide 0", "Use as Shared Default").has_focus()
        });
        assert_remote_focus(
            &win,
            &guide_action(&guide, "Guide 0", "Use as Shared Default"),
            "removed guide UUID falls back to surviving source",
        );
        let first_default = guide_action(&guide, "Guide 0", "Use as Shared Default");
        inject("Left");
        assert_remote_focus(
            &win,
            &first_default,
            "first-row Left boundary stays in manager",
        );
        // Back at guide level should close only the active guide manager.
    }
    assert!(guide.is_mapped() && manager.is_mapped());
    let manager_list = if count > 0 {
        row(&guide, "Guide 0")
            .expect("guide row")
            .parent()
            .and_then(|w| w.downcast::<gtk::Box>().ok())
            .expect("guide list")
    } else {
        descendants(guide.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .find(|label| label.text() == "No programme guides configured")
            .and_then(|w| w.parent())
            .and_then(|w| w.downcast::<gtk::Box>().ok())
            .expect("empty guide list")
    };
    let weak_list = manager_list.downgrade();
    assert!(named_button(guide.upcast_ref(), "Close")
        .unwrap()
        .grab_focus());
    inject("XF86Back");
    until("guide manager dismissed", || !guide.is_mapped());
    assert!(
        manager.is_mapped(),
        "source manager must remain under guide modal"
    );
    drop(manager_list);
    drop(guide);
    until(
        "closed guide list disposed, no callback reference cycle",
        || weak_list.upgrade().is_none(),
    );
    // A new manager must be an independent fresh tree, not the prior retained list.
    assert!(guides.grab_focus());
    inject("Return");
    until("fresh guide manager opens", || {
        find_dialog(win.upcast_ref(), "broadcaster-guide-manager").is_some()
    });
    let reopened = find_dialog(win.upcast_ref(), "broadcaster-guide-manager").unwrap();
    assert!(reopened.is_mapped(), "reopened manager mapped");
    if count > 0 {
        until("reopened SQLite guide rows", || {
            row(&reopened, "Guide 0").is_some()
        });
        assert!(guide_action(&reopened, "Guide 0", "Edit").is_mapped());
    }
    assert!(named_button(reopened.upcast_ref(), "Close")
        .unwrap()
        .grab_focus());
    inject("XF86Back");
    until(
        "reopened manager closes without affecting source manager",
        || !reopened.is_mapped(),
    );
    assert!(manager.is_mapped());
    drop(reopened);
    win.destroy();
}

#[test]
#[ignore = "native GTK/XTest and isolated SQLite"]
fn c2_native_0_640_480() {
    run(0, 640, 480, false);
}
#[test]
#[ignore = "native GTK/XTest and isolated SQLite"]
fn c2_native_3_640_480() {
    run(3, 640, 480, false);
}
#[test]
#[ignore = "native GTK/XTest and isolated SQLite"]
fn c2_native_0_1280_800() {
    run(0, 1280, 800, false);
}
#[test]
#[ignore = "native GTK/XTest and isolated SQLite"]
fn c2_native_3_1280_800() {
    run(3, 1280, 800, false);
}
#[test]
#[ignore = "expected-RED negative focus oracle"]
fn c2_native_negative_oracle() {
    run(3, 640, 480, true);
}

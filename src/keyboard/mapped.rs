// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Mapped GTK/Xvfb regression probes. Intentionally ignored by ordinary,
//! potentially headless cargo test; Meson runs these on a real X11 display.

use super::*;

fn spin() {
    let context = glib::MainContext::default();
    // GTK maps dialog descendants on a frame tick, not necessarily on the
    // same idle turn in which present() was called.
    for _ in 0..8 {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
}

fn host() -> (adw::Window, gtk::Box) {
    adw::init().expect("mapped fixture needs GTK/adw");
    // Match app startup: regression geometry must include packaged CSS.
    let resources = adw::gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
        .expect("bundled resources");
    adw::gio::resources_register(&resources);
    let provider = gtk::CssProvider::new();
    provider.load_from_resource("/io/github/Certain1989/Broadcaster/broadcaster.css");
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("mapped Xvfb display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let win = adw::Window::new();
    win.set_default_size(960, 720);
    let container = gtk::Box::new(gtk::Orientation::Vertical, 8);
    win.set_content(Some(&container));
    win.present();
    spin();
    assert!(win.is_mapped());
    (win, container)
}

fn find_button(widget: &gtk::Widget, text: &str) -> Option<gtk::Button> {
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        if button.label().as_deref() == Some(text) {
            return Some(button.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(item) = child {
        if let Some(button) = find_button(&item, text) {
            return Some(button);
        }
        child = item.next_sibling();
    }
    None
}

fn find_entry(widget: &gtk::Widget) -> Option<gtk::Entry> {
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        return Some(entry.clone());
    }
    let mut child = widget.first_child();
    while let Some(item) = child {
        if let Some(entry) = find_entry(&item) {
            return Some(entry);
        }
        child = item.next_sibling();
    }
    None
}

fn click(dialog: &adw::Dialog, name: &str) {
    let button = find_button(dialog.upcast_ref(), name).expect("mapped keyboard key");
    button.grab_focus();
    spin();
    assert!(button.is_mapped(), "keyboard key {name} did not map");
    button.emit_by_name::<()>("clicked", &[]);
    spin();
}

fn keyboard(win: &adw::Window, entry: &impl IsA<gtk::Widget>) -> adw::Dialog {
    // This helper deliberately does not open the editor: each test calls
    // the same production typed API on the right GtkEditable type.
    let _ = entry;
    spin();
    let dlg = win.visible_dialog().expect("mapped OSK dialog");
    assert!(dlg.is_mapped());
    dlg
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_input_transaction_and_cancel() {
    let (win, body) = host();
    let entry = gtk::Entry::new();
    entry.set_text("original");
    body.append(&entry);
    spin();
    let changes = Rc::new(Cell::new(0));
    entry.connect_changed({
        let changes = changes.clone();
        move |_| changes.set(changes.get() + 1)
    });
    show_with_options(&entry, KeyboardOptions::default());
    let dlg = keyboard(&win, &entry);
    let draft = find_entry(dlg.upcast_ref()).expect("native independent draft entry");
    draft.set_text("ändered");
    draft.select_region(1, 3);
    assert_eq!(entry.text(), "original");
    assert_eq!(changes.get(), 0);
    click(&dlg, "Cancel");
    assert_eq!(entry.text(), "original");
    assert_eq!(changes.get(), 0);
    show_with_options(&entry, KeyboardOptions::default());
    let dlg = keyboard(&win, &entry);
    let draft = find_entry(dlg.upcast_ref()).expect("native draft");
    draft.set_text("new");
    click(&dlg, "Done");
    assert_eq!(entry.text(), "new");
    assert_eq!(changes.get(), 1, "one original changed signal on Done");
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_lifecycle_guards() {
    let (win, body) = host();
    let entry = gtk::Entry::new();
    entry.set_text("before");
    body.append(&entry);
    spin();
    show(&entry);
    let dlg = keyboard(&win, &entry);
    entry.set_sensitive(false);
    click(&dlg, "Done");
    assert_eq!(entry.text(), "before", "insensitive origin cannot commit");
    entry.set_sensitive(true);

    show(&entry);
    let dlg = keyboard(&win, &entry);
    entry.set_text("external reset");
    click(&dlg, "Done");
    assert_eq!(
        entry.text(),
        "external reset",
        "changed origin invalidates draft"
    );

    show(&entry);
    let dlg = keyboard(&win, &entry);
    entry.set_visible(false);
    if dlg.is_mapped() {
        click(&dlg, "Done");
    }
    assert_eq!(
        entry.text(),
        "external reset",
        "unmapped origin cannot commit"
    );
    entry.set_visible(true);
    spin();

    show(&entry);
    let dlg = keyboard(&win, &entry);
    body.remove(&entry);
    if dlg.is_mapped() {
        click(&dlg, "Done");
    }
    assert_eq!(
        entry.text(),
        "external reset",
        "removed origin cannot commit"
    );
    body.append(&entry);
    spin();

    show(&entry);
    let dlg = keyboard(&win, &entry);
    let other = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&other);
    body.remove(&entry);
    other.append(&entry);
    spin();
    if dlg.is_mapped() {
        click(&dlg, "Done");
    }
    assert_eq!(
        entry.text(),
        "external reset",
        "reparented origin cannot commit"
    );
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_close_releases_dialog_and_draft() {
    let (win, body) = host();
    let entry = gtk::Entry::new();
    body.append(&entry);
    spin();
    show(&entry);
    let dlg = keyboard(&win, &entry);
    let weak = dlg.downgrade();
    click(&dlg, "Cancel");
    drop(dlg);
    // Allow Adw's close animation and the focus-return idle source to finish.
    for _ in 0..80 {
        spin();
        if weak.upgrade().is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    assert!(
        weak.upgrade().is_none(),
        "no strong dialog/controller/widget cycle"
    );
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_preferences_expander_and_popover() {
    let (win, body) = host();
    let prefs = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let row = adw::EntryRow::new();
    row.set_title("Synthetic argument");
    group.add(&row);
    page.add(&group);
    prefs.add(&page);
    prefs.present(Some(&win));
    spin();
    assert!(prefs.is_mapped());
    show_with_options(&row, KeyboardOptions::default());
    let dlg = keyboard(&win, &row);
    click(&dlg, "Cancel");
    assert!(prefs.is_mapped(), "nested parent remains open");
    prefs.close();
    spin();

    let parent = adw::Dialog::new();
    let expander = gtk::Expander::new(Some("Advanced"));
    expander.set_expanded(true);
    let origin = gtk::Entry::new();
    expander.set_child(Some(&origin));
    parent.set_child(Some(&expander));
    parent.present(Some(&win));
    spin();
    assert!(origin.is_mapped());
    show_with_options(&origin, KeyboardOptions::default());
    let dlg = keyboard(&win, &origin);
    click(&dlg, "Cancel");
    assert!(parent.is_mapped(), "expanded source editor remains");
    parent.close();
    spin();

    let trigger = gtk::Button::with_label("Synthetic selector");
    body.append(&trigger);
    let popover = gtk::Popover::new();
    let search = gtk::Entry::new();
    popover.set_child(Some(&search));
    popover.set_parent(&trigger);
    popover.popup();
    spin();
    assert!(search.is_mapped(), "genuine GTK popup content mapped");
    show_with_options(&search, KeyboardOptions::default());
    let dlg = keyboard(&win, &search);
    click(&dlg, "Cancel");
    assert_eq!(search.text(), "", "popover origin unchanged");
    popover.popdown();
    popover.unparent();
    win.destroy();
}

#[test]
#[ignore = "requires mapped GTK and disposable GSettings"]
fn osk_mapped_gtk_entry_row_immediate_save_once() {
    let (win, _) = host();
    let prefs = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let row = adw::EntryRow::new();
    row.set_title("Synthetic mpv argument");
    row.set_text("--before");
    group.add(&row);
    page.add(&group);
    prefs.add(&page);
    prefs.present(Some(&win));
    spin();
    assert!(row.is_mapped());

    let settings = crate::settings::TvSettings::get();
    settings
        .set_strv("mpv-arguments", ["--before"])
        .expect("synthetic mpv key");
    let saves = Rc::new(Cell::new(0u32));
    row.connect_changed({
        let settings = settings.clone();
        let saves = saves.clone();
        move |row| {
            saves.set(saves.get() + 1);
            settings
                .set_strv("mpv-arguments", [row.text().to_string()])
                .expect("synthetic save");
        }
    });

    show_with_options(&row, KeyboardOptions::default());
    let dialog = keyboard(&win, &row);
    let draft = find_entry(dialog.upcast_ref()).expect("nested editable draft");
    draft.set_text("--canceled");
    click(&dialog, "Cancel");
    assert_eq!(row.text(), "--before");
    assert_eq!(saves.get(), 0);
    assert_eq!(settings.strv("mpv-arguments")[0].as_str(), "--before");

    show_with_options(&row, KeyboardOptions::default());
    let dialog = keyboard(&win, &row);
    let draft = find_entry(dialog.upcast_ref()).expect("nested editable draft");
    draft.set_text("--committed");
    click(&dialog, "Done");
    assert_eq!(row.text(), "--committed");
    assert_eq!(saves.get(), 1, "one normal row.changed save callback");
    assert_eq!(settings.strv("mpv-arguments")[0].as_str(), "--committed");

    prefs.close();
    win.destroy();
}

#[test]
#[ignore = "requires mapped GTK and disposable GSettings"]
fn osk_mapped_gtk_settings_toggle_and_search() {
    let (win, body) = host();
    let search = gtk::SearchEntry::new();
    let button = gtk::Button::with_label("Synthetic keyboard");
    body.append(&search);
    body.append(&button);
    spin();

    let settings = crate::settings::TvSettings::get();
    assert!(settings.osk_enabled(), "new profiles must default ON");
    bind_launcher(&button, &search, KeyboardOptions::default());
    spin();
    assert!(button.is_mapped(), "enabled launcher is D-pad focusable");

    settings
        .set_boolean("osk-enabled", false)
        .expect("test must switch OSK off");
    spin();
    assert!(
        !button.is_visible(),
        "launcher hides immediately without restart"
    );
    assert!(!crate::settings::TvSettings::new(crate::config::BASE_APP_ID).osk_enabled());

    settings
        .set_boolean("osk-enabled", true)
        .expect("test must switch OSK back on");
    spin();
    assert!(button.is_mapped(), "launcher reappears immediately");
    button.emit_by_name::<()>("clicked", &[]);
    let dialog = keyboard(&win, &search);
    click(&dialog, "Cancel");
    assert_eq!(search.text(), "", "search stays unchanged after Cancel");
    win.destroy();
}

#[test]
#[ignore = "requires mapped GTK and disposable GSettings"]
fn osk_mapped_gtk_removed_entry_row_no_ghost_save() {
    let (win, _) = host();
    let prefs = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let row = adw::EntryRow::new();
    row.set_title("Transient mpv argument");
    row.set_text("--existing");
    group.add(&row);
    page.add(&group);
    prefs.add(&page);
    prefs.present(Some(&win));
    spin();

    let settings = crate::settings::TvSettings::get();
    settings
        .set_strv("mpv-arguments", ["--existing"])
        .expect("prepare isolated fixture");
    row.connect_changed({
        let settings = settings.clone();
        move |row| {
            settings
                .set_strv("mpv-arguments", [row.text().to_string()])
                .expect("fixture immediate-save callback");
        }
    });

    show_with_options(&row, KeyboardOptions::default());
    let dialog = keyboard(&win, &row);
    find_entry(dialog.upcast_ref())
        .expect("draft exists")
        .set_text("--ghost");
    group.remove(&row); // Remove or Restore/Undo drops this widget instance.
    spin();
    if dialog.is_mapped() {
        click(&dialog, "Done");
    }
    assert_eq!(
        settings.strv("mpv-arguments")[0].as_str(),
        "--existing",
        "removed widget must not trigger stale mpv settings save"
    );
    prefs.close();
    win.destroy();
}

#[test]
#[ignore = "requires mapped GTK and isolated test GSettings"]
fn osk_mapped_gtk_all_keys_fit_without_scrolling() {
    // Regression for real 2026-10-20 Wayland report: the old seven-action
    // row was wider than the dialog; Cancel or Backspace disappeared.
    let (win, body) = host();
    let entry = gtk::Entry::new();
    body.append(&entry);
    spin();
    show(&entry);
    let dialog = keyboard(&win, &entry);
    let content = dialog.child().expect("GTK-owned keyboard content");
    fn scan(widget: &gtk::Widget, buttons: &mut Vec<gtk::Button>) {
        assert!(
            !widget.is::<gtk::ScrolledWindow>(),
            "keyboard must not require scrolling to reach its actions"
        );
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            buttons.push(button.clone());
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            scan(&item, buttons);
            child = item.next_sibling();
        }
    }
    let mut buttons = Vec::new();
    scan(&content, &mut buttons);
    assert_eq!(buttons.len(), 10 + 11 + 11 + 10 + 12 + 5 + 5);
    let assert_inside = || {
        for button in &buttons {
            assert!(button.is_mapped(), "every key must be mapped");
            let rect = button
                .compute_bounds(&dialog)
                .expect("mapped key has dialog-relative coordinates");
            assert!(
                rect.x() >= -1.0
                    && rect.x() + rect.width() <= dialog.width() as f32 + 1.0
                    && rect.y() >= -1.0
                    && rect.y() + rect.height() <= dialog.height() as f32 + 1.0,
                "keyboard key {:?} lies outside dialog bounds",
                button.label()
            );
        }
    };
    assert_inside();
    for name in [
        "Backspace",
        "Caret left",
        "Space",
        "Caret right",
        "Clear",
        "Shift",
        "QWERTZ",
        "Symbols",
        "Done",
        "Cancel",
    ] {
        assert!(
            buttons
                .iter()
                .any(|button| button.label().as_deref() == Some(name)),
            "required key {name} must be visible"
        );
    }
    // Shift changes the German action label's natural width; it must still
    // leave Done and Cancel fully visible without any horizontal scrolling.
    click(&dialog, "Shift");
    assert_inside();
    click(&dialog, "Cancel");
    win.destroy();
}

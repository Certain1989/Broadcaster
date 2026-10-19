// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Mapped GTK/Xvfb regression probes. Intentionally ignored by ordinary,
//! potentially headless cargo test; Meson runs these on a real X11 display.

use super::*;

fn spin() {
    let context = glib::MainContext::default();
    for _ in 0..10 {
        while context.pending() {
            context.iteration(false);
        }
    }
}

fn host() -> (adw::Window, gtk::Box) {
    adw::init().expect("mapped fixture needs GTK/adw");
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
    assert!(button.is_mapped());
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

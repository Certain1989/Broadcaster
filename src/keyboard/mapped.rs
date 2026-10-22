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
    let resources =
        adw::gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
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

fn find_persistent_caret(widget: &gtk::Widget) -> Option<gtk::DrawingArea> {
    if let Some(area) = widget.downcast_ref::<gtk::DrawingArea>() {
        if area.has_css_class("osk-persistent-caret") {
            return Some(area.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(item) = child {
        if let Some(area) = find_persistent_caret(&item) {
            return Some(area);
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
fn osk_mapped_gtk_multiple_changed_signal_observers() {
    // R-01: Make the observable signal trace explicit, without pretending it
    // proves equivalence for every custom/delegated GtkEditable implementation.
    let (win, body) = host();
    let entry = gtk::Entry::new();
    entry.set_text("before");
    body.append(&entry);
    spin();

    let observed = Rc::new(RefCell::new(Vec::<String>::new()));
    for id in ["first", "second"] {
        let observed = observed.clone();
        entry.connect_changed(move |entry| {
            observed.borrow_mut().push(format!("{id}:{}", entry.text()));
        });
    }
    let notified = Rc::new(RefCell::new(Vec::<String>::new()));
    entry.connect_notify_local(Some("text"), {
        let notified = notified.clone();
        move |entry, _| notified.borrow_mut().push(entry.text().to_string())
    });

    show(&entry);
    let dlg = keyboard(&win, &entry);
    find_entry(dlg.upcast_ref())
        .expect("draft")
        .set_text("after");
    assert!(observed.borrow().is_empty(), "draft is isolated");
    click(&dlg, "Done");
    assert_eq!(
        observed.borrow().as_slice(),
        &["first:after".to_string(), "second:after".to_string()],
        "every changed observer must see the final text exactly once"
    );
    assert_eq!(entry.text(), "after");
    assert_eq!(notified.borrow().last().map(String::as_str), Some("after"));
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_caret_visible_with_remote_key_focus() {
    let (win, body) = host();
    let source = gtk::Entry::new();
    source.set_text("unchanged");
    body.append(&source);
    spin();
    show(&source);
    let dialog = keyboard(&win, &source);
    let preview = find_entry(dialog.upcast_ref()).expect("native draft preview");
    let canvas = find_persistent_caret(dialog.upcast_ref()).expect("noninteractive cursor overlay");
    assert!(canvas.is_mapped(), "overlay must map with the OSK");
    assert!(
        !canvas.can_target(),
        "cursor must not intercept remote/mouse focus"
    );

    preview.set_text("aäöbcd");
    preview.set_position(3);
    // Real OSK interaction keeps focus on the buttons, not the GtkText.
    let left = find_button(dialog.upcast_ref(), "←").expect("left caret key");
    assert!(left.grab_focus());
    spin();
    assert!(left.has_focus());
    assert!(!draft_native_text_has_focus(&preview));
    let original =
        remote_preview_caret_rect(&preview, &canvas).expect("visible remote insertion cursor");

    click(&dialog, "←");
    assert_eq!(preview.position(), 2);
    let moved =
        remote_preview_caret_rect(&preview, &canvas).expect("cursor remains visible after Left");
    assert!(
        moved.x() < original.x(),
        "visible cursor must follow the actual Unicode insertion position"
    );
    click(&dialog, "x");
    assert_eq!(
        preview.text(),
        "aäxöbcd",
        "insert at displayed cursor, not at the end"
    );
    assert!(
        remote_preview_caret_rect(&preview, &canvas).is_some(),
        "cursor stays visible after character insertion"
    );
    click(&dialog, "→");
    assert!(
        remote_preview_caret_rect(&preview, &canvas).is_some(),
        "cursor stays visible after Right"
    );

    // The real GtkText owns horizontal scrolling for long URLs; the
    // visual caret follows its own geometry rather than assuming fixed glyphs.
    preview.set_text(&format!("{}äöß", "w".repeat(240)));
    preview.set_position(240);
    spin();
    let long = remote_preview_caret_rect(&preview, &canvas)
        .expect("long horizontally scrolled text keeps a visible cursor");
    let bounds = canvas
        .compute_bounds(&dialog)
        .expect("mapped overlay bounds");
    assert!(long.width() > 0.0 && long.height() > 0.0);
    assert!(long.x() >= 0.0 && long.x() + long.width() <= bounds.width() + 1.0);

    // GTK's original native insertion caret must take over without a
    // doubled indicator when physical typing refocuses the preview.
    assert!(preview.grab_focus());
    spin();
    assert!(draft_native_text_has_focus(&preview));
    assert!(remote_preview_caret_rect(&preview, &canvas).is_none());
    assert!(left.grab_focus());
    spin();
    assert!(remote_preview_caret_rect(&preview, &canvas).is_some());

    assert_eq!(
        source.text(),
        "unchanged",
        "preview edits must not escape on Cancel"
    );
    click(&dialog, "Cancel");
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_caret_buttons_collapse_selection() {
    let (win, body) = host();
    let source = gtk::Entry::new();
    source.set_text("unchanged");
    body.append(&source);
    spin();
    show(&source);
    let dialog = keyboard(&win, &source);
    let draft = find_entry(dialog.upcast_ref()).expect("native preview");
    draft.set_text("abcdef");
    for &(a, b) in &[(2, 4), (4, 2)] {
        draft.select_region(a, b);
        click(&dialog, "←");
        assert!(draft.selection_bounds().is_none());
        assert_eq!(draft.position(), 2, "Left collapses to selection start");

        draft.select_region(a, b);
        click(&dialog, "→");
        assert!(draft.selection_bounds().is_none());
        assert_eq!(draft.position(), 4, "Right collapses to selection end");
    }
    assert_eq!(source.text(), "unchanged", "draft actions cannot save");
    click(&dialog, "Cancel");
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_pending_selection_native_backspace() {
    let (win, body) = host();
    let source = gtk::Entry::new();
    source.set_text("abcdef");
    body.append(&source);
    spin();
    let changes = Rc::new(Cell::new(0));
    source.connect_changed({
        let changes = changes.clone();
        move |_| changes.set(changes.get() + 1)
    });

    // Force the precise focus-loss state: selection retained by the OSK
    // but collapsed in GtkText. The native action must delete "cd".
    show(&source);
    let dlg = keyboard(&win, &source);
    let draft = find_entry(dlg.upcast_ref()).expect("draft");
    draft.set_position(4);
    assert!(draft.selection_bounds().is_none());
    assert!(native_preview_backspace(&draft, Some((2, 4))));
    assert_eq!(draft.text(), "abef");
    assert_eq!(source.text(), "abcdef");
    click(&dlg, "Cancel");
    assert_eq!(source.text(), "abcdef");
    assert_eq!(changes.get(), 0);

    // Exercise the actual mapped key/selection capture path separately.
    show(&source);
    let dlg = keyboard(&win, &source);
    let draft = find_entry(dlg.upcast_ref()).expect("draft");
    draft.select_region(2, 4);
    click(&dlg, "Backspace");
    assert_eq!(draft.text(), "abef", "selected span wins over caret");
    assert_eq!(source.text(), "abcdef", "draft must remain private");
    click(&dlg, "Done");
    assert_eq!(source.text(), "abef");
    assert_eq!(changes.get(), 1, "Done emits one original changed");
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_compact_add_playlist_url_launcher() {
    let (win, body) = host();
    let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
    form.set_hexpand(true);
    body.append(&form);
    let url = gtk::Entry::new();
    url.set_hexpand(true);
    form.append(&url);
    let launcher = gtk::Button::with_label("Keyboard");
    style_add_playlist_field_launcher(&launcher);
    bind_launcher(&launcher, &url, KeyboardOptions::url());
    form.append(&launcher);
    spin();

    assert!(
        form.width() > 300,
        "fixture must exercise wide dialog layout"
    );
    assert!(
        launcher.is_mapped() && launcher.width() >= 48 && launcher.height() >= 44,
        "launcher mapped={}, size={}x{}, request={}x{}, visible={}",
        launcher.is_mapped(),
        launcher.width(),
        launcher.height(),
        launcher.width_request(),
        launcher.height_request(),
        launcher.is_visible()
    );
    assert!(
        launcher.width() < 100,
        "icon button must not stretch to field width"
    );
    let field_rect = url.compute_bounds(&form).expect("mapped URL bounds");
    let key_rect = launcher
        .compute_bounds(&form)
        .expect("mapped launcher bounds");
    assert!(
        ((field_rect.x() + field_rect.width()) - (key_rect.x() + key_rect.width())).abs() <= 4.0,
        "launcher must align at the right edge"
    );
    assert!(url.grab_focus());
    // GtkEntry delegates keyboard focus to its GtkText child on GTK4.
    // Checking has_focus() on the outer Entry incorrectly rejects a
    // successful native focus transfer. Verify the actual text delegate.
    assert!(
        url.delegate()
            .and_downcast::<gtk::Text>()
            .is_some_and(|text| text.has_focus()),
        "URL entry's native GtkText delegate must own keyboard focus"
    );
    assert!(launcher.grab_focus());
    assert!(launcher.has_focus(), "launcher must be focusable by remote");
    assert!(url.grab_focus());
    win.destroy();
}

// A mapped focus assertion, not synthetic GDK key-dispatch proof. GTK4
// directs actual keyboard input to the GtkText delegate of GtkEntry.
#[test]
#[ignore = "requires mapped GTK and isolated test GSettings"]
fn osk_mapped_gtk_native_draft_focus_clears_collapsed_selection() {
    let (win, body) = host();
    let source = gtk::Entry::new();
    source.set_text("abcdef");
    body.append(&source);
    spin();
    let changes = Rc::new(Cell::new(0));
    source.connect_changed({
        let changes = changes.clone();
        move |_| changes.set(changes.get() + 1)
    });

    show(&source);
    let dlg = keyboard(&win, &source);
    let draft = find_entry(dlg.upcast_ref()).expect("native draft");
    assert!(draft.grab_focus());
    spin();
    assert!(
        draft_native_text_has_focus(&draft),
        "native GtkText delegate must report focus while draft is editable"
    );
    draft.select_region(1, 3);
    assert!(draft.selection_bounds().is_some());
    draft.set_position(5); // Real focused caret motion collapses selection.
    spin();
    assert!(draft.selection_bounds().is_none());
    assert!(draft_native_text_has_focus(&draft));
    click(&dlg, "a");
    assert_eq!(
        draft.text().as_str(),
        "abcdeaf",
        "focused deselection must discard stale pending selection"
    );
    assert_eq!(source.text().as_str(), "abcdef");
    click(&dlg, "Cancel");
    assert_eq!(source.text().as_str(), "abcdef");
    assert_eq!(changes.get(), 0, "Cancel must not notify the origin");

    show(&source);
    let dlg = keyboard(&win, &source);
    let draft = find_entry(dlg.upcast_ref()).expect("native draft");
    assert!(draft.grab_focus());
    spin();
    assert!(draft_native_text_has_focus(&draft));
    draft.select_region(1, 3);
    draft.set_position(5);
    spin();
    assert!(draft.selection_bounds().is_none());
    click(&dlg, "Backspace");
    assert_eq!(
        draft.text().as_str(),
        "abcdf",
        "Backspace after focused deselection must delete at the new caret"
    );
    assert_eq!(source.text().as_str(), "abcdef");
    click(&dlg, "Done");
    assert_eq!(source.text().as_str(), "abcdf");
    assert_eq!(changes.get(), 1, "Done must notify the origin exactly once");
    win.destroy();
}

#[test]
#[ignore = "requires mapped GTK and isolated test GSettings"]
fn osk_mapped_gtk_draft_selection_survives_key_focus_transfer() {
    let (win, body) = host();
    let source = gtk::Entry::new();
    source.set_text("abcdef");
    body.append(&source);
    spin();
    let changes = Rc::new(Cell::new(0));
    source.connect_changed({
        let changes = changes.clone();
        move |_| changes.set(changes.get() + 1)
    });

    show(&source);
    let dlg = keyboard(&win, &source);
    let draft = find_entry(dlg.upcast_ref()).expect("native draft");
    assert!(draft.grab_focus());
    spin();
    assert!(draft_native_text_has_focus(&draft));
    draft.select_region(2, 4);
    assert!(draft.selection_bounds().is_some());

    // The key owns native focus, so a selection collapsed by focus loss
    // must remain available for the user's next on-screen edit.
    let key = find_button(dlg.upcast_ref(), "a").expect("mapped character key");
    assert!(key.grab_focus());
    spin();
    assert!(key.has_focus());
    assert!(
        !draft_native_text_has_focus(&draft),
        "OSK key focus must differ from native draft focus"
    );
    key.emit_by_name::<()>("clicked", &[]);
    spin();
    assert_eq!(
        draft.text().as_str(),
        "abaef",
        "selection selected before focus transfer must be replaced"
    );
    assert_eq!(source.text().as_str(), "abcdef");
    click(&dlg, "Done");
    assert_eq!(source.text().as_str(), "abaef");
    assert_eq!(changes.get(), 1);
    win.destroy();
}

// Exercises observable wrapper signal semantics, not a claim of universal
// transaction atomicity across arbitrary GTK delegate implementations.
fn reentrant_signal_trace<T>(win: &adw::Window, target: &T)
where
    T: IsA<gtk::Widget> + IsA<gtk::Editable> + Clone + 'static,
{
    let editable: gtk::Editable = target.clone().upcast();
    let observed = Rc::new(RefCell::new(Vec::<String>::new()));
    editable.connect_changed({
        let observed = observed.clone();
        move |entry| {
            observed
                .borrow_mut()
                .push(format!("first:{}", entry.text()))
        }
    });
    let to_disconnect = Rc::new(RefCell::new(None::<glib::SignalHandlerId>));
    let id = editable.connect_changed({
        let observed = observed.clone();
        move |entry| {
            observed
                .borrow_mut()
                .push(format!("second:{}", entry.text()))
        }
    });
    *to_disconnect.borrow_mut() = Some(id);
    let connected = Rc::new(Cell::new(false));
    let notified = Rc::new(Cell::new(0u32));
    let delegate_events = Rc::new(RefCell::new(Vec::<&'static str>::new()));
    if let Some(delegate) = editable.delegate() {
        delegate.connect_insert_text({
            let events = delegate_events.clone();
            move |_, _, _| events.borrow_mut().push("insert")
        });
        delegate.connect_delete_text({
            let events = delegate_events.clone();
            move |_, _, _| events.borrow_mut().push("delete")
        });
    }
    let search_changes = Rc::new(Cell::new(0u32));
    if let Ok(search) = target
        .clone()
        .upcast::<gtk::Widget>()
        .downcast::<gtk::SearchEntry>()
    {
        let events = search_changes.clone();
        search.connect_search_changed(move |_| events.set(events.get() + 1));
    }
    editable.connect_notify_local(Some("text"), {
        let observed = observed.clone();
        let to_disconnect = to_disconnect.clone();
        let connected = connected.clone();
        let notified = notified.clone();
        move |entry, _| {
            notified.set(notified.get() + 1);
            if entry.text().as_str() == "after" && !connected.replace(true) {
                if let Some(id) = to_disconnect.borrow_mut().take() {
                    entry.disconnect(id);
                }
                let observed = observed.clone();
                entry.connect_changed(move |entry| {
                    observed.borrow_mut().push(format!("new:{}", entry.text()))
                });
            }
        }
    });
    show_with_options(target, KeyboardOptions::default());
    let dlg = keyboard(win, target);
    find_entry(dlg.upcast_ref())
        .expect("draft")
        .set_text("after");
    assert!(observed.borrow().is_empty(), "draft cannot notify source");
    click(&dlg, "Cancel");
    assert!(observed.borrow().is_empty(), "Cancel cannot notify source");

    show_with_options(target, KeyboardOptions::default());
    let dlg = keyboard(win, target);
    find_entry(dlg.upcast_ref())
        .expect("draft")
        .set_text("after");
    click(&dlg, "Done");
    assert!(
        connected.get(),
        "native text notification must be observable"
    );
    assert!(notified.get() > 0);
    assert_eq!(editable.text(), "after");
    let trace = observed.borrow();
    assert_eq!(
        trace.iter().filter(|s| s.as_str() == "first:after").count(),
        1
    );
    assert_eq!(
        trace
            .iter()
            .filter(|s| s.as_str() == "second:after")
            .count(),
        0
    );
    assert_eq!(
        trace.iter().filter(|s| s.as_str() == "new:after").count(),
        1
    );
    drop(trace);
    observed.borrow_mut().clear();
    editable.set_text("later");
    let trace = observed.borrow();
    assert_eq!(
        trace.iter().filter(|s| s.as_str() == "first:later").count(),
        1
    );
    assert_eq!(
        trace.iter().filter(|s| s.as_str() == "new:later").count(),
        1,
        "newly added callback must not remain accidentally blocked"
    );
    eprintln!(
        "Mapped GTK delegate trace: {} insert/delete events, {} search-changed events",
        delegate_events.borrow().len(),
        search_changes.get()
    );
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_entry_reentrant_signal_trace() {
    let (win, body) = host();
    let entry = gtk::Entry::new();
    entry.set_text("before");
    body.append(&entry);
    spin();
    reentrant_signal_trace(&win, &entry);
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_search_reentrant_signal_trace() {
    let (win, body) = host();
    let search = gtk::SearchEntry::new();
    search.set_text("before");
    body.append(&search);
    spin();
    reentrant_signal_trace(&win, &search);
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_entry_row_reentrant_signal_trace() {
    let (win, _) = host();
    let prefs = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let row = adw::EntryRow::new();
    row.set_title("Synthetic editable");
    row.set_text("before");
    group.add(&row);
    page.add(&group);
    prefs.add(&page);
    prefs.present(Some(&win));
    spin();
    assert!(row.is_mapped());
    reentrant_signal_trace(&win, &row);
    prefs.close();
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_native_url_correction_clears_error() {
    let (win, body) = host();
    let source = gtk::Entry::new();
    source.set_text("https://example.invalid/previous.m3u");
    body.append(&source);
    spin();
    show_with_options(&source, KeyboardOptions::url());
    let dialog = keyboard(&win, &source);
    let draft = find_entry(dialog.upcast_ref()).expect("native URL draft");
    fn error_label(widget: &gtk::Widget) -> Option<gtk::Label> {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            if label.has_css_class("error") {
                return Some(label.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(item) = child {
            if let Some(label) = error_label(&item) {
                return Some(label);
            }
            child = item.next_sibling();
        }
        None
    }
    let error = error_label(dialog.upcast_ref()).expect("URL validation label");

    draft.set_text("not-a-url");
    click(&dialog, "Done");
    assert!(!error.text().is_empty(), "invalid URL shows feedback");
    draft.set_text("https://example.invalid/corrected.m3u");
    assert!(
        error.text().is_empty(),
        "native correction clears stale error"
    );
    assert_eq!(source.text(), "https://example.invalid/previous.m3u");
    click(&dialog, "Cancel");
    assert_eq!(source.text(), "https://example.invalid/previous.m3u");
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_native_unicode_backspace() {
    let (win, body) = host();
    let entry = gtk::Entry::new();
    entry.set_text("unchanged");
    body.append(&entry);
    spin();
    show(&entry);
    let dlg = keyboard(&win, &entry);
    let preview = find_entry(dlg.upcast_ref()).expect("GTK editable preview");

    preview.set_text("e\u{301}");
    preview.set_position(-1);
    click(&dlg, "Backspace");
    assert_eq!(
        preview.text(),
        "",
        "native Backspace must remove the accent cluster"
    );
    assert_eq!(
        entry.text(),
        "unchanged",
        "the source is isolated until Done"
    );

    preview.set_text("👨‍👩‍👧");
    preview.set_position(-1);
    click(&dlg, "Backspace");
    assert_eq!(
        preview.text(),
        "",
        "native Backspace handles a joined emoji cluster"
    );
    click(&dlg, "Cancel");
    assert_eq!(entry.text(), "unchanged");
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
        "←",
        "Space",
        "→",
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
    // The compact visual symbols still expose translated, descriptive help
    // even after the OSK relabels itself for shift/layout transitions.
    let assert_caret_help = || {
        for (symbol, name) in [("←", "Caret left"), ("→", "Caret right")] {
            let button = find_button(&content, symbol).expect("mapped caret arrow button");
            assert_eq!(
                button.tooltip_text().as_deref(),
                Some(gettext(name).as_str()),
                "symbolic caret controls must keep accessible descriptive help"
            );
        }
    };
    assert_caret_help();
    // Shift changes the German action label's natural width; it must still
    // leave Done and Cancel fully visible without any horizontal scrolling.
    click(&dialog, "Shift");
    assert_inside();
    assert_caret_help();
    click(&dialog, "Cancel");
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_done_and_back_release_dialog_and_draft() {
    let (win, body) = host();
    let origin = gtk::Entry::new();
    origin.set_text("before");
    body.append(&origin);
    spin();

    // A Done transaction and an external dialog dismissal must each release
    // the complete OSK subtree, not just hide the dialog.
    for (finish, draft_text, expected) in [
        ("Done", "committed", "committed"),
        ("Back-close", "discarded", "committed"),
    ] {
        show(&origin);
        let dialog = keyboard(&win, &origin);
        let draft = find_entry(dialog.upcast_ref()).expect("native draft");
        let weak_dialog = dialog.downgrade();
        let weak_draft = draft.downgrade();
        draft.set_text(draft_text);

        if finish == "Done" {
            click(&dialog, "Done");
        } else {
            // Exercise Adw's non-button close path. Physical GDK Back/Escape
            // event propagation is a separate, unproven requirement.
            dialog.close();
        }
        assert_eq!(origin.text(), expected, "correct transaction boundary");
        drop(draft);
        drop(dialog);
        for _ in 0..80 {
            spin();
            if weak_dialog.upgrade().is_none() && weak_draft.upgrade().is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        assert!(weak_dialog.upgrade().is_none(), "{finish}: dialog retained");
        assert!(weak_draft.upgrade().is_none(), "{finish}: draft retained");
    }
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_parent_close_during_draft() {
    let (win, _) = host();
    let parent = adw::Dialog::new();
    let origin = gtk::Entry::new();
    origin.set_text("before");
    parent.set_child(Some(&origin));
    parent.present(Some(&win));
    spin();
    assert!(
        origin.is_mapped(),
        "origin must begin inside a mapped parent"
    );

    show(&origin);
    let osk = keyboard(&win, &origin);
    find_entry(osk.upcast_ref())
        .expect("draft")
        .set_text("should never commit");
    parent.close();
    for _ in 0..80 {
        spin();
        if !origin.is_mapped() {
            break;
        }
    }
    assert!(
        !origin.is_mapped(),
        "closing parent must unmap its editable"
    );
    if osk.is_mapped() {
        click(&osk, "Done");
    }
    assert_eq!(
        origin.text(),
        "before",
        "an orphaned keyboard cannot commit after its parent closes"
    );
    if osk.is_mapped() {
        osk.close();
    }
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_cancel_invalidated_origin() {
    let (win, body) = host();
    let origin = gtk::Entry::new();
    origin.set_text("before");
    body.append(&origin);
    spin();

    show(&origin);
    let osk = keyboard(&win, &origin);
    find_entry(osk.upcast_ref())
        .expect("draft")
        .set_text("ghost");
    origin.set_sensitive(false);
    if osk.is_mapped() {
        click(&osk, "Cancel");
    }
    assert_eq!(origin.text(), "before");
    origin.set_sensitive(true);

    show(&origin);
    let osk = keyboard(&win, &origin);
    find_entry(osk.upcast_ref())
        .expect("draft")
        .set_text("ghost");
    body.remove(&origin);
    spin();
    if osk.is_mapped() {
        click(&osk, "Cancel");
    }
    assert_eq!(origin.text(), "before", "removed origin stays unchanged");
    win.destroy();
}

#[test]
#[ignore = "requires a mapped display and isolated test GSettings"]
fn osk_mapped_gtk_dropped_origin_cancel_nonretention() {
    let (win, body) = host();
    let origin = gtk::Entry::new();
    origin.set_text("before");
    let weak_origin = origin.downgrade();
    body.append(&origin);
    spin();

    show(&origin);
    let osk = keyboard(&win, &origin);
    find_entry(osk.upcast_ref())
        .expect("draft")
        .set_text("ghost");
    body.remove(&origin);
    drop(origin);
    spin();

    // A dead editable must never be retained by the open OSK or restored
    // from its draft when the dialog is cancelled.
    if osk.is_mapped() {
        click(&osk, "Cancel");
    }
    drop(osk);
    for _ in 0..80 {
        spin();
        if weak_origin.upgrade().is_none() {
            break;
        }
    }
    assert!(
        weak_origin.upgrade().is_none(),
        "removed/dropped origin must not be retained by the keyboard"
    );
    win.destroy();
}

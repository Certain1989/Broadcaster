// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GTK-owned on-demand editing for TV remotes. K2 binds the remaining fields.
//! No per-key change escapes the separate draft GtkEntry before Done.

use adw::{glib, gtk, prelude::*};
use gettextrs::gettext;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const ROWS: &[&[&str]] = &[
    &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
    &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "ü"],
    &["a", "s", "d", "f", "g", "h", "j", "k", "l", "ö", "ä"],
    &["z", "x", "c", "v", "b", "n", "m", "ß", ",", "."],
    &[":", "/", "?", "&", "=", "#", "@", "+", "-", "_", "%", "~"],
    &["Backspace", "Left", "Space", "Right", "Clear"],
    &["Shift", "QWERTZ", "Symbols", "Done", "Cancel"],
];
const SYMBOLS: &[&[&str]] = &[
    &["!", "'", "\"", "(", ")", "[", "]", "{", "}", "\\", "|"],
    &[";", "*", "$", "^", "<", ">", "€", "£", "§", "°", "~"],
    &["ä", "ö", "ü", "ß", "ẞ", ":", "/", "?", "&", "="],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardLayout {
    Text,
    Url,
}

/// Typed K2 registration contract: a caller supplies a *particular editable*,
/// its intended layout and an optional error-only validator (never log drafts).
pub type KeyboardValidator = dyn Fn(&str) -> Result<(), String>;

#[derive(Clone)]
pub struct KeyboardOptions {
    pub layout: KeyboardLayout,
    pub validate: Option<Rc<KeyboardValidator>>,
}
impl Default for KeyboardOptions {
    fn default() -> Self {
        Self {
            layout: KeyboardLayout::Text,
            validate: None,
        }
    }
}
impl KeyboardOptions {
    pub fn url() -> Self {
        Self {
            layout: KeyboardLayout::Url,
            validate: Some(Rc::new(|value| {
                if reqwest::Url::parse(value).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https") && url.host().is_some()
                }) {
                    Ok(())
                } else {
                    Err(gettext("Enter a valid HTTP(S) playlist URL"))
                }
            })),
        }
    }
}

/// A weak, live field binding. Changing Settings immediately shows/hides its
/// D-pad launcher; keyboard OFF leaves native editing completely untouched.
pub fn bind_launcher<T>(button: &gtk::Button, target: &T, options: KeyboardOptions)
where
    T: IsA<gtk::Widget> + IsA<gtk::Editable> + Clone + 'static,
{
    // Icon-only launchers require an explicit screen-reader name; tooltip
    // also documents the remote OK action without consuming toolbar width.
    let description = gettext("Open on-screen keyboard");
    button.set_tooltip_text(Some(&description));
    button.update_property(&[gtk::accessible::Property::Label(&description)]);
    crate::settings::TvSettings::get()
        .bind_osk_enabled(button, "visible")
        .flags(adw::gio::SettingsBindFlags::GET)
        .build();
    let weak = target.clone().upcast::<gtk::Widget>().downgrade();
    button.connect_clicked(move |_| {
        if let Some(widget) = weak.upgrade() {
            show_widget(&widget, options.clone());
        }
    });
}

/// Layout contract for the existing Add Playlist URL launcher only.
pub(crate) fn style_add_playlist_url_launcher(button: &gtk::Button) {
    button.set_icon_name("input-keyboard-symbolic");
    button.set_size_request(48, 44);
    button.set_halign(gtk::Align::End);
}

/// Caps Lock and Num Lock must not disable D-pad navigation. Reject only
/// modifiers with an actual navigation/selection meaning.
pub(crate) fn unmodified_remote(modifiers: gtk::gdk::ModifierType) -> bool {
    !modifiers.intersects(
        gtk::gdk::ModifierType::SHIFT_MASK
            | gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::ALT_MASK
            | gtk::gdk::ModifierType::SUPER_MASK
            | gtk::gdk::ModifierType::META_MASK,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

/// Vertical navigation compares *actual mapped allocation centres* in the
/// common rows coordinate system. Equal distances favour leftmost buttons.
/// Left/Right wrap within their visible row, Up/Down across the top/bottom.
fn next_key(
    keys: &[Vec<glib::WeakRef<gtk::Button>>],
    rows: &gtk::Box,
    (row, col): (usize, usize),
    direction: Arrow,
) -> (usize, usize) {
    match direction {
        Arrow::Left => (row, (col + keys[row].len() - 1) % keys[row].len()),
        Arrow::Right => (row, (col + 1) % keys[row].len()),
        Arrow::Up | Arrow::Down => {
            let target_row = if direction == Arrow::Up {
                (row + keys.len() - 1) % keys.len()
            } else {
                (row + 1) % keys.len()
            };
            let center = |r: usize, c: usize| -> f64 {
                let Some(button) = keys[r][c].upgrade() else {
                    return 0.0;
                };
                if let Some(rect) = button.compute_bounds(rows) {
                    (rect.x() + rect.width() / 2.0) as f64
                } else {
                    (c as f64 + 0.5) / keys[r].len() as f64
                }
            };
            let x = center(row, col);
            let col = (0..keys[target_row].len())
                .min_by(|&a, &b| {
                    (center(target_row, a) - x)
                        .abs()
                        .total_cmp(&(center(target_row, b) - x).abs())
                        .then_with(|| a.cmp(&b))
                })
                .unwrap_or(0);
            (target_row, col)
        }
    }
}

#[derive(Debug, Clone)]
struct Draft {
    text: String,
    cursor: usize,
    selection: Option<(usize, usize)>,
}
impl Draft {
    fn new(text: &str, cursor: usize, selection: Option<(usize, usize)>) -> Self {
        Self {
            text: text.into(),
            cursor: cursor.min(text.chars().count()),
            selection,
        }
    }
    fn offset(&self, pos: usize) -> usize {
        self.text
            .char_indices()
            .nth(pos)
            .map_or(self.text.len(), |(i, _)| i)
    }
    fn remove_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection.take() else {
            return false;
        };
        let (a, b) = (
            a.min(b).min(self.text.chars().count()),
            a.max(b).min(self.text.chars().count()),
        );
        if a == b {
            return false;
        }
        self.text.replace_range(self.offset(a)..self.offset(b), "");
        self.cursor = a;
        true
    }
    fn insert(&mut self, s: &str) {
        self.remove_selection();
        self.text.insert_str(self.offset(self.cursor), s);
        self.cursor += s.chars().count();
    }
    fn backspace(&mut self) {
        if self.remove_selection() {
            return;
        }
        if self.cursor > 0 {
            let end = self.offset(self.cursor);
            self.cursor -= 1;
            self.text.replace_range(self.offset(self.cursor)..end, "");
        }
    }
    fn move_caret(&mut self, left: bool) {
        if let Some((a, b)) = self.selection.take() {
            if a != b {
                // Native GTK arrows collapse a selection to its corresponding
                // edge; they do not move an extra character into the range.
                self.cursor = if left { a.min(b) } else { a.max(b) };
                return;
            }
        }
        self.cursor = if left {
            self.cursor.saturating_sub(1)
        } else {
            (self.cursor + 1).min(self.text.chars().count())
        };
    }
}

fn label_for(r: usize, c: usize, qwertz: bool, shift: bool, symbols: bool) -> String {
    let raw = if (1..=3).contains(&r) && symbols {
        SYMBOLS[r - 1][c]
    } else if r == 1 && c == 5 && qwertz {
        "z"
    } else if r == 3 && c == 0 && qwertz {
        "y"
    } else {
        ROWS[r][c]
    };
    if r <= 4 {
        if shift {
            if raw == "ß" {
                "ẞ".into()
            } else {
                raw.to_uppercase()
            }
        } else {
            raw.into()
        }
    } else {
        match raw {
            "Backspace" => gettext("Backspace"),
            "Left" => gettext("Caret left"),
            "Space" => gettext("Space"),
            "Right" => gettext("Caret right"),
            "Clear" => gettext("Clear"),
            "Done" => gettext("Done"),
            "Cancel" => gettext("Cancel"),
            "Shift" => {
                if shift {
                    gettext("Shift on")
                } else {
                    gettext("Shift")
                }
            }
            "QWERTZ" => {
                if qwertz {
                    gettext("QWERTY")
                } else {
                    gettext("QWERTZ")
                }
            }
            "Symbols" => {
                if symbols {
                    gettext("Letters")
                } else {
                    gettext("Symbols")
                }
            }
            _ => raw.into(),
        }
    }
}
fn relabel(keys: &[Vec<glib::WeakRef<gtk::Button>>], qwertz: bool, shift: bool, symbols: bool) {
    for (r, row) in keys.iter().enumerate() {
        for (c, weak) in row.iter().enumerate() {
            if let Some(button) = weak.upgrade() {
                let label = label_for(r, c, qwertz, shift, symbols);
                button.set_label(&label);
                button.set_tooltip_text(Some(&label));
            }
        }
    }
}
fn from_preview(preview: &gtk::Entry) -> Draft {
    Draft::new(
        preview.text().as_str(),
        preview.position().max(0) as usize,
        preview
            .selection_bounds()
            .map(|(a, b)| (a.max(0) as usize, b.max(0) as usize)),
    )
}

/// Preserve the selection GtkEntry may collapse during focus transfer, then
/// delegate deletion to native GtkText for correct grapheme-cluster handling.
fn native_preview_backspace(preview: &gtk::Entry, pending: Option<(usize, usize)>) -> bool {
    let Some(text) = preview.delegate().and_downcast::<gtk::Text>() else {
        return false;
    };
    if preview.selection_bounds().is_none() {
        if let Some((a, b)) = pending {
            let len = preview.text().chars().count();
            if a != b && a <= len && b <= len {
                preview.select_region(a as i32, b as i32);
            }
        }
    }
    text.emit_by_name::<()>("backspace", &[]);
    true
}
/// GtkEditable::set_text may deliver 'changed' once for deletion and again
/// for insertion when replacing nonempty contents. That leaks an intermediate
/// empty field to observers (e.g. immediate mpv GSettings persistence).
/// Deliver exactly one normal GtkEditable::changed notification to connected
/// application handlers, after the native setter finishes. No parent action,
/// virtual key event or separate settings write occurs here.
///
/// Temporarily block the currently active *changed* listeners by their
/// individual handler IDs. Property notifications during GTK's native setter
/// may disconnect a handler or attach a new one. Only handlers actually
/// blocked here may be unblocked afterward: a second pattern match can target
/// newly connected, never-blocked handlers and cause a GObject critical.
/// Native text and property notifications are not suppressed.
fn commit_editable_once(entry: &gtk::Editable, value: &str, cursor: i32) {
    if entry.text().as_str() == value {
        entry.set_position(cursor);
        return;
    }

    use glib::translate::{IntoGlib, ToGlibPtr};
    let owned: glib::Object = entry.clone().upcast();
    let object: *mut glib::gobject_ffi::GObject = owned.to_glib_none().0;
    // SAFETY: the GtkEditable owns a valid GObject for this synchronous call.
    // Signal IDs and handler IDs are inspected only while owned remains alive.
    // G_SIGNAL_MATCH_UNBLOCKED excludes already blocked handlers from lookup,
    // allowing each original handler to be blocked precisely once.
    unsafe {
        let signal = glib::gobject_ffi::g_signal_lookup(
            c"changed".as_ptr(),
            gtk::Editable::static_type().into_glib(),
        );
        assert_ne!(signal, 0, "GtkEditable must expose its changed signal");
        let mask = glib::gobject_ffi::G_SIGNAL_MATCH_ID
            | glib::gobject_ffi::G_SIGNAL_MATCH_UNBLOCKED;
        let null = std::ptr::null_mut();
        let opaque: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut blocked = Vec::new();
        loop {
            let id = glib::gobject_ffi::g_signal_handler_find(
                object, mask, signal, 0, null, opaque, opaque,
            );
            if id == 0 {
                break;
            }
            glib::gobject_ffi::g_signal_handler_block(object, id);
            blocked.push(id);
        }
        entry.set_text(value);
        entry.set_position(cursor);
        for id in blocked {
            if glib::gobject_ffi::g_signal_handler_is_connected(object, id) != 0 {
                glib::gobject_ffi::g_signal_handler_unblock(object, id);
            }
        }
    }
    entry.emit_by_name::<()>("changed", &[]);
}

fn target_live(
    widget: &gtk::Widget,
    parent: &Option<glib::WeakRef<gtk::Widget>>,
    window: &Option<glib::WeakRef<gtk::Window>>,
    initial: &str,
    invalidated: bool,
) -> bool {
    widget.is_mapped()
        && widget.is_sensitive()
        && !invalidated
        && widget.parent() == parent.as_ref().and_then(|w| w.upgrade())
        && widget.root().and_downcast::<gtk::Window>() == window.as_ref().and_then(|w| w.upgrade())
        && widget
            .clone()
            .downcast::<gtk::Editable>()
            .ok()
            .is_some_and(|entry| entry.text().as_str() == initial)
}

#[cfg(test)]
pub fn show<T>(target: &T)
where
    T: IsA<gtk::Widget> + IsA<gtk::Editable> + Clone + 'static,
{
    show_with_options(target, KeyboardOptions::default());
}

#[cfg(test)]
pub fn show_with_options<T>(target: &T, options: KeyboardOptions)
where
    T: IsA<gtk::Widget> + IsA<gtk::Editable> + Clone + 'static,
{
    if !crate::settings::TvSettings::get().osk_enabled() {
        return;
    }
    let widget: gtk::Widget = target.clone().upcast();
    show_widget(&widget, options);
}

fn show_widget(widget: &gtk::Widget, options: KeyboardOptions) {
    if !crate::settings::TvSettings::get().osk_enabled() {
        return;
    }
    let widget = widget.clone();
    let Ok(editable) = widget.clone().downcast::<gtk::Editable>() else {
        return;
    };
    if !widget.is_mapped() || !widget.is_sensitive() {
        return;
    }
    let initial = editable.text().to_string();
    let weak_target = widget.downgrade();
    let parent = widget.parent().map(|w| w.downgrade());
    let window = widget
        .root()
        .and_downcast::<gtk::Window>()
        .map(|w| w.downgrade());
    let invalidated = Rc::new(Cell::new(false));
    // Keep the committed final text only for the bounded close/idle return.
    // The initial target snapshot remains immutable for write validation.
    let restored_text = Rc::new(RefCell::new(initial.clone()));
    let committed = Rc::new(Cell::new(false));

    let dialog = adw::Dialog::new();
    // The window's existing remote capture prioritizes this modal marker.
    dialog.add_css_class("osk-feasibility");
    dialog.add_css_class("osk-production");
    dialog.set_title(&gettext("On-screen keyboard"));
    // Stay inside libadwaita's floating-sheet breakpoint at common TV sizes.
    dialog.set_content_width(600);
    dialog.set_content_height(510);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    let hint = gtk::Label::new(Some(&gettext(
        "Arrows choose keys; OK types; Back cancels.",
    )));
    hint.set_wrap(true);
    body.append(&hint);
    // The draft is a real GtkEntry: native selection, paste, IME, keyboard,
    // mouse and caret work here, without saving anything to the origin.
    let preview = gtk::Entry::new();
    preview.set_hexpand(true);
    preview.set_text(&initial);
    preview.set_position(editable.position());
    if let Some((a, b)) = editable.selection_bounds() {
        preview.select_region(a, b);
    }
    preview.set_input_purpose(match options.layout {
        KeyboardLayout::Text => gtk::InputPurpose::FreeForm,
        KeyboardLayout::Url => gtk::InputPurpose::Url,
    });
    body.append(&preview);
    // GtkEntry may collapse its native selection as the user moves focus
    // onto an OSK key. Preserve the last live selection so that clicking
    // Caret left/right, Backspace or a character uses the pre-transfer range.
    // Never preserve it across an actual text edit or a focused deselection.
    let pending_selection: Rc<Cell<Option<(usize, usize)>>> = Rc::new(Cell::new(None));
    preview.connect_notify_local(Some("selection-bound"), {
        let pending_selection = pending_selection.clone();
        move |entry, _| {
            let selected = entry
                .selection_bounds()
                .map(|(a, b)| (a.max(0) as usize, b.max(0) as usize));
            if selected.is_some() || entry.has_focus() {
                pending_selection.set(selected);
            }
        }
    });
    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    body.append(&error);
    // Native typing, IME and paste must clear stale URL validation feedback
    // just as pressing an on-screen key does.
    let weak_error = error.downgrade();
    preview.connect_changed({
        let pending_selection = pending_selection.clone();
        move |_| {
            pending_selection.set(None);
            if let Some(error) = weak_error.upgrade() {
                error.set_text("");
            }
        }
    });
    // Keep every action visible at once. A GTK scroller used to clip
    // Backspace/Cancel on real Wayland TVs and moved the focused key offscreen.
    // Seven compact rows fit the dialog without scrolling at TV scale.
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 5);
    rows.set_hexpand(true);
    body.append(&rows);
    let selected = Rc::new(Cell::new((0usize, 0usize)));
    let qwertz = Rc::new(Cell::new(false));
    let shifted = Rc::new(Cell::new(false));
    let symbols = Rc::new(Cell::new(false));
    let keys: Rc<RefCell<Vec<Vec<glib::WeakRef<gtk::Button>>>>> = Rc::new(RefCell::new(Vec::new()));
    let weak_dialog = dialog.downgrade();
    for (r, labels) in ROWS.iter().enumerate() {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        line.set_hexpand(true);
        line.set_halign(gtk::Align::Fill);
        // Characters should be equal width. Action labels vary by language:
        // forcing them homogeneous would clip German 'Umschaltung aktiv'.
        line.set_homogeneous(r <= 4);
        if r == 6 {
            line.set_margin_top(6);
        }
        rows.append(&line);
        let mut refs = Vec::new();
        for (c, _) in labels.iter().enumerate() {
            let button = gtk::Button::with_label(&label_for(r, c, false, false, false));
            button.add_css_class("osk-key");
            button.set_height_request(42);
            button.set_hexpand(true);
            // Character rows are dense, action rows need readable labels.
            // Homogeneous allocation adapts both to the actual dialog width.
            button.set_width_request(if r <= 4 { 28 } else { 0 });
            line.append(&button);
            refs.push(button.downgrade());
            let selected = selected.clone();
            button.connect_has_focus_notify(move |button| {
                if button.has_focus() {
                    selected.set((r, c));
                }
            });

            // No descendant callback captures a strong dialog or origin.
            let weak_dialog = weak_dialog.clone();
            let preview = preview.clone();
            let pending_selection = pending_selection.clone();
            let error = error.clone();
            let weak_target = weak_target.clone();
            let parent = parent.clone();
            let window = window.clone();
            let initial = initial.clone();
            let invalidated = invalidated.clone();
            let restored_text = restored_text.clone();
            let committed = committed.clone();
            let qwertz = qwertz.clone();
            let shifted = shifted.clone();
            let symbols = symbols.clone();
            let keys = keys.clone();
            let options = options.clone();
            button.connect_clicked(move |_| {
                let Some(dialog) = weak_dialog.upgrade() else {
                    return;
                };
                match (r, c) {
                    (6, 4) => {
                        dialog.close();
                    }
                    (6, 3) => {
                        let Some(widget) = weak_target.upgrade() else {
                            dialog.close();
                            return;
                        };
                        if !target_live(&widget, &parent, &window, &initial, invalidated.get()) {
                            dialog.close();
                            return;
                        }
                        let value = preview.text().to_string();
                        if let Some(validate) = &options.validate {
                            if let Err(problem) = validate(&value) {
                                error.set_text(&problem);
                                return;
                            }
                        }
                        if let Ok(entry) = widget.downcast::<gtk::Editable>() {
                            commit_editable_once(&entry, &value, preview.position());
                            *restored_text.borrow_mut() = entry.text().to_string();
                            committed.set(true);
                        }
                        dialog.close();
                    }
                    (6, 0) => {
                        shifted.set(!shifted.get());
                        relabel(&keys.borrow(), qwertz.get(), shifted.get(), symbols.get());
                    }
                    (6, 1) => {
                        qwertz.set(!qwertz.get());
                        relabel(&keys.borrow(), qwertz.get(), shifted.get(), symbols.get());
                    }
                    (6, 2) => {
                        symbols.set(!symbols.get());
                        relabel(&keys.borrow(), qwertz.get(), shifted.get(), symbols.get());
                    }
                    _ => {
                        if (r, c) == (5, 0)
                            && native_preview_backspace(&preview, pending_selection.get())
                        {
                            pending_selection.set(None);
                            return;
                        }
                        let mut draft = from_preview(&preview);
                        if draft.selection.is_none() {
                            draft.selection = pending_selection.get();
                        }
                        pending_selection.set(None);
                        match (r, c) {
                            (5, 0) => draft.backspace(),
                            (5, 1) => draft.move_caret(true),
                            (5, 2) => draft.insert(" "),
                            (5, 3) => draft.move_caret(false),
                            (5, 4) => {
                                draft.text.clear();
                                draft.cursor = 0;
                                draft.selection = None;
                            }
                            _ => draft.insert(&label_for(
                                r,
                                c,
                                qwertz.get(),
                                shifted.get(),
                                symbols.get(),
                            )),
                        }
                        preview.set_text(&draft.text);
                        if matches!((r, c), (5, 1) | (5, 3)) {
                            // GTK may retain the selection when set_text is
                            // unchanged. A zero-length selected region
                            // explicitly collapses it at the requested edge.
                            preview.select_region(draft.cursor as i32, draft.cursor as i32);
                        } else {
                            preview.set_position(draft.cursor as i32);
                        }
                        error.set_text("");
                    }
                }
            });
        }
        keys.borrow_mut().push(refs);
    }
    dialog.set_child(Some(&body));

    let keys_for_nav = keys.clone();
    let weak_rows = rows.downgrade();
    let weak_preview = preview.downgrade();
    let weak_dialog_for_keys = dialog.downgrade();
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed(move |_, key, _, mods| {
        if !unmodified_remote(mods) {
            return glib::Propagation::Proceed;
        }
        use gtk::gdk::Key;
        // Physical Home retains native GtkEntry caret-to-start semantics.
        // Remote HomePage, Back and Escape dismiss only the topmost OSK.
        if matches!(key, Key::Back | Key::Escape | Key::HomePage) {
            if let Some(dialog) = weak_dialog_for_keys.upgrade() {
                dialog.close();
            }
            return glib::Propagation::Stop;
        }
        // Physical input in the preview retains native caret, IME, selection.
        if weak_preview.upgrade().is_some_and(|p| p.has_focus()) {
            return glib::Propagation::Proceed;
        }
        let arrow = match key {
            Key::Left | Key::KP_Left => Arrow::Left,
            Key::Right | Key::KP_Right => Arrow::Right,
            Key::Up | Key::KP_Up => Arrow::Up,
            Key::Down | Key::KP_Down => Arrow::Down,
            _ => return glib::Propagation::Proceed,
        };
        if let Some(rows) = weak_rows.upgrade() {
            let next = next_key(&keys_for_nav.borrow(), &rows, selected.get(), arrow);
            if let Some(button) = keys_for_nav.borrow()[next.0][next.1].upgrade() {
                button.grab_focus();
            }
        }
        glib::Propagation::Stop
    });
    dialog.add_controller(controller);

    // Any outside changes invalidate this draft even if a reset happens
    // to restore the same text later. Changed handler is removed on close.
    let changed = invalidated.clone();
    let handler = Rc::new(RefCell::new(Some(
        editable.connect_changed(move |_| changed.set(true)),
    )));
    let weak_editable = editable.downgrade();
    dialog.connect_closed(move |_| {
        if let (Some(entry), Some(handler)) = (weak_editable.upgrade(), handler.borrow_mut().take())
        {
            entry.disconnect(handler);
        }
        let weak_target = weak_target.clone();
        let weak_window = window.clone();
        let weak_parent = parent.clone();
        let invalidated = invalidated.clone();
        let restored_text = restored_text.clone();
        let committed = committed.clone();
        glib::idle_add_local_once(move || {
            let Some(window) = weak_window.as_ref().and_then(|w| w.upgrade()) else {
                return;
            };
            // Do not pull focus out of a newer active page/window.
            if !window.is_mapped() || !window.is_active() {
                return;
            }
            let original_parent = weak_parent.as_ref().and_then(|w| w.upgrade());
            if let Some(target) = weak_target.upgrade() {
                if target_live(
                    &target,
                    &weak_parent,
                    &weak_window,
                    &restored_text.borrow(),
                    invalidated.get() && !committed.get(),
                ) && target.grab_focus()
                {
                    return;
                }
            }
            // Never focus a reparented target, or run broad window-wide
            // TabForward navigation. Fall back only within the original,
            // still-mapped parent (if it still belongs to this window).
            if let Some(parent) = original_parent {
                if parent.is_mapped()
                    && parent.is_sensitive()
                    && parent.root().and_downcast::<gtk::Window>() == Some(window)
                {
                    parent.child_focus(gtk::DirectionType::TabForward);
                }
            }
        });
    });
    dialog.present(Some(&widget));
    let first = keys.borrow()[0][0].clone();
    glib::idle_add_local_once(move || {
        if let Some(button) = first.upgrade() {
            if button.is_mapped() {
                button.grab_focus();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caret_selection_collapse_without_extra_movement() {
        for &(a, b) in &[(2, 4), (4, 2)] {
            let mut left = Draft::new("abcdef", b, Some((a, b)));
            left.move_caret(true);
            assert_eq!(left.cursor, 2);
            assert_eq!(left.selection, None);
            left.insert("X");
            assert_eq!(left.text, "abXcdef");

            let mut right = Draft::new("abcdef", a, Some((a, b)));
            right.move_caret(false);
            assert_eq!(right.cursor, 4);
            assert_eq!(right.selection, None);
            right.insert("X");
            assert_eq!(right.text, "abcdXef");
        }
    }

    #[test]
    fn lock_modifiers_do_not_disable_remote_navigation() {
        use gtk::gdk::ModifierType as Mods;
        assert!(unmodified_remote(Mods::LOCK_MASK));
        assert!(!unmodified_remote(Mods::SHIFT_MASK | Mods::LOCK_MASK));
        assert!(!unmodified_remote(Mods::CONTROL_MASK));
    }

    #[test]
    fn unicode_selection_insert_and_backspace() {
        let mut d = Draft::new("aäößz", 4, Some((1, 4)));
        d.insert("Ü");
        assert_eq!(d.text, "aÜz");
        d.backspace();
        assert_eq!(d.text, "az");
    }
    #[test]
    fn draft_isolation_caret_and_shift() {
        let original = "https://example.invalid/?x=1";
        let mut d = Draft::new(original, 0, None);
        d.insert("ä");
        d.move_caret(false);
        d.backspace();
        assert_eq!(original, "https://example.invalid/?x=1");
        assert_eq!(label_for(3, 7, false, true, false), "ẞ");
        assert_eq!(label_for(1, 5, true, false, false), "z");
        assert!(ROWS[4].contains(&"%"));
        assert!(ROWS[4].contains(&"/"));
    }
}

#[cfg(test)]
mod mapped;

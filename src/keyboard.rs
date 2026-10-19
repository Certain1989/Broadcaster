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
    &[
        "Backspace",
        "Left",
        "Space",
        "Right",
        "Clear",
        "Done",
        "Cancel",
    ],
    &["Shift", "QWERTZ", "Symbols"],
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
#[derive(Clone)]
pub struct KeyboardOptions {
    pub layout: KeyboardLayout,
    pub validate: Option<Rc<dyn Fn(&str) -> Result<(), String>>>,
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
    crate::settings::TvSettings::get()
        .bind_osk_enabled(button, "visible")
        .build();
    let weak = target.clone().upcast::<gtk::Widget>().downgrade();
    button.connect_clicked(move |_| {
        if let Some(widget) = weak.upgrade() {
            show_widget(&widget, options.clone());
        }
    });
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
        self.selection = None;
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

pub fn show<T>(target: &T)
where
    T: IsA<gtk::Widget> + IsA<gtk::Editable> + Clone + 'static,
{
    show_with_options(target, KeyboardOptions::default());
}

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

    let dialog = adw::Dialog::new();
    // The window's existing remote capture prioritizes this modal marker.
    dialog.add_css_class("osk-feasibility");
    dialog.add_css_class("osk-production");
    dialog.set_title(&gettext("On-screen keyboard"));
    dialog.set_content_width(735);
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
    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    body.append(&error);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scroll.set_vexpand(true);
    body.append(&scroll);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 5);
    rows.set_valign(gtk::Align::Start);
    scroll.set_child(Some(&rows));
    let selected = Rc::new(Cell::new((0usize, 0usize)));
    let qwertz = Rc::new(Cell::new(false));
    let shifted = Rc::new(Cell::new(false));
    let symbols = Rc::new(Cell::new(false));
    let keys: Rc<RefCell<Vec<Vec<glib::WeakRef<gtk::Button>>>>> = Rc::new(RefCell::new(Vec::new()));
    let weak_dialog = dialog.downgrade();
    for (r, labels) in ROWS.iter().enumerate() {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        line.set_halign(gtk::Align::Center);
        if r == 6 {
            line.set_margin_top(6);
        }
        rows.append(&line);
        let mut refs = Vec::new();
        for (c, _) in labels.iter().enumerate() {
            let button = gtk::Button::with_label(&label_for(r, c, false, false, false));
            button.set_height_request(42);
            button.set_width_request(if r == 6 {
                154
            } else if r == 5 {
                if c == 2 {
                    168
                } else {
                    70
                }
            } else {
                49
            });
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
            let error = error.clone();
            let weak_target = weak_target.clone();
            let parent = parent.clone();
            let window = window.clone();
            let initial = initial.clone();
            let invalidated = invalidated.clone();
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
                    (5, 6) => { dialog.close(); }
                    (5, 5) => {
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
                            entry.set_text(&value); // exactly one normal commit
                            entry.set_position(preview.position());
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
                        let mut draft = from_preview(&preview);
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
                        preview.set_position(draft.cursor as i32);
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
        if !mods.is_empty() {
            return glib::Propagation::Proceed;
        }
        use gtk::gdk::Key;
        if matches!(key, Key::Back | Key::Escape | Key::Home | Key::HomePage) {
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
        glib::idle_add_local_once(move || {
            if let Some(target) = weak_target.upgrade() {
                if target.is_mapped() && target.is_sensitive() && target.grab_focus() {
                    return;
                }
            }
            if let Some(window) = weak_window.and_then(|w| w.upgrade()) {
                if window.is_mapped() {
                    window.child_focus(gtk::DirectionType::TabForward);
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

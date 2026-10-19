// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Deliberately bounded GTK-owned keyboard feasibility experiment.
//! This is not the production, application-wide on-screen keyboard.

use adw::{glib, gtk, prelude::*};
use gettextrs::gettext;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

// Rows have native-looking spacing and independent lengths; unlike a flat
// column-indexed grid, there are no phantom keys in the final action row.
// QWERTY swaps Y and Z without removing German characters or URL punctuation.
const ROWS: &[&[&str]] = &[
    &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
    &["q", "w", "e", "r", "t", "z", "u", "i", "o", "p", "ü"],
    &["a", "s", "d", "f", "g", "h", "j", "k", "l", "ö", "ä"],
    &["y", "x", "c", "v", "b", "n", "m", "ß", ",", "."],
    &[":", "/", "?", "&", "=", "#", "@", "+", "-", "_"],
    &[
        "Space", "Delete", "Left", "Right", "Clear",
        "Layout: QWERTZ", "Done", "Cancel",
    ],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

fn next_key((row, col): (usize, usize), direction: Arrow) -> (usize, usize) {
    match direction {
        Arrow::Left => (row, (col + ROWS[row].len() - 1) % ROWS[row].len()),
        Arrow::Right => (row, (col + 1) % ROWS[row].len()),
        Arrow::Up => {
            let next_row = row.saturating_sub(1);
            (next_row, col.min(ROWS[next_row].len() - 1))
        }
        Arrow::Down => {
            let next_row = (row + 1).min(ROWS.len() - 1);
            (next_row, col.min(ROWS[next_row].len() - 1))
        }
    }
}

#[derive(Clone, Debug)]
struct Draft {
    text: String,
    cursor: usize,
}
impl Draft {
    fn new(text: &str, cursor: usize) -> Self {
        Self {
            text: text.to_owned(),
            cursor: cursor.min(text.chars().count()),
        }
    }
    fn offset(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(i, _)| i)
    }
    fn insert(&mut self, text: &str) {
        let at = self.offset();
        self.text.insert_str(at, text);
        self.cursor += text.chars().count();
    }
    fn delete(&mut self) {
        if self.cursor > 0 {
            let end = self.offset();
            self.cursor -= 1;
            let start = self.offset();
            self.text.replace_range(start..end, "");
        }
    }
    fn move_caret(&mut self, left: bool) {
        if left {
            self.cursor = self.cursor.saturating_sub(1);
        } else {
            self.cursor = (self.cursor + 1).min(self.text.chars().count());
        }
    }
    fn split_at_caret(&self) -> (&str, &str) {
        self.text.split_at(self.offset())
    }
}

/// Open on demand from a real mapped GtkEditable. The original is only
/// changed by Done; Cancel/Back leaves it completely untouched.
pub fn show<T>(target: &T)
where
    T: IsA<gtk::Widget> + IsA<gtk::Editable> + Clone + 'static,
{
    let widget: gtk::Widget = target.clone().upcast();
    let editable: gtk::Editable = target.clone().upcast();
    if !widget.is_mapped() {
        return;
    }
    let weak_widget = widget.downgrade();
    let weak_parent = widget.parent().map(|parent| parent.downgrade());
    let weak_window = widget
        .root()
        .and_downcast::<gtk::Window>()
        .map(|w| w.downgrade());
    let draft = Rc::new(RefCell::new(Draft::new(
        editable.text().as_str(),
        editable.position().max(0) as usize,
    )));

    let dialog = adw::Dialog::new();
    dialog.add_css_class("osk-feasibility");
    dialog.set_title(&gettext("Keyboard prototype"));
    dialog.set_content_width(735);
    dialog.set_content_height(430);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    let instruction = gtk::Label::new(Some(&gettext(
        "Arrows select keys, OK activates, Back cancels.",
    )));
    body.append(&instruction);
    // Split the preview into two labels and a thin GTK separator. Unlike a
    // Unicode box-drawing glyph, this cursor takes no character-cell width:
    // the next letter appears directly against it, not after a blank space.
    let preview = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    preview.set_hexpand(true);
    let before = gtk::Label::new(None);
    before.set_selectable(false);
    before.set_xalign(1.0);
    let caret = gtk::Separator::new(gtk::Orientation::Vertical);
    caret.set_height_request(22);
    let after = gtk::Label::new(None);
    after.set_selectable(false);
    after.set_xalign(0.0);
    {
        let state = draft.borrow();
        let (left, right) = state.split_at_caret();
        before.set_text(left);
        after.set_text(right);
    }
    preview.append(&before);
    preview.append(&caret);
    preview.append(&after);
    body.append(&preview);

    let rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
    rows.set_valign(gtk::Align::Start);
    body.append(&rows);

    let selected = Rc::new(Cell::new((0usize, 0usize)));
    let qwertz = Rc::new(Cell::new(true));
    // Only weak references are kept in signal closures; the dialog widget
    // tree remains the sole owner of the buttons.
    let keys: Rc<RefCell<Vec<Vec<glib::WeakRef<gtk::Button>>>>> =
        Rc::new(RefCell::new(Vec::new()));
    for (row_index, labels) in ROWS.iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.set_halign(gtk::Align::Center);
        rows.append(&row);
        let mut button_refs = Vec::new();
        for (col_index, label) in labels.iter().enumerate() {
            let key = gtk::Button::with_label(label);
            key.set_height_request(42);
            key.set_width_request(if row_index == ROWS.len() - 1 { 74 } else { 51 });
            row.append(&key);
            let state = selected.clone();
            key.connect_has_focus_notify(move |button| {
                if button.has_focus() {
                    state.set((row_index, col_index));
                }
            });

            let weak_key = key.downgrade();
            let draft = draft.clone();
            let before = before.clone();
            let after = after.clone();
            let dialog = dialog.clone();
            let weak_widget = weak_widget.clone();
            let weak_parent = weak_parent.clone();
            let keys_for_layout = keys.clone();
            let qwertz = qwertz.clone();
            key.connect_clicked(move |_| {
                let Some(key) = weak_key.upgrade() else {
                    return;
                };
                let label = key.label().unwrap_or_default();
                match label.as_str() {
                    "Cancel" => {
                        dialog.close();
                    }
                    "Done" => {
                        if let Some(widget) = weak_widget.upgrade() {
                            // Refuse stale, reparented or unmapped fields.
                            if widget.is_mapped()
                                && weak_parent.as_ref().and_then(|w| w.upgrade()) == widget.parent()
                            {
                                if let Ok(editable) = widget.downcast::<gtk::Editable>() {
                                    let state = draft.borrow();
                                    editable.set_text(&state.text); // one real commit
                                    editable.set_position(state.cursor as i32);
                                }
                            }
                        }
                        dialog.close();
                    }
                    name if name.starts_with("Layout: ") => {
                        let use_qwertz = !qwertz.get();
                        qwertz.set(use_qwertz);
                        let keys = keys_for_layout.borrow();
                        if let (Some(top), Some(bottom)) = (
                            keys[1][5].upgrade(),
                            keys[3][0].upgrade(),
                        ) {
                            top.set_label(if use_qwertz { "z" } else { "y" });
                            bottom.set_label(if use_qwertz { "y" } else { "z" });
                        }
                        key.set_label(if use_qwertz {
                            "Layout: QWERTZ"
                        } else {
                            "Layout: QWERTY"
                        });
                    }
                    _ => {
                        let mut state = draft.borrow_mut();
                        match label.as_str() {
                            "Space" => state.insert(" "),
                            "Delete" => state.delete(),
                            "Clear" => {
                                state.text.clear();
                                state.cursor = 0;
                            }
                            "Left" => state.move_caret(true),
                            "Right" => state.move_caret(false),
                            letter => state.insert(letter),
                        }
                        let (left, right) = state.split_at_caret();
                        before.set_text(left);
                        after.set_text(right);
                    }
                }
            });
            button_refs.push(key.downgrade());
        }
        keys.borrow_mut().push(button_refs);
    }
    dialog.set_child(Some(&body));

    let keys_for_navigation = keys.clone();
    let dialog_for_keys = dialog.clone();
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed(move |_, key, _, modifiers| {
        if !modifiers.is_empty() {
            return glib::Propagation::Proceed;
        }
        use gtk::gdk::Key;
        let next = match key {
            Key::Left => next_key(selected.get(), Arrow::Left),
            Key::Right => next_key(selected.get(), Arrow::Right),
            Key::Up => next_key(selected.get(), Arrow::Up),
            Key::Down => next_key(selected.get(), Arrow::Down),
            Key::Back | Key::Escape | Key::Home | Key::HomePage => {
                tracing::trace!("osk prototype: dialog cancel capture");
                dialog_for_keys.close();
                return glib::Propagation::Stop;
            }
            Key::Return | Key::KP_Enter => {
                tracing::trace!("osk prototype: dialog OK propagation");
                return glib::Propagation::Proceed;
            }
            _ => return glib::Propagation::Proceed,
        };
        if let Some(button) = keys_for_navigation.borrow()[next.0][next.1].upgrade() {
            tracing::trace!("osk prototype: dialog arrow capture");
            button.grab_focus();
        }
        glib::Propagation::Stop
    });
    dialog.add_controller(controller);

    dialog.connect_closed(move |_| {
        let weak_widget = weak_widget.clone();
        let weak_window = weak_window.clone();
        glib::idle_add_local_once(move || {
            if let Some(widget) = weak_widget.upgrade() {
                if widget.is_mapped() && widget.is_sensitive() && widget.grab_focus() {
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
    let initial_key = keys.borrow()[0][0].clone();
    glib::idle_add_local_once(move || {
        if let Some(button) = initial_key.upgrade() {
            if button.is_mapped() {
                button.grab_focus();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::Draft;

    #[test]
    fn utf8_navigation_and_delete() {
        let mut draft = Draft::new("äß", 1);
        draft.insert("ö");
        assert_eq!(draft.text, "äöß");
        draft.move_caret(true);
        draft.delete();
        assert_eq!(draft.text, "öß");
        assert_eq!(draft.cursor, 0);
    }

    #[test]
    fn editing_draft_never_changes_original() {
        let original = String::from("https://example.invalid/?a=1");
        let mut draft = Draft::new(&original, 0);
        draft.insert("ä");
        assert_eq!(original, "https://example.invalid/?a=1");
        assert_eq!(draft.split_at_caret(), ("ä", original.as_str()));
    }

    #[test]
    fn horizontal_navigation_wraps_within_each_real_row() {
        use super::{next_key, Arrow, ROWS};
        assert_eq!(next_key((1, 10), Arrow::Right), (1, 0));
        assert_eq!(next_key((1, 0), Arrow::Left), (1, 10));
        assert_eq!(next_key((5, ROWS[5].len() - 1), Arrow::Right), (5, 0));
        assert_eq!(next_key((5, 0), Arrow::Left), (5, ROWS[5].len() - 1));
        assert_eq!(next_key((4, 9), Arrow::Down), (5, 7));
        assert_eq!(next_key((5, 7), Arrow::Up), (4, 7));
    }
}

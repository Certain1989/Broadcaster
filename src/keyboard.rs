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

const COLS: usize = 8;
const ROWS: &[&[&str]] = &[
    &["a", "b", "c", "d", "e", "f", "g", "h"],
    &["i", "j", "k", "l", "m", "n", "o", "p"],
    &["q", "r", "s", "t", "u", "v", "w", "x"],
    &["y", "z", "ä", "ö", "ü", "ß", "0", "1"],
    &["2", "3", "4", "5", "6", "7", "8", "9"],
    &[":", "/", "?", "&", "=", "#", "@", "."],
    &["-", "_", "+", "%", "~", "Space", "Delete", "Clear"],
    &["Left", "Right", "Done", "Cancel"],
];

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
    fn preview(&self) -> String {
        let at = self.offset();
        format!("{}│{}", &self.text[..at], &self.text[at..])
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
    dialog.set_content_width(620);
    dialog.set_content_height(490);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    let instruction = gtk::Label::new(Some(&gettext(
        "Arrows select keys, OK activates, Back cancels.",
    )));
    body.append(&instruction);
    let preview = gtk::Label::new(Some(&draft.borrow().preview()));
    preview.set_wrap(true);
    preview.set_xalign(0.0);
    body.append(&preview);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(4);
    grid.set_column_spacing(4);
    grid.set_column_homogeneous(true);
    body.append(&grid);

    let selected = Rc::new(Cell::new(0usize));
    let mut buttons = Vec::new();
    for (r, labels) in ROWS.iter().enumerate() {
        for (c, label) in labels.iter().enumerate() {
            let key = gtk::Button::with_label(label);
            key.set_height_request(40);
            key.set_hexpand(true);
            grid.attach(&key, c as i32, r as i32, 1, 1);
            let index = buttons.len();
            let state = selected.clone();
            key.connect_has_focus_notify(move |key| {
                if key.has_focus() {
                    state.set(index);
                }
            });
            let label = (*label).to_string();
            let draft = draft.clone();
            let preview = preview.clone();
            let dialog = dialog.clone();
            let weak_widget = weak_widget.clone();
            let weak_parent = weak_parent.clone();
            key.connect_clicked(move |_| {
                match label.as_str() {
                    "Cancel" => {
                        dialog.close();
                    }
                    "Done" => {
                        if let Some(widget) = weak_widget.upgrade() {
                            // Refuse a stale, reparented or unmapped entry.
                            if widget.is_mapped()
                                && weak_parent.as_ref().and_then(|w| w.upgrade()) == widget.parent()
                            {
                                if let Ok(editable) = widget.downcast::<gtk::Editable>() {
                                    let state = draft.borrow();
                                    editable.set_text(&state.text); // exactly one real commit
                                    editable.set_position(state.cursor as i32);
                                }
                            }
                        }
                        dialog.close();
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
                            literal => state.insert(literal),
                        }
                        preview.set_text(&state.preview());
                    }
                }
            });
            buttons.push(key);
        }
    }
    dialog.set_child(Some(&body));

    let buttons = Rc::new(buttons);
    let keys = buttons.clone();
    let dialog_for_keys = dialog.clone();
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed(move |_, key, _, modifiers| {
        if !modifiers.is_empty() {
            return glib::Propagation::Proceed;
        }
        use gtk::gdk::Key;
        let col = selected.get() % COLS;
        let row = selected.get() / COLS;
        let index = match key {
            Key::Left => row * COLS + col.saturating_sub(1),
            Key::Right => row * COLS + (col + 1).min(COLS - 1),
            Key::Up => row.saturating_sub(1) * COLS + col,
            Key::Down => (row + 1).min(ROWS.len() - 1) * COLS + col,
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
        if let Some(button) = keys.get(index.min(keys.len() - 1)) {
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
    let first = buttons[0].clone();
    glib::idle_add_local_once(move || {
        if first.is_mapped() {
            first.grab_focus();
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
        assert_eq!(draft.preview(), "ä│https://example.invalid/?a=1");
    }
}

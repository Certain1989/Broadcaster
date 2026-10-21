// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::RefCell;

use adw::{glib, gtk, prelude::*, subclass::prelude::*};
use gettextrs::gettext;

use crate::settings::TvSettings;

// D-pad movement is independent of list selection (which is disabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerticalDestination {
    Row(i32),
    RestoreDefaults,
    AddArgument,
}

fn vertical_destination(
    row_index: i32,
    has_next_row: bool,
    key: gtk::gdk::Key,
) -> Option<VerticalDestination> {
    if row_index < 0 {
        return None;
    }
    if key == gtk::gdk::Key::Up || key == gtk::gdk::Key::KP_Up {
        return Some(if row_index == 0 {
            VerticalDestination::RestoreDefaults
        } else {
            VerticalDestination::Row(row_index - 1)
        });
    }
    if key == gtk::gdk::Key::Down || key == gtk::gdk::Key::KP_Down {
        return Some(if has_next_row {
            VerticalDestination::Row(row_index + 1)
        } else {
            VerticalDestination::AddArgument
        });
    }
    None
}

fn unmodified_arrow(modifiers: gtk::gdk::ModifierType) -> bool {
    !modifiers.intersects(
        gtk::gdk::ModifierType::SHIFT_MASK
            | gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::ALT_MASK
            | gtk::gdk::ModifierType::SUPER_MASK
            | gtk::gdk::ModifierType::META_MASK,
    )
}

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate, glib::Properties)]
    #[template(file = "src/preferences/mpv_arguments.blp")]
    #[properties(wrapper_type = super::TvMpvArgumentsPage)]
    pub struct TvMpvArgumentsPage {
        #[template_child]
        arguments_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        arguments_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        restore_defaults_button: TemplateChild<gtk::Button>,
        #[template_child]
        add_argument_button: TemplateChild<gtk::Button>,

        /// The settings key holding the edited arguments.
        #[property(get, set, construct_only)]
        key: RefCell<String>,

        settings: TvSettings,
    }

    #[gtk::template_callbacks]
    impl TvMpvArgumentsPage {
        #[template_callback]
        fn add_argument(&self, #[rest] _: &[glib::Value]) {
            self.append_row("").grab_focus();
        }
        #[template_callback]
        fn restore_defaults(&self, #[rest] _: &[glib::Value]) {
            let previous = self.arguments();
            self.settings.reset(&self.key.borrow());
            self.load();
            self.offer_undo(&gettext("Defaults restored"), previous);
        }
    }

    impl TvMpvArgumentsPage {
        fn arguments(&self) -> Vec<String> {
            self.settings
                .strv(&self.key.borrow())
                .iter()
                .map(ToString::to_string)
                .collect()
        }

        /// Shows a toast whose button brings back the `previous` arguments.
        fn offer_undo(&self, title: &str, previous: Vec<String>) {
            let Some(dialog) = self.obj().ancestor(adw::PreferencesDialog::static_type()) else {
                return;
            };
            let toast = adw::Toast::builder()
                .title(title)
                .button_label(gettext("_Undo"))
                .build();
            toast.connect_button_clicked(glib::clone!(
                #[weak(rename_to = slf)]
                self,
                move |_| {
                    if slf
                        .settings
                        .set_strv(&slf.key.borrow(), previous.as_slice())
                        .is_err()
                    {
                        tracing::error!("Failed to restore mpv arguments");
                    }
                    slf.load();
                    slf.focus_nearest_row_or_add(0);
                }
            ));
            dialog
                .downcast_ref::<adw::PreferencesDialog>()
                .expect("is a PreferencesDialog")
                .add_toast(toast);
        }

        fn load(&self) {
            self.arguments_list.remove_all();
            for argument in self.settings.strv(&self.key.borrow()) {
                self.append_row(&argument);
            }
        }

        fn save(&self) {
            let mut arguments = Vec::new();
            let mut child = self.arguments_list.first_child();
            while let Some(row) = child {
                if let Some(row) = row.downcast_ref::<adw::EntryRow>() {
                    let text = row.text();
                    let argument = text.trim();
                    if !argument.is_empty() {
                        arguments.push(argument.to_owned());
                    }
                }
                child = row.next_sibling();
            }

            if self
                .settings
                .set_strv(&self.key.borrow(), arguments.as_slice())
                .is_err()
            {
                tracing::error!("Failed to save mpv arguments");
            }
        }

        // A header control is always a visible escape from the first/last row.
        // Down from either header button returns to the first editable row.
        fn install_header_navigation(&self) {
            for button in [
                self.restore_defaults_button.get(),
                self.add_argument_button.get(),
            ] {
                let controller = gtk::EventControllerKey::new();
                controller.set_propagation_phase(gtk::PropagationPhase::Capture);
                controller.connect_key_pressed(glib::clone!(
                    #[weak(rename_to = slf)]
                    self,
                    #[upgrade_or]
                    glib::Propagation::Proceed,
                    move |_, key, _, modifiers| {
                        if unmodified_arrow(modifiers)
                            && (key == gtk::gdk::Key::Down || key == gtk::gdk::Key::KP_Down)
                        {
                            if let Some(first) = slf.arguments_list.row_at_index(0) {
                                if first
                                    .downcast::<adw::EntryRow>()
                                    .is_ok_and(|row| row.grab_focus_without_selecting())
                                {
                                    return glib::Propagation::Stop;
                                }
                            }
                        }
                        glib::Propagation::Proceed
                    }
                ));
                button.add_controller(controller);
            }
        }

        fn focus_vertical(&self, row: &adw::EntryRow, key: gtk::gdk::Key) -> bool {
            let index = row.index();
            let destination = vertical_destination(
                index,
                self.arguments_list.row_at_index(index + 1).is_some(),
                key,
            );
            match destination {
                Some(VerticalDestination::Row(index)) => self
                    .arguments_list
                    .row_at_index(index)
                    .and_then(|target| target.downcast::<adw::EntryRow>().ok())
                    .is_some_and(|target| target.grab_focus_without_selecting()),
                Some(VerticalDestination::RestoreDefaults) => {
                    self.restore_defaults_button.grab_focus()
                }
                Some(VerticalDestination::AddArgument) => self.add_argument_button.grab_focus(),
                None => false,
            }
        }

        fn focus_nearest_row_or_add(&self, index: i32) {
            let focused = self
                .arguments_list
                .row_at_index(index)
                .or_else(|| self.arguments_list.row_at_index(index.saturating_sub(1)))
                .and_then(|row| row.downcast::<adw::EntryRow>().ok())
                .is_some_and(|row| row.grab_focus_without_selecting());
            if !focused {
                self.add_argument_button.grab_focus();
            }
        }

        fn append_row(&self, argument: &str) -> adw::EntryRow {
            let row = adw::EntryRow::builder()
                .text(argument)
                .css_classes(["monospace"])
                .build();

            let remove_button = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(gettext("Remove Argument"))
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            remove_button.connect_clicked(glib::clone!(
                #[weak(rename_to = slf)]
                self,
                #[weak]
                row,
                move |_| {
                    let previous = slf.arguments();
                    let index = row.index();
                    slf.arguments_list.remove(&row);
                    slf.save();
                    slf.focus_nearest_row_or_add(index);
                    slf.offer_undo(&gettext("Argument removed"), previous);
                }
            ));
            // Bind to each newly constructed EntryRow, including rows rebuilt
            // by Restore Defaults and Undo. Its existing changed handler is
            // the sole immediate GSettings writer, not an OSK Save action.
            let keyboard = gtk::Button::new();
            keyboard.set_icon_name("input-keyboard-symbolic");
            keyboard.set_size_request(48, 44);
            keyboard.set_valign(gtk::Align::Center);
            keyboard.add_css_class("flat");
            row.add_suffix(&keyboard);
            crate::keyboard::bind_launcher(
                &keyboard,
                &row,
                crate::keyboard::KeyboardOptions::default(),
            );
            row.add_suffix(&remove_button);

            // A remote cannot Tab out of EntryRow's native GtkEditable.
            // Use vertical steps within the row: Up from the editor reaches
            // its own keyboard suffix, then Remove, then the preceding row
            // (or Restore Defaults). Down reverses these steps, except that
            // Down from an editor proceeds to the following row/Add. Keep
            // Left/Right entirely native for physical text-caret movement.
            // When the OSK is disabled, skip its hidden suffix altogether.
            let controller = gtk::EventControllerKey::new();
            controller.set_propagation_phase(gtk::PropagationPhase::Capture);
            controller.connect_key_pressed(glib::clone!(
                #[weak(rename_to = slf)]
                self,
                #[weak]
                row,
                #[weak]
                keyboard,
                #[weak]
                remove_button,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, key, _, modifiers| {
                    if !unmodified_arrow(modifiers) {
                        return glib::Propagation::Proceed;
                    }
                    let up = matches!(key, gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up);
                    let down = matches!(key, gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down);
                    if !up && !down {
                        return glib::Propagation::Proceed;
                    }
                    let keyboard_live = keyboard.is_mapped() && keyboard.is_sensitive();
                    let handled = if up {
                        if remove_button.has_focus() {
                            slf.focus_vertical(&row, key)
                        } else if keyboard.has_focus() {
                            remove_button.grab_focus()
                        } else {
                            (keyboard_live && keyboard.grab_focus()) || remove_button.grab_focus()
                        }
                    } else if remove_button.has_focus() {
                        (keyboard_live && keyboard.grab_focus())
                            || row.grab_focus_without_selecting()
                    } else if keyboard.has_focus() {
                        row.grab_focus_without_selecting()
                    } else {
                        slf.focus_vertical(&row, key)
                    };
                    if handled {
                        glib::Propagation::Stop
                    } else {
                        glib::Propagation::Proceed
                    }
                }
            ));
            row.add_controller(controller);

            row.connect_changed(glib::clone!(
                #[weak(rename_to = slf)]
                self,
                move |_| slf.save()
            ));

            self.arguments_list.append(&row);
            row
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvMpvArgumentsPage {
        const NAME: &'static str = "TvMpvArgumentsPage";
        type Type = super::TvMpvArgumentsPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for TvMpvArgumentsPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.load();
            self.install_header_navigation();
        }
    }
    impl WidgetImpl for TvMpvArgumentsPage {}
    impl NavigationPageImpl for TvMpvArgumentsPage {}
}

glib::wrapper! {
    pub struct TvMpvArgumentsPage(ObjectSubclass<imp::TvMpvArgumentsPage>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

pub const GENERAL_KEY: &str = "mpv-arguments";

impl TvMpvArgumentsPage {
    /// Creates a page editing the string list in the settings key `key`.
    pub fn new(key: &str) -> Self {
        glib::Object::builder().property("key", key).build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Send actual XTest keyboard events into the mapped GTK native surface.
    // Emitting EventControllerKey's signal directly would bypass capture
    // routing and would not establish that remote-only navigation works.
    fn native_key(key: &str) {
        let script = r#"
import ctypes as c
import sys
x = c.CDLL('libX11.so.6')
t = c.CDLL('libXtst.so.6')
x.XOpenDisplay.argtypes = [c.c_char_p]
x.XOpenDisplay.restype = c.c_void_p
d = x.XOpenDisplay(None)
assert d, 'X11 display missing'
x.XDefaultRootWindow.argtypes = [c.c_void_p]
x.XDefaultRootWindow.restype = c.c_ulong
x.XQueryTree.argtypes = [
    c.c_void_p, c.c_ulong, c.POINTER(c.c_ulong),
    c.POINTER(c.c_ulong), c.POINTER(c.POINTER(c.c_ulong)),
    c.POINTER(c.c_uint)]
x.XQueryTree.restype = c.c_int
root = x.XDefaultRootWindow(d)
returned_root, parent, count = c.c_ulong(), c.c_ulong(), c.c_uint()
children = c.POINTER(c.c_ulong)()
assert x.XQueryTree(d, root, c.byref(returned_root), c.byref(parent),
                    c.byref(children), c.byref(count))
assert count.value > 0, 'no mapped GTK X11 window'
x.XSetInputFocus.argtypes = [c.c_void_p, c.c_ulong, c.c_int, c.c_ulong]
x.XSetInputFocus(d, children[count.value - 1], 1, 0)
x.XFree.argtypes = [c.c_void_p]
x.XFree(c.cast(children, c.c_void_p))
x.XStringToKeysym.argtypes = [c.c_char_p]
x.XStringToKeysym.restype = c.c_ulong
x.XKeysymToKeycode.argtypes = [c.c_void_p, c.c_ulong]
x.XKeysymToKeycode.restype = c.c_uint
symbol = x.XStringToKeysym(sys.argv[1].encode())
code = x.XKeysymToKeycode(d, symbol)
assert code, 'invalid key'
t.XTestFakeKeyEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
assert t.XTestFakeKeyEvent(d, code, 1, 0)
assert t.XTestFakeKeyEvent(d, code, 0, 0)
x.XFlush.argtypes = [c.c_void_p]
x.XFlush(d)
x.XCloseDisplay.argtypes = [c.c_void_p]
x.XCloseDisplay(d)
"#;
        let result = std::process::Command::new("python3")
            .args(["-c", script, key])
            .output()
            .expect("Python XTest keyboard driver");
        assert!(
            result.status.success(),
            "native {key} injection: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        mapped_spin();
    }

    fn mapped_spin() {
        for _ in 0..12 {
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn find_listbox(widget: &gtk::Widget) -> Option<gtk::ListBox> {
        if let Some(list) = widget.downcast_ref::<gtk::ListBox>() {
            return Some(list.clone());
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Some(found) = find_listbox(&w) {
                return Some(found);
            }
            child = w.next_sibling();
        }
        None
    }

    fn find_icon_button(widget: &gtk::Widget, icon: &str) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.icon_name().as_deref() == Some(icon) {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Some(found) = find_icon_button(&w, icon) {
                return Some(found);
            }
            child = w.next_sibling();
        }
        None
    }

    fn row_owns_focus(win: &adw::Window, row: &adw::EntryRow) -> bool {
        gtk::prelude::GtkWindowExt::focus(win).is_some_and(|focused| {
            focused.is_ancestor(row) || focused == row.clone().upcast::<gtk::Widget>()
        })
    }

    fn find_draft(widget: &gtk::Widget) -> Option<gtk::Entry> {
        if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
            return Some(entry.clone());
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Some(found) = find_draft(&w) {
                return Some(found);
            }
            child = w.next_sibling();
        }
        None
    }

    fn find_labeled_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Some(found) = find_labeled_button(&w, label) {
                return Some(found);
            }
            child = w.next_sibling();
        }
        None
    }

    #[test]
    #[ignore = "requires mapped GTK/Xvfb, XTest and in-memory GSettings"]
    fn osk_mapped_gtk_mpv_row_native_remote_route_and_lifecycle() {
        use std::{cell::Cell, rc::Rc};

        adw::init().expect("GTK display");
        let resources =
            adw::gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
                .expect("bundled resources");
        adw::gio::resources_register(&resources);

        let settings = TvSettings::get();
        settings
            .set_boolean("osk-enabled", true)
            .expect("enable OSK");
        settings
            .set_strv(GENERAL_KEY, ["--first=1", "--second=2"])
            .expect("synthetic settings");
        let changed = Rc::new(Cell::new(0));
        settings.connect_changed(Some(GENERAL_KEY), {
            let changed = changed.clone();
            move |_, _| changed.set(changed.get() + 1)
        });

        let win = adw::Window::new();
        win.set_default_size(960, 720);
        win.present();
        let dialog = adw::PreferencesDialog::new();
        let page = TvMpvArgumentsPage::new(GENERAL_KEY);
        dialog.push_subpage(&page);
        dialog.present(Some(&win));
        mapped_spin();

        let list = find_listbox(page.upcast_ref()).expect("actual mpv argument list");
        let first = list
            .row_at_index(0)
            .expect("row 0")
            .downcast::<adw::EntryRow>()
            .expect("editable row 0");
        let second = list
            .row_at_index(1)
            .expect("row 1")
            .downcast::<adw::EntryRow>()
            .expect("editable row 1");
        let keyboard0 = find_icon_button(first.upcast_ref(), "input-keyboard-symbolic")
            .expect("first live keyboard suffix");
        let keyboard1 = find_icon_button(second.upcast_ref(), "input-keyboard-symbolic")
            .expect("second live keyboard suffix");
        let remove0 = find_icon_button(first.upcast_ref(), "user-trash-symbolic")
            .expect("first Remove suffix");
        let restore = find_icon_button(page.upcast_ref(), "edit-undo-symbolic")
            .expect("Restore Defaults header");
        let add =
            find_icon_button(page.upcast_ref(), "list-add-symbolic").expect("Add Argument header");

        assert!(first.grab_focus_without_selecting());
        first.set_position(4);
        native_key("Left");
        assert_eq!(first.position(), 3, "native Left edits caret, not focus");
        native_key("Up");
        assert!(keyboard0.has_focus(), "first editor Up reaches own suffix");
        native_key("Return");
        let osk = win.visible_dialog().expect("OK opens real nested OSK");
        let draft = find_draft(osk.upcast_ref()).expect("OSK draft entry");
        assert_eq!(draft.text(), "--first=1");
        draft.set_text("--first=changed");
        let done = find_labeled_button(osk.upcast_ref(), "Done").expect("Done");
        done.emit_by_name::<()>("clicked", &[]);
        mapped_spin();
        assert_eq!(changed.get(), 1, "one GSettings update on Done");
        assert_eq!(settings.strv(GENERAL_KEY)[0].as_str(), "--first=changed");

        assert!(first.grab_focus_without_selecting());
        native_key("Up");
        assert!(keyboard0.has_focus());
        native_key("Up");
        assert!(remove0.has_focus(), "second Up reaches Remove");
        native_key("Up");
        assert!(restore.has_focus(), "first row can escape to header");
        native_key("Down");
        assert!(
            row_owns_focus(&win, &first),
            "header Down returns to first editor"
        );
        native_key("Up");
        assert!(keyboard0.has_focus(), "editor Up returns to its keyboard");
        native_key("Down");
        assert!(
            row_owns_focus(&win, &first),
            "keyboard Down returns to row editor"
        );

        native_key("Down");
        assert!(row_owns_focus(&win, &second), "editor Down visits next row");
        native_key("Up");
        assert!(keyboard1.has_focus(), "second row has distinct keyboard");
        native_key("Return");
        let osk = win.visible_dialog().expect("second row OSK");
        find_draft(osk.upcast_ref())
            .expect("second draft")
            .set_text("cancelled");
        find_labeled_button(osk.upcast_ref(), "Cancel")
            .expect("Cancel")
            .emit_by_name::<()>("clicked", &[]);
        mapped_spin();
        assert_eq!(changed.get(), 1, "Cancel cannot write settings");
        assert_eq!(settings.strv(GENERAL_KEY)[1].as_str(), "--second=2");

        assert!(second.grab_focus_without_selecting());
        native_key("Down");
        assert!(add.has_focus(), "last row Down reaches Add");

        // Turning off the keyboard must eliminate, not trap, its destination.
        settings
            .set_boolean("osk-enabled", false)
            .expect("disable OSK");
        mapped_spin();
        assert!(!keyboard0.is_visible() && !keyboard1.is_visible());
        assert!(first.grab_focus_without_selecting());
        native_key("Up");
        assert!(remove0.has_focus(), "OFF skips hidden keyboard button");
        native_key("Down");
        assert!(
            row_owns_focus(&win, &first),
            "OFF retains editor and Remove route"
        );
        settings
            .set_boolean("osk-enabled", true)
            .expect("re-enable OSK");
        mapped_spin();

        // Removing a row during a draft must never resurrect its GSettings
        // value. This also exercises the real remove/rebuild code path.
        assert!(second.grab_focus_without_selecting());
        native_key("Up");
        assert!(keyboard1.has_focus());
        native_key("Return");
        let osk = win.visible_dialog().expect("second OSK before removal");
        find_draft(osk.upcast_ref())
            .expect("draft")
            .set_text("stale resurrection");
        let remove1 =
            find_icon_button(second.upcast_ref(), "user-trash-symbolic").expect("live row removal");
        remove1.emit_by_name::<()>("clicked", &[]);
        mapped_spin();
        let after_remove = settings.strv(GENERAL_KEY);
        assert_eq!(after_remove.len(), 1);
        if osk.is_mapped() {
            find_labeled_button(osk.upcast_ref(), "Done")
                .expect("Done")
                .emit_by_name::<()>("clicked", &[]);
        }
        mapped_spin();
        assert_eq!(
            settings.strv(GENERAL_KEY).as_slice(),
            after_remove.as_slice(),
            "no ghost save"
        );

        restore.emit_by_name::<()>("clicked", &[]);
        mapped_spin();
        let rebuilt = list
            .row_at_index(0)
            .expect("Restore rebuilds rows")
            .downcast::<adw::EntryRow>()
            .expect("restored editable");
        let rebuilt_keyboard = find_icon_button(rebuilt.upcast_ref(), "input-keyboard-symbolic")
            .expect("rebuilt row keyboard");
        assert!(rebuilt.grab_focus_without_selecting());
        native_key("Up");
        assert!(
            rebuilt_keyboard.has_focus(),
            "restored row has live routing"
        );
        dialog.close();
        win.destroy();
    }

    #[test]
    fn dpad_rows_and_header_boundaries() {
        use VerticalDestination::{AddArgument, RestoreDefaults, Row};

        assert_eq!(
            vertical_destination(0, true, gtk::gdk::Key::Up),
            Some(RestoreDefaults)
        );
        assert_eq!(
            vertical_destination(0, true, gtk::gdk::Key::Down),
            Some(Row(1))
        );
        assert_eq!(
            vertical_destination(1, true, gtk::gdk::Key::Up),
            Some(Row(0))
        );
        assert_eq!(
            vertical_destination(1, true, gtk::gdk::Key::Down),
            Some(Row(2))
        );
        assert_eq!(
            vertical_destination(2, false, gtk::gdk::Key::Down),
            Some(AddArgument)
        );
        assert_eq!(
            vertical_destination(0, false, gtk::gdk::Key::Up),
            Some(RestoreDefaults)
        );
        assert_eq!(
            vertical_destination(0, false, gtk::gdk::Key::Down),
            Some(AddArgument)
        );
        assert_eq!(vertical_destination(-1, false, gtk::gdk::Key::Down), None);
        assert_eq!(vertical_destination(0, true, gtk::gdk::Key::Left), None);
        assert_eq!(vertical_destination(0, true, gtk::gdk::Key::Right), None);
        assert_eq!(
            vertical_destination(0, true, gtk::gdk::Key::KP_Up),
            Some(RestoreDefaults)
        );
        assert_eq!(
            vertical_destination(0, true, gtk::gdk::Key::KP_Down),
            Some(Row(1))
        );
    }

    #[test]
    fn modified_arrows_are_reserved_for_native_editing() {
        assert!(unmodified_arrow(gtk::gdk::ModifierType::empty()));
        for modifier in [
            gtk::gdk::ModifierType::SHIFT_MASK,
            gtk::gdk::ModifierType::CONTROL_MASK,
            gtk::gdk::ModifierType::ALT_MASK,
            gtk::gdk::ModifierType::SUPER_MASK,
            gtk::gdk::ModifierType::META_MASK,
        ] {
            assert!(!unmodified_arrow(modifier));
        }
    }
}

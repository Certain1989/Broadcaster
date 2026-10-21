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

            // Capture on the row so Up/Down also work from its embedded
            // GtkEditable and from suffix buttons, without changing their
            // Left/Right, typing or activation handling.
            let controller = gtk::EventControllerKey::new();
            controller.set_propagation_phase(gtk::PropagationPhase::Capture);
            controller.connect_key_pressed(glib::clone!(
                #[weak(rename_to = slf)]
                self,
                #[weak]
                row,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, key, _, modifiers| {
                    if unmodified_arrow(modifiers) && slf.focus_vertical(&row, key) {
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

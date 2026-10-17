// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::RefCell;

use adw::{glib, gtk, prelude::*, subclass::prelude::*};
use gettextrs::gettext;

use crate::settings::TvSettings;

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
                    slf.arguments_list.remove(&row);
                    slf.save();
                    slf.offer_undo(&gettext("Argument removed"), previous);
                }
            ));
            row.add_suffix(&remove_button);

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

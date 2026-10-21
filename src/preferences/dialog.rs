// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use adw::{glib, gtk, prelude::*, subclass::prelude::*};

use gettextrs::gettext;

use crate::{
    launcher::{ExternalProgramType, ProgramSelector},
    mpv,
    settings::TvSettings,
};

use super::{
    live::TvLiveChannelSelector,
    mpv_arguments::{self, TvMpvArgumentsPage},
};

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate, glib::Properties)]
    #[template(file = "src/preferences/dialog.blp")]
    #[properties(wrapper_type = super::TvPreferencesDialog)]
    pub struct TvPreferencesDialog {
        #[template_child]
        video_player_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        video_downloader_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        osk_enabled_row: TemplateChild<adw::SwitchRow>,

        #[property(get, set)]
        use_external_player: Cell<bool>,
        #[property(get)]
        video_player_display_name: RefCell<String>,
        #[property(get)]
        video_downloader_display_name: RefCell<String>,

        #[property(get)]
        mpv_missing: Cell<bool>,
        #[property(get)]
        mpv_arguments_summary: RefCell<String>,

        // Internal settings handle, not a public GObject property.
        settings: TvSettings,
        settings_handlers: RefCell<Vec<glib::SignalHandlerId>>,
    }

    #[gtk::template_callbacks]
    impl TvPreferencesDialog {
        #[template_callback]
        async fn select_video_player(&self, #[rest] _: &[glib::Value]) {
            if let Some(player) = ProgramSelector::select_program(
                ExternalProgramType::Player,
                self.settings.video_player_id(),
            )
            .await
            {
                self.settings.set_video_player_name(&player.name);
                self.settings.set_video_player_id(&player.id);
            }
        }
        #[template_callback]
        async fn select_video_downloader(&self, #[rest] _: &[glib::Value]) {
            if let Some(downloader) = ProgramSelector::select_program(
                ExternalProgramType::Downloader,
                self.settings.video_downloader_id(),
            )
            .await
            {
                self.settings.set_video_downloader_name(&downloader.name);
                self.settings.set_video_downloader_id(&downloader.id);
            }
        }
        #[template_callback]
        async fn select_live_channels(&self, #[rest] _: &[glib::Value]) {
            self.obj().push_subpage(&TvLiveChannelSelector::new())
        }
        #[template_callback]
        fn edit_mpv_arguments(&self, #[rest] _: &[glib::Value]) {
            self.obj()
                .push_subpage(&TvMpvArgumentsPage::new(mpv_arguments::GENERAL_KEY))
        }
    }

    impl TvPreferencesDialog {
        fn update_video_player_display_name(&self) {
            let name = self.settings.video_player_name();
            let id = self.settings.video_player_id();

            *self.video_player_display_name.borrow_mut() = if name.is_empty() {
                id
            } else {
                format!("{name} ({id})",)
            };

            self.obj().notify_video_player_display_name();
        }
        fn update_video_downloader_display_name(&self) {
            let name = self.settings.video_downloader_name();
            let id = self.settings.video_downloader_id();

            *self.video_downloader_display_name.borrow_mut() = if name.is_empty() {
                id
            } else {
                format!("{name} ({id})",)
            };

            self.obj().notify_video_downloader_display_name();
        }
        fn update_mpv_arguments_summary(&self) {
            *self.mpv_arguments_summary.borrow_mut() =
                arguments_summary(self.settings.mpv_arguments());
            self.obj().notify_mpv_arguments_summary();
        }
    }

    fn arguments_summary(arguments: Vec<String>) -> String {
        if arguments.is_empty() {
            gettext("No arguments")
        } else {
            arguments.join(" ")
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvPreferencesDialog {
        const NAME: &'static str = "TvPreferencesDialog";
        type Type = super::TvPreferencesDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for TvPreferencesDialog {
        fn constructed(&self) {
            self.parent_constructed();

            self.settings
                .bind_use_external_player(&*self.obj(), "use-external-player")
                .build();

            self.settings
                .bind_osk_enabled(&*self.osk_enabled_row, "active")
                .build();

            self.mpv_missing.set(!mpv::is_installed());

            self.update_mpv_arguments_summary();
            self.update_video_player_display_name();
            self.update_video_downloader_display_name();

            let settings = &self.settings;
            self.settings_handlers.replace(vec![
                settings.connect_mpv_arguments_changed(glib::clone!(
                    #[weak(rename_to = slf)]
                    self,
                    move |_| slf.update_mpv_arguments_summary()
                )),
                settings.connect_video_player_name_changed(glib::clone!(
                    #[weak(rename_to = slf)]
                    self,
                    move |_| slf.update_video_player_display_name()
                )),
                settings.connect_video_player_id_changed(glib::clone!(
                    #[weak(rename_to = slf)]
                    self,
                    move |_| slf.update_video_player_display_name()
                )),
                settings.connect_video_downloader_name_changed(glib::clone!(
                    #[weak(rename_to = slf)]
                    self,
                    move |_| slf.update_video_downloader_display_name()
                )),
                settings.connect_video_downloader_id_changed(glib::clone!(
                    #[weak(rename_to = slf)]
                    self,
                    move |_| slf.update_video_downloader_display_name()
                )),
            ]);
        }

        fn dispose(&self) {
            // the settings object outlives the dialog
            for handler in self.settings_handlers.take() {
                self.settings.disconnect(handler);
            }
        }
    }
    impl WidgetImpl for TvPreferencesDialog {}
    impl AdwDialogImpl for TvPreferencesDialog {}
    impl PreferencesDialogImpl for TvPreferencesDialog {}
}

glib::wrapper! {
    pub struct TvPreferencesDialog(ObjectSubclass<imp::TvPreferencesDialog>)
        @extends gtk::Widget, adw::Dialog, adw::PreferencesDialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TvPreferencesDialog {
    pub fn new() -> Self {
        glib::Object::new()
    }
}

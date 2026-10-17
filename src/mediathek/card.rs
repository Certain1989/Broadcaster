// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use adw::{gio, glib, gtk, prelude::*, subclass::prelude::*};
use gettextrs::gettext;

use crate::{
    application::{TvApplication, VideoInfo},
    channel_icons::load_channel_icon,
    settings::VideoQuality,
    utils::{show_error, spawn},
};

use super::shows::ShowObject;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate, glib::Properties)]
    #[template(file = "src/mediathek/card.blp")]
    #[properties(wrapper_type=super::TvMediathekCard)]
    pub struct TvMediathekCard {
        #[template_child]
        icon: TemplateChild<gtk::Image>,
        #[template_child]
        fallback_icon: TemplateChild<gtk::Label>,
        #[template_child]
        revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        play_button: TemplateChild<adw::SplitButton>,
        #[template_child]
        copy_url_button: TemplateChild<gtk::MenuButton>,

        #[property(get, construct_only)]
        show: RefCell<Option<ShowObject>>,

        #[property(get, set)]
        expanded: Cell<bool>,
    }
    impl TvMediathekCard {
        fn set_icon(&self) {
            if let Some(show) = self.obj().show() {
                self.icon
                    .update_property(&[gtk::accessible::Property::Label(&show.channel())]);
            }
            load_channel_icon(
                self.obj().show().map(|c| c.channel()).as_deref(),
                &self.icon,
                &self.fallback_icon,
                64,
            )
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvMediathekCard {
        const NAME: &'static str = "TvMediathekCard";
        type Type = super::TvMediathekCard;
        type ParentType = gtk::ListBoxRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for TvMediathekCard {
        fn constructed(&self) {
            self.parent_constructed();

            self.obj().setup_actions();

            // Result rows are generated asynchronously after the window was
            // constructed. Register grab-root handlers on *their own* menu
            // popovers; the window's one-time widget-tree scan cannot see them.
            // Also observe replacement popovers to cover lazy construction.
            let play = self.play_button.clone();
            if let Some(popover) = play.popover() {
                super::install_card_popover_back(&popover, play.upcast_ref());
            }
            play.connect_notify_local(Some("popover"), |button, _| {
                if let Some(popover) = button.popover() {
                    super::install_card_popover_back(&popover, button.upcast_ref());
                }
            });
            let copy = self.copy_url_button.clone();
            if let Some(popover) = copy.popover() {
                super::install_card_popover_back(&popover, copy.upcast_ref());
            }
            copy.connect_notify_local(Some("popover"), |button, _| {
                if let Some(popover) = button.popover() {
                    super::install_card_popover_back(&popover, button.upcast_ref());
                }
            });

            self.obj().connect_show_notify(|slf| slf.imp().set_icon());

            // tell assistive technologies that the row expands
            let update_expanded = |slf: &super::TvMediathekCard| {
                slf.update_state(&[gtk::accessible::State::Expanded(Some(slf.expanded()))])
            };
            update_expanded(&self.obj());
            self.obj().connect_expanded_notify(update_expanded);

            // After revealing a row, take the remote straight to Play rather
            // than leaving it on the row with no directional path to the action.
            self.obj().connect_expanded_notify(|row| {
                if row.expanded() {
                    // A hidden revealer cannot expose keyboard-focusable actions.
                    row.imp().revealer.set_visible(true);
                    let row = row.downgrade();
                    glib::idle_add_local_once(move || {
                        if let Some(row) = row.upgrade() {
                            if row.expanded() {
                                row.imp().play_button.grab_focus();
                            }
                        }
                    });
                }
            });

            // Back out of an expanded card without triggering window dismissal.
            // A menu popover gets first chance to consume Escape itself.
            let key_controller = gtk::EventControllerKey::new();
            let weak_row = self.obj().downgrade();
            key_controller.connect_key_pressed(move |_, key, _, modifiers| {
                let Some(row) = weak_row.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if row.expanded()
                    && modifiers.is_empty()
                    && (key == gtk::gdk::Key::Escape
                        || key == gtk::gdk::Key::BackSpace
                        || key == gtk::gdk::Key::Back)
                {
                    row.set_expanded(false);
                    row.grab_focus();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            self.obj().add_controller(key_controller);

            self.revealer.connect_child_revealed_notify(|revealer| {
                revealer.set_visible(revealer.is_child_revealed())
            });
        }
    }
    impl WidgetImpl for TvMediathekCard {}
    impl ListBoxRowImpl for TvMediathekCard {}
}

glib::wrapper! {
    pub struct TvMediathekCard(ObjectSubclass<imp::TvMediathekCard>)
        @extends gtk::Widget, gtk::ListBoxRow,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

/// A modal GTK popover owns the keyboard grab: Back must be caught at
/// that root, before a highlighted quality action can be activated.
fn install_card_popover_back(popover: &gtk::Popover, opener: &gtk::Widget) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_popover = popover.downgrade();
    let weak_opener = opener.downgrade();
    controller.connect_key_pressed(move |_, key, _, modifiers| {
        if modifiers.is_empty() && matches!(key, gtk::gdk::Key::Back | gtk::gdk::Key::Escape) {
            if let Some(popover) = weak_popover.upgrade() {
                popover.popdown();
            }
            if let Some(opener) = weak_opener.upgrade() {
                opener.grab_focus();
            }
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    popover.add_controller(controller);
}

impl TvMediathekCard {
    fn has_playable_stream(&self) -> bool {
        self.show().is_some_and(|show| {
            [VideoQuality::High, VideoQuality::Medium, VideoQuality::Low]
                .into_iter()
                .any(|quality| show.video_url(quality).is_some_and(|url| !url.is_empty()))
        })
    }

    fn play(&self, quality: VideoQuality) {
        let show = self
            .show()
            .expect("action must only be enabled if show is not None");

        spawn(async move {
            TvApplication::get()
                .play(VideoInfo::Mediathek {
                    title: show.title(),
                    preferred_quality: quality,
                    uri_high: show.video_url_high(),
                    uri_medium: show.video_url_medium(),
                    uri_low: show.video_url_low(),
                    subtitle_url: show.subtitle_url(),
                })
                .await
        });
    }
    fn copy_video_url(&self, quality: VideoQuality) {
        self.clipboard().set(
            &self
                .show()
                .and_then(|show| show.video_url(quality))
                .expect("action must only be enabled if url is not None"),
        );

        TvApplication::get()
            .window()
            .add_toast(adw::Toast::new(&gettext("Copied video URL to clipboard")));
    }
    fn copy_subtitles_url(&self) {
        self.clipboard().set(
            &self
                .show()
                .and_then(|show| show.subtitle_url())
                .expect("action must only be enabled if url is not None"),
        );

        TvApplication::get()
            .window()
            .add_toast(adw::Toast::new(&gettext(
                "Copied subtitles URL to clipboard",
            )));
    }
    fn download(&self) {
        self.activate_action(
            "app.download",
            Some(
                &self
                    .show()
                    .and_then(|show| show.website_url())
                    .expect("action must only be enabled if url is not None")
                    .to_variant(),
            ),
        )
        .unwrap()
    }
    fn setup_actions(&self) {
        let actions = gio::SimpleActionGroup::new();

        macro_rules! video_url_action {
            ( $name:literal, $method:ident, $quality:expr) => {{
                let action = gio::SimpleAction::new($name, None);
                action.connect_activate(glib::clone!(
                    #[weak(rename_to = slf)]
                    self,
                    move |_, _| slf.$method($quality)
                ));
                action.set_enabled(
                    self.show()
                        .and_then(|show| show.video_url($quality))
                        .is_some_and(|url| !url.is_empty()),
                );
                self.connect_show_notify(glib::clone!(
                    #[weak]
                    action,
                    move |slf| {
                        action.set_enabled(
                            slf.show()
                                .and_then(|show| show.video_url($quality))
                                .is_some_and(|url| !url.is_empty()),
                        );
                    }
                ));
                actions.add_action(&action);
                action
            }};
        }

        // The default Play action is available whenever *any* stream exists.
        // Quality fallback is performed by VideoInfo::uri at playback time.
        let play_default = gio::SimpleAction::new("play-default", None);
        play_default.connect_activate(glib::clone!(
            #[weak(rename_to = slf)]
            self,
            move |_, _| slf.play(VideoQuality::default_playback())
        ));
        play_default.set_enabled(self.has_playable_stream());
        self.connect_show_notify(glib::clone!(
            #[weak]
            play_default,
            move |slf| play_default.set_enabled(slf.has_playable_stream())
        ));
        actions.add_action(&play_default);
        video_url_action!("play-high", play, VideoQuality::High);
        video_url_action!("play-medium", play, VideoQuality::Medium);
        video_url_action!("play-low", play, VideoQuality::Low);

        video_url_action!("copy-url-high", copy_video_url, VideoQuality::High);
        video_url_action!("copy-url-medium", copy_video_url, VideoQuality::Medium);
        video_url_action!("copy-url-low", copy_video_url, VideoQuality::Low);

        let copy_subtitles_url = gio::SimpleAction::new("copy-subtitles-url", None);
        copy_subtitles_url.connect_activate(glib::clone!(
            #[weak(rename_to = slf)]
            self,
            move |_, _| slf.copy_subtitles_url()
        ));
        copy_subtitles_url.set_enabled(
            self.show()
                .and_then(|show| show.subtitle_url())
                .is_some_and(|url| !url.is_empty()),
        );
        self.connect_show_notify(glib::clone!(
            #[weak]
            copy_subtitles_url,
            move |slf| {
                copy_subtitles_url
                    .set_enabled(slf.show().and_then(|show| show.subtitle_url()).is_some());
            }
        ));
        actions.add_action(&copy_subtitles_url);

        let download = gio::SimpleAction::new("download", None);
        download.connect_activate(glib::clone!(
            #[weak(rename_to = slf)]
            self,
            move |_, _| slf.download()
        ));
        download.set_enabled(self.show().and_then(|show| show.website_url()).is_some());
        self.connect_show_notify(glib::clone!(
            #[weak]
            download,
            move |slf| {
                download.set_enabled(slf.show().and_then(|show| show.website_url()).is_some());
            }
        ));
        actions.add_action(&download);

        let open_website = gio::SimpleAction::new("open-website", None);
        open_website.connect_activate(glib::clone!(
            #[weak(rename_to = slf)]
            self,
            move |_, _| spawn(async move {
                let url = slf
                    .show()
                    .and_then(|show| show.website_url())
                    .expect("action must only be enabled if url is not None");
                // A third-party programme URL is only a website link. Do
                // not invoke desktop handlers for arbitrary URI schemes.
                if !matches!(
                    reqwest::Url::parse(&url),
                    Ok(parsed) if matches!(parsed.scheme(), "http" | "https")
                ) {
                    show_error(&gettext("Invalid programme website URL"));
                    return;
                }
                if gtk::UriLauncher::new(&url)
                    .launch_future(slf.root().and_downcast_ref::<gtk::Window>())
                    .await
                    .is_err()
                {
                    show_error(&gettext("Failed to open website in browser"));
                }
            })
        ));
        open_website.set_enabled(self.show().and_then(|show| show.website_url()).is_some());
        self.connect_show_notify(glib::clone!(
            #[weak]
            open_website,
            move |slf| {
                open_website.set_enabled(slf.show().and_then(|show| show.website_url()).is_some());
            }
        ));
        actions.add_action(&open_website);

        self.insert_action_group("card", Some(&actions));
    }
}

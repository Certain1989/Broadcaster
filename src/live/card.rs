// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use adw::{glib, gtk, prelude::*, subclass::prelude::*};

use crate::{application::VideoInfo, channel_icons::load_channel_icon, TvApplication};

use super::channels::ChannelObject;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate, glib::Properties)]
    #[template(file = "src/live/card.blp")]
    #[properties(wrapper_type=super::TvLiveCard)]
    pub struct TvLiveCard {
        #[template_child]
        icon: TemplateChild<gtk::Image>,
        #[template_child]
        fallback_icon: TemplateChild<gtk::Label>,
        #[template_child]
        title: TemplateChild<gtk::Label>,
        #[template_child]
        subtitle: TemplateChild<gtk::Label>,
        #[template_child]
        progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        description: TemplateChild<gtk::Label>,
        #[template_child]
        play_button: TemplateChild<gtk::Button>,

        #[property(get, set)]
        expanded: Cell<bool>,

        #[property(get, construct_only)]
        pub(super) channel: RefCell<Option<ChannelObject>>,
    }

    impl TvLiveCard {
        fn set_icon(&self) {
            let Some(channel) = self.obj().channel() else {
                return;
            };
            self.icon
                .update_property(&[gtk::accessible::Property::Label(&channel.name())]);

            load_channel_icon(Some(&channel.id()), &self.icon, &self.fallback_icon, 64);
        }

        /// Shows how far the current show has progressed.
        fn update_progress(&self) {
            let Some(channel) = self.obj().channel() else {
                return;
            };
            let start_time = channel.start_time();
            let end_time = channel.end_time();
            let Ok(now) = glib::DateTime::now_local() else {
                return;
            };

            if let Some(fraction) = programme_progress(start_time, end_time, now.to_unix()) {
                self.progress.set_fraction(fraction);
                self.progress.set_visible(true);
            } else {
                self.progress.set_visible(false);
            }
        }

        /// Updates the progress bar when a programme changes.
        fn watch_progress(&self, channel: &ChannelObject) {
            let update = {
                let card = self.obj().downgrade();
                move |_: &ChannelObject| {
                    if let Some(card) = card.upgrade() {
                        card.imp().update_progress();
                    }
                }
            };
            channel.connect_start_time_notify(update.clone());
            channel.connect_end_time_notify(update);
            self.update_progress();
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvLiveCard {
        const NAME: &'static str = "TvLiveCard";
        type Type = super::TvLiveCard;
        type ParentType = gtk::ListBoxRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();

            klass.install_action_async("card.play", None, |slf, _, _| async move {
                let channel = slf.channel().unwrap();
                TvApplication::get()
                    .play(VideoInfo::Live {
                        title: channel.name(),
                        uri: channel.stream_url(),
                    })
                    .await;
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for TvLiveCard {
        fn constructed(&self) {
            self.parent_constructed();

            self.obj().connect_channel_notify(|slf| {
                slf.imp().set_icon();
                if let Some(channel) = slf.channel() {
                    slf.imp().watch_progress(&channel);
                }
            });

            // tell assistive technologies that the row expands
            let update_expanded = |slf: &super::TvLiveCard| {
                slf.update_state(&[gtk::accessible::State::Expanded(Some(slf.expanded()))])
            };
            update_expanded(&self.obj());
            self.obj().connect_expanded_notify(update_expanded);

            self.title
                .connect_label_notify(|title| title.set_visible(!title.label().is_empty()));

            self.subtitle.connect_label_notify(|subtitle| {
                subtitle.set_visible(!subtitle.label().is_empty())
            });

            self.description.connect_label_notify(|description| {
                description.set_visible(!description.label().is_empty())
            });

            self.revealer.connect_child_revealed_notify(|revealer| {
                revealer.set_visible(revealer.is_child_revealed())
            });

            // the progress advances every 10 seconds
            glib::timeout_add_seconds_local(
                10,
                glib::clone!(
                    #[weak(rename_to = slf)]
                    self,
                    #[upgrade_or]
                    glib::ControlFlow::Break,
                    move || {
                        slf.update_progress();
                        glib::ControlFlow::Continue
                    }
                ),
            );
        }
    }
    impl WidgetImpl for TvLiveCard {}
    impl ListBoxRowImpl for TvLiveCard {}
}

glib::wrapper! {
    pub struct TvLiveCard(ObjectSubclass<imp::TvLiveCard>)
        @extends gtk::Widget, gtk::ListBoxRow,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

/// Invalid/absent upstream programme times must not reach GtkProgressBar.
fn programme_progress(start: i64, end: i64, now: i64) -> Option<f64> {
    if start <= 0 || end <= start {
        return None;
    }
    Some(((now - start) as f64 / (end - start) as f64).clamp(0.0, 1.0))
}

#[cfg(test)]
mod progress_tests {
    use super::programme_progress;

    #[test]
    fn progress_handles_invalid_and_boundary_times() {
        assert_eq!(programme_progress(0, 100, 20), None);
        assert_eq!(programme_progress(100, 100, 100), None);
        assert_eq!(programme_progress(101, 100, 100), None);
        assert_eq!(programme_progress(100, 200, 50), Some(0.0));
        assert_eq!(programme_progress(100, 200, 150), Some(0.5));
        assert_eq!(programme_progress(100, 200, 250), Some(1.0));
    }
}

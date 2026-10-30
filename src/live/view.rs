// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, OnceCell};

use adw::{gio, glib, gtk, prelude::*, subclass::prelude::*};
use futures_util::StreamExt;
use gettextrs::gettext;
use smart_default::SmartDefault;

use crate::{
    application::TvApplication,
    settings::TvSettings,
    utils::{spawn, tokio},
    zapp::{ChannelId, ChannelInfo, Show, ShowsResult},
};

use super::{card::TvLiveCard, channels::ChannelObject, filter::channel_filter};

/// The number of programme requests that run at the same time.
const MAX_PARALLEL_REQUESTS: usize = 6;

mod imp {
    use super::*;

    #[derive(Debug, SmartDefault, gtk::CompositeTemplate)]
    #[template(file = "src/live/view.blp")]
    pub struct TvLiveView {
        #[template_child]
        pub(super) stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(super) search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub(super) keyboard_prototype_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) results_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(super) channels_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        error_page: TemplateChild<adw::StatusPage>,

        filter: OnceCell<gtk::CustomFilter>,
        /// Incremented on every reload to discard outdated results.
        pub(super) generation: Cell<u64>,
    }

    impl TvLiveView {
        async fn load_channels(&self) -> eyre::Result<gio::ListStore> {
            let client = TvApplication::get().zapp();
            let settings = TvSettings::get();
            let visible_channels = settings.live_channels();

            let live_channels = TvApplication::get()
                .live_channels()
                .await
                .map_err(|_| eyre::eyre!(gettext("Failed to load livestream channels")))?;

            let live_channels: Vec<(ChannelId, ChannelInfo)> = if visible_channels.is_empty() {
                live_channels
                    .iter()
                    .map(|(channel_id, channel_info)| (channel_id.clone(), channel_info.clone()))
                    .collect()
            } else {
                visible_channels
                    .into_iter()
                    .filter_map(|channel_id| {
                        let channel_info = live_channels.get(&channel_id)?;
                        Some((channel_id, channel_info.clone()))
                    })
                    .collect()
            };

            if live_channels.is_empty() {
                eyre::bail!(gettext("No livestream channels available"));
            }

            // the programme of a channel is optional, a failing request only
            // leaves its card without one
            let channels = tokio(async move {
                let channel_ids: Vec<ChannelId> =
                    live_channels.iter().map(|(id, _)| id.clone()).collect();
                let shows: Vec<Option<Vec<Show>>> = futures_util::stream::iter(channel_ids)
                    .map(|channel_id| {
                        let client = client.clone();
                        async move {
                            match client.shows(&channel_id).await {
                                Ok(ShowsResult::Shows(shows)) => Some(shows),
                                Ok(ShowsResult::Error(_)) => {
                                    tracing::debug!(
                                        "No programme information available for a channel"
                                    );
                                    None
                                }
                                Err(_) => {
                                    tracing::warn!(
                                        "Failed to load programme information for a channel"
                                    );
                                    None
                                }
                            }
                        }
                    })
                    .buffered(MAX_PARALLEL_REQUESTS)
                    .collect()
                    .await;

                live_channels.into_iter().zip(shows).collect::<Vec<_>>()
            })
            .await;

            let channel_objects = channels
                .into_iter()
                .map(|((channel_id, channel_info), shows)| {
                    let channel = ChannelObject::new(
                        channel_id.as_ref(),
                        &channel_info.name,
                        &channel_info.stream_url,
                    );
                    if let Some(Show {
                        title,
                        subtitle,
                        description,
                        channel: _,
                        start_time,
                        end_time,
                    }) = shows.as_ref().and_then(|shows| shows.first())
                    {
                        channel.set_title(Some(title.as_str()));
                        channel.set_subtitle(subtitle.as_deref());
                        channel.set_description(
                            description
                                .as_deref()
                                .map(crate::programme_markup::markup)
                                .as_deref(),
                        );
                        channel.set_start_time(start_time.unix_timestamp());
                        channel.set_end_time(end_time.unix_timestamp());
                    }
                    channel
                })
                .collect::<gio::ListStore>();

            Ok(channel_objects)
        }
        pub(super) async fn reload(&self) {
            let generation = self.generation.get() + 1;
            self.generation.set(generation);
            self.stack.set_visible_child_name("spinner");

            let result = self.load_channels().await;
            // a newer reload has started in the meantime
            if self.generation.get() != generation {
                return;
            }

            match result {
                Ok(channels) => {
                    let channels =
                        gtk::FilterListModel::new(Some(channels), self.filter.get().cloned());
                    channels.connect_items_changed(glib::clone!(
                        #[weak(rename_to = slf)]
                        self,
                        move |channels, _, _, _| slf.show_results(channels.n_items())
                    ));
                    self.show_results(channels.n_items());

                    self.channels_list.bind_model(Some(&channels), |channel| {
                        glib::Object::builder::<TvLiveCard>()
                            .property("channel", channel)
                            .build()
                            .upcast()
                    });
                    self.stack.set_visible_child_name("channels");
                }
                Err(_) => {
                    // No upstream diagnostics in logs or UI: network errors may
                    // contain credential-bearing URLs or response data.
                    tracing::error!("Failed to load livestream channels");
                    self.error_page.set_description(Some(&gettext(
                        "Failed to load livestream channels. Check your connection and retry.",
                    )));
                    self.stack.set_visible_child_name("error");
                }
            }
        }

        /// Shows the list, or a hint if no channel matches the filter.
        fn show_results(&self, matches: u32) {
            let page = if matches == 0 && !self.search_entry.text().is_empty() {
                "no-matches"
            } else {
                "list"
            };
            self.results_stack.set_visible_child_name(page);
        }

        pub(super) fn focus_search(&self) {
            self.search_entry.grab_focus();
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvLiveView {
        const NAME: &'static str = "TvLiveView";
        type Type = super::TvLiveView;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TvLiveView {
        fn constructed(&self) {
            self.parent_constructed();

            let settings = TvSettings::get();

            self.filter
                .set(channel_filter(&self.search_entry))
                .expect("the view is constructed once");
            // typing anywhere in the view filters the channels
            self.search_entry.set_key_capture_widget(Some(&*self.obj()));

            let search = self.search_entry.get();
            crate::keyboard::bind_launcher(
                &self.keyboard_prototype_button,
                &search,
                crate::keyboard::KeyboardOptions::default(),
            );

            // A second activation of an expanded row plays it. Keep playback
            // in the existing card action, which the explicit button also uses.
            connect_live_row_activation(&self.channels_list, |row| {
                if let Err(error) = row.activate_action("card.play", None) {
                    tracing::warn!("Failed to activate livestream playback: {error}");
                }
            });

            let slf = self.to_owned();
            spawn(async move { slf.reload().await });

            settings.connect_live_channels_changed(glib::clone!(
                #[weak(rename_to = slf)]
                self,
                move |_| {
                    spawn(async move { slf.reload().await });
                }
            ));
        }
    }
    impl WidgetImpl for TvLiveView {}
    impl BinImpl for TvLiveView {}
}

glib::wrapper! {
    pub struct TvLiveView(ObjectSubclass<imp::TvLiveView>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TvLiveView {
    /// Reloads the channel list and the programme, e.g. after an error.
    pub fn reload(&self) {
        TvApplication::get().live_channels().load();
        let slf = self.imp().to_owned();
        spawn(async move { slf.reload().await });
    }

    pub fn focus_search(&self) {
        self.imp().focus_search();
    }

    /// Expose the real channel toolbar for offline mapped GTK navigation tests.
    /// Production still waits for the normal asynchronous channel load.
    #[cfg(test)]
    pub(crate) fn show_toolbar_for_test(&self) {
        self.imp().stack.set_visible_child_name("channels");
    }

    /// Exercise the real channel-list-to-toolbar Up boundary without network
    /// requests. A synthetic row is only inserted when the offline fixture
    /// has not loaded a channel model.
    #[cfg(test)]
    pub(crate) fn focus_first_channel_for_test(&self) -> bool {
        let imp = self.imp();
        imp.results_stack.set_visible_child_name("list");
        if imp.channels_list.row_at_index(0).is_none() {
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&gtk::Label::new(Some("Synthetic channel"))));
            imp.channels_list.append(&row);
        }
        imp.channels_list
            .row_at_index(0)
            .is_some_and(|row| row.grab_focus())
    }

    /// Test-only complete native Live rows backed by actual TvLiveCard and
    /// ChannelObject widgets. No network requests or player activation.
    #[cfg(test)]
    pub(crate) fn install_r07_native_channel_rows(&self) {
        let imp = self.imp();
        // Invalidate a pending async production load before installing this
        // disposable model so its completion cannot rebind the test rows.
        imp.generation.set(imp.generation.get().saturating_add(1));
        let model = (0..5)
            .map(|index| {
                ChannelObject::new(
                    &format!("r07-live-{index}"),
                    &format!("Fixture Live {index}"),
                    "https://example.invalid/offline",
                )
            })
            .collect::<gio::ListStore>();
        imp.channels_list.bind_model(Some(&model), |channel| {
            glib::Object::builder::<TvLiveCard>()
                .property("channel", channel)
                .build()
                .upcast()
        });
        imp.results_stack.set_visible_child_name("list");
        imp.stack.set_visible_child_name("channels");
    }

    #[cfg(test)]
    pub(crate) fn r07_native_channel_id(&self, index: i32) -> Option<String> {
        self.imp()
            .channels_list
            .row_at_index(index)?
            .downcast::<TvLiveCard>()
            .ok()?
            .channel()
            .map(|channel| channel.id())
    }

    /// The Live focus style must reach the button actually selected by the
    /// remote, even when programmatic focus is not yet :focus-visible.
    #[cfg(test)]
    pub(crate) fn assert_keyboard_remote_focus_styled(&self) {
        let button = &self.imp().keyboard_prototype_button;
        assert!(
            button.has_focus(),
            "Up from first channel must focus Keyboard"
        );
        let toolbar = button.parent().expect("Live keyboard toolbar");
        assert!(
            toolbar.has_css_class("tv-live-controls"),
            "Live keyboard must be inside a visible remote-focus styling scope"
        );
        assert!(
            self.imp().search_entry.is_ancestor(&toolbar),
            "Live search and Keyboard must share focus styling"
        );
        let css = gio::resources_lookup_data(
            "/io/github/Certain1989/Broadcaster/broadcaster.css",
            gio::ResourceLookupFlags::NONE,
        )
        .expect("bundled application focus stylesheet");
        let css = String::from_utf8_lossy(css.as_ref());
        for selector in [
            ".tv-live-controls button:focus",
            ".tv-live-controls button:focus-within",
            ".tv-live-controls button:focus-visible",
        ] {
            assert!(
                css.contains(selector),
                "focused Keyboard needs bundled stylesheet selector {selector}"
            );
        }
    }

    /// D-pad Left and remote Back from the real Play control return to its
    /// owning row, without activating playback or changing the selected page.
    /// A rebound model cannot reuse the stale row: ownership is resolved
    /// exclusively from the current focused widget's actual GTK ancestry.
    pub fn focus_play_return(&self, focused: &gtk::Widget) -> bool {
        if !focused.is_ancestor(self.upcast_ref::<gtk::Widget>()) {
            return false;
        }
        let mut ancestor = focused.parent();
        while let Some(widget) = ancestor {
            if let Some(row) = widget.downcast_ref::<TvLiveCard>() {
                return live_play_button(row.upcast_ref())
                    .is_some_and(|button| focused == button.upcast_ref::<gtk::Widget>())
                    && row.grab_focus();
            }
            ancestor = widget.parent();
        }
        false
    }

    /// Only mapped upper controls belong to spatial arrow navigation.
    /// Returning the actual field instance preserves K1's ON/OFF launcher.
    pub fn toolbar_focus_targets(&self) -> Vec<gtk::Widget> {
        let imp = self.imp();
        vec![
            imp.search_entry.get().upcast(),
            imp.keyboard_prototype_button.get().upcast(),
        ]
    }

    pub fn focus_keyboard_prototype(&self) -> bool {
        self.imp().keyboard_prototype_button.grab_focus()
    }

    pub fn keyboard_prototype_has_focus(&self, widget: &gtk::Widget) -> bool {
        widget
            == self
                .imp()
                .keyboard_prototype_button
                .upcast_ref::<gtk::Widget>()
    }
}

/// Locate the actual Play control without exposing the card template's
/// private implementation or guessing an action from child order.
fn live_play_button(widget: &gtk::Widget) -> Option<gtk::Button> {
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        if button.action_name().as_deref() == Some("card.play") {
            return Some(button.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(button) = live_play_button(&current) {
            return Some(button);
        }
        child = current.next_sibling();
    }
    None
}

/// Connect GTK's normal row-activation signal (keyboard/remote OK or mouse)
/// without intercepting Return globally or interfering with child buttons.
fn connect_live_row_activation(list: &gtk::ListBox, play: impl Fn(&TvLiveCard) + 'static) {
    list.connect_row_activated(move |_, row| {
        let row = row
            .downcast_ref::<TvLiveCard>()
            .expect("invalid livestream ListBoxRow type");
        if row.expanded() {
            play(row);
        } else {
            row.set_expanded(true);
            // Remote OK enters the primary action without starting playback.
            // It is a real button rather than a synthetic card focus target.
            if let Some(button) = live_play_button(row.upcast_ref()) {
                button.grab_focus();
            }
        }
    });
}

#[cfg(test)]
mod live_row_activation_tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    /// Exercises the actual GTK ListBox signal and the same signal handler used
    /// in production. Playback is replaced by a counter so mpv is never started.
    #[test]
    #[ignore = "requires a live GDK display; CI executes this under xvfb-run"]
    fn gtk_row_activation_expands_then_plays_exactly_once() {
        let bytes = glib::Bytes::from_static(crate::GRESOURCE_BYTES);
        let resource = gio::Resource::from_data(&bytes).expect("valid bundled resources");
        gio::resources_register(&resource);
        gtk::init().expect("GTK needs a test display");

        // No current-show metadata: these channels must still be playable.
        let channel = ChannelObject::new("test", "Test channel", "https://example.org/live");
        let card: TvLiveCard = glib::Object::builder()
            .property("channel", &channel)
            .build();
        let list = gtk::ListBox::new();
        list.append(&card);

        let starts = Rc::new(Cell::new(0u32));
        connect_live_row_activation(&list, {
            let starts = starts.clone();
            move |_| starts.set(starts.get() + 1)
        });

        assert!(!card.expanded());
        list.emit_by_name::<()>("row-activated", &[&card]);
        assert!(card.expanded());
        assert_eq!(starts.get(), 0);

        list.emit_by_name::<()>("row-activated", &[&card]);
        assert!(card.expanded());
        assert_eq!(starts.get(), 1);

        // One activation causes one playback request; the existing play
        // button still binds directly to the same card.play class action.
        assert!(has_play_button(card.upcast_ref()));
        card.set_expanded(false);
        list.emit_by_name::<()>("row-activated", &[&card]);
        assert!(card.expanded());
        assert_eq!(starts.get(), 1);
    }

    fn has_play_button(widget: &gtk::Widget) -> bool {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.action_name().as_deref() == Some("card.play") {
                return true;
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if has_play_button(&current) {
                return true;
            }
            child = current.next_sibling();
        }
        false
    }
}

#[cfg(test)]
mod programme_description_markup_tests {
    #[test]
    fn default_sanitizer_preserves_formatting_without_css_or_svg_animation() {
        // The crate::programme_markup::markup path uses ammonia's default allowlist.
        // The style/SVG conditions described in RUSTSEC-2021-0157/0158
        // must not be enabled when displaying programme descriptions.
        let input = r#"<b>Bold programme</b><span style="color:red">Text</span><svg><a><animateMotion attributeName="href" to="javascript:alert(1)"></animateMotion></a></svg>"#;
        let rendered = crate::programme_markup::markup(input);

        assert!(rendered.contains("<b>Bold programme</b>"));
        assert!(!rendered.contains("style="));
        assert!(!rendered.contains("javascript:"));
        assert!(!rendered.contains("animateMotion"));
    }
}

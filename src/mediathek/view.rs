// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, OnceCell, RefCell},
    sync::Arc,
    time::Duration,
};

use adw::{gio, glib, gtk, prelude::*, subclass::prelude::*};
use eyre::WrapErr;
use gettextrs::gettext;
use mediathekviewweb::{
    models::{QueryField, SortField, SortOrder},
    Mediathek, MediathekQueryBuilder,
};
use serde::Deserialize;

use crate::{
    config::{APP_ID, PROJECT_URL, VERSION},
    settings::TvSettings,
    utils::{show_error, spawn, spawn_clone, tokio},
};

use super::{card::TvMediathekCard, shows::ShowObject};

const SEARCH_TIMEOUT: Duration = Duration::from_secs(25);
const CHANNELS_TIMEOUT: Duration = Duration::from_secs(8);
/// Bound catalogue responses independently of server-provided Content-Length.
const MAX_CHANNELS_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const CHANNELS_URL: &str = "https://mediathekviewweb.de/api/channels";

// Immediately available offline; replaced with the indexed catalogue when online.
const FALLBACK_BROADCASTERS: &[&str] = &[
    "ARD", "ZDF", "ARTE", "3sat", "KiKA", "WDR", "NDR", "MDR", "BR", "SWR", "phoenix",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Broadcaster {
    label: String,
    identifier: String,
}

#[derive(Debug, Deserialize)]
struct ChannelsResponse {
    error: Option<String>,
    channels: Option<Vec<String>>,
}

fn append_channels_bounded(body: &mut Vec<u8>, chunk: &[u8]) -> eyre::Result<()> {
    if chunk.len() > MAX_CHANNELS_RESPONSE_BYTES.saturating_sub(body.len()) {
        eyre::bail!("Mediathek channel catalogue exceeds the allowed size");
    }
    body.extend_from_slice(chunk);
    Ok(())
}

async fn decode_channels_bounded(
    mut response: reqwest::Response,
) -> eyre::Result<ChannelsResponse> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        append_channels_bounded(&mut body, &chunk)?;
    }
    Ok(serde_json::from_slice(&body)?)
}

fn broadcaster_index(entries: &[Broadcaster], identifier: Option<&str>) -> u32 {
    identifier
        .and_then(|name| {
            entries
                .iter()
                .position(|entry| entry.identifier == name)
                .map(|index| index as u32 + 1)
        })
        .unwrap_or(0)
}

/// The upstream endpoint supplies names currently present in the search index.
/// Keep presentation labels separate from request identifiers.
fn normalise_channels(names: impl IntoIterator<Item = String>) -> Vec<Broadcaster> {
    let mut names: Vec<String> = names
        .into_iter()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
        .into_iter()
        .map(|identifier| Broadcaster {
            label: identifier.clone(),
            identifier,
        })
        .collect()
}

/// Compose textual selectors for initial searches and pagination.
///
/// A selected graphical filter takes precedence over manually entered
/// selectors for that same dimension, without editing the search field.
/// Duration presets use the second-accurate API request builder.
fn compose_query(text: &str, broadcaster: Option<&str>, duration_index: u32) -> String {
    let parts: Vec<String> = text
        .split_whitespace()
        .filter(|term| {
            let is_duration_selector = term
                .strip_prefix('<')
                .or_else(|| term.strip_prefix('>'))
                .is_some_and(|minutes| minutes.parse::<u64>().is_ok());

            (broadcaster.is_none() || !term.starts_with('!'))
                && (duration_index == 0 || !is_duration_selector)
        })
        .map(ToOwned::to_owned)
        .collect();

    parts.join(" ")
}

/// Work around mediathekviewweb 0.5.0's plain-term parser, which puts the
/// entire input (including !channel etc.) into each unprefixed search clause.
/// Keep the library's advanced-selector parsing, but submit the complete
/// plain-text expression as a separate topic/title (or all-field) clause.
fn separate_search_terms(query: &str) -> (String, String) {
    let mut selectors = Vec::new();
    let mut plain = Vec::new();

    for term in query.split_whitespace() {
        let field_selector = term.starts_with('!')
            || term.starts_with('#')
            || term.starts_with('+')
            || term.starts_with('*');
        let duration_selector = term
            .strip_prefix('>')
            .or_else(|| term.strip_prefix('<'))
            .is_some_and(|minutes| minutes.parse::<u64>().is_ok());

        if field_selector || duration_selector {
            selectors.push(term);
        } else {
            plain.push(term);
        }
    }

    (selectors.join(" "), plain.join(" "))
}

fn build_search_query<'a>(
    client: &'a Mediathek,
    query: &str,
    search_everywhere: bool,
    broadcaster: Option<&str>,
) -> MediathekQueryBuilder<'a> {
    let (selectors, plain) = separate_search_terms(query);
    let mut builder = client.query_string(&selectors, search_everywhere);
    // A full indexed channel name may contain spaces; do not encode it as !sender.
    if let Some(identifier) = broadcaster.filter(|name| !name.is_empty()) {
        builder = builder.query([QueryField::Channel], identifier);
    }
    if !plain.is_empty() {
        let fields = if search_everywhere {
            QueryField::ALL.to_vec()
        } else {
            vec![QueryField::Topic, QueryField::Title]
        };
        // Preserve the user's complete ordinary text expression, rather than
        // turning each word into an extra AND clause.
        builder = builder.query(fields, plain);
    }
    builder
}

/// The API uses strict bounds in seconds; textual selectors accept integer
/// minutes only. Adjust by one second to include both interval endpoints.
fn duration_bounds(index: u32) -> (Option<Duration>, Option<Duration>) {
    match index {
        1 => (None, Some(Duration::from_secs(900))),
        2 => (
            Some(Duration::from_secs(899)),
            Some(Duration::from_secs(3601)),
        ),
        3 => (Some(Duration::from_secs(3600)), None),
        _ => (None, None),
    }
}

/// Bound both first-page and pagination requests so stalled connections can be retried.
async fn complete_request<T, E>(
    request: impl std::future::Future<Output = Result<T, E>>,
    timeout: Duration,
) -> eyre::Result<T>
where
    E: std::error::Error + Send + Sync + 'static,
{
    tokio::time::timeout(timeout, request)
        .await
        .wrap_err_with(|| gettext("MediathekViewWeb request timed out"))?
        .wrap_err_with(|| gettext("Failed to query the MediathekViewWeb API"))
}

fn apply_duration_bounds(
    mut query: mediathekviewweb::MediathekQueryBuilder<'_>,
    min: Option<Duration>,
    max: Option<Duration>,
) -> mediathekviewweb::MediathekQueryBuilder<'_> {
    if let Some(min) = min {
        query = query.duration_min(min);
    }
    if let Some(max) = max {
        query = query.duration_max(max);
    }
    query
}

/// Serializes page requests within one search and rejects completions from
/// superseded searches. GTK callbacks run on the main context, so no mutex is
/// needed; the request future must not hold a borrow across an await.
#[derive(Debug, Default)]
struct SearchRequests {
    generation: u64,
    pending_page: Option<(u64, u32)>,
}

impl SearchRequests {
    fn begin_search(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending_page = None;
        self.generation
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    fn begin_page(&mut self, offset: u32) -> Option<u64> {
        if self.pending_page.is_some() {
            return None;
        }
        self.pending_page = Some((self.generation, offset));
        Some(self.generation)
    }

    fn finish_page(&mut self, generation: u64, offset: u32) -> bool {
        if self.pending_page != Some((generation, offset)) {
            return false;
        }
        self.pending_page = None;
        true
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate, glib::Properties)]
    #[template(file = "src/mediathek/view.blp")]
    #[properties(wrapper_type = super::TvMediathekView)]
    pub struct TvMediathekView {
        #[template_child]
        filters_toolbar: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub(super) keyboard_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) broadcaster_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub(super) load_more_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) duration_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub(super) sort_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        status_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub(super) results_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub(super) stack: TemplateChild<gtk::Stack>,
        #[template_child]
        nothing_found_view: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub(super) results_view: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        loading_view: TemplateChild<adw::Spinner>,
        #[template_child]
        error_view: TemplateChild<adw::StatusPage>,

        #[property(get, set)]
        compact: Cell<bool>,

        #[property(get, set)]
        query_string: RefCell<String>,
        #[property(get, set)]
        include_future: Cell<bool>,
        #[property(get, set)]
        search_everywhere: Cell<bool>,

        #[property(get, set)]
        sort_by: RefCell<String>,
        #[property(get, set)]
        sort_order: RefCell<String>,

        #[property(get, set)]
        total_results: Cell<u64>,
        #[property(get, set)]
        more_available: Cell<bool>,

        pub(super) client: OnceCell<Arc<Mediathek>>,
        pub(super) shows_model: OnceCell<gio::ListStore>,
        /// Search generation and exactly one in-flight page per generation.
        pub(super) requests: RefCell<SearchRequests>,
        pub(super) broadcasters: RefCell<Vec<Broadcaster>>,
        pub(super) pending_broadcasters: RefCell<Option<Vec<Broadcaster>>>,
        pub(super) updating_broadcaster_model: Cell<bool>,
        /// Only choose automatic focus on the first mapping.
        initial_focus_assigned: Cell<bool>,
    }
    impl TvMediathekView {
        pub(super) fn shows_model(&self) -> gio::ListStore {
            self.shows_model
                .get_or_init(gio::ListStore::new::<ShowObject>)
                .clone()
        }
        pub(super) fn show_status_page(&self) {
            self.stack.set_visible_child(&*self.status_page)
        }
        /// Shows the results, or a hint if there are none.
        pub(super) fn show_results(&self) {
            let n_items = self.shows_model().n_items();
            if n_items == 0 {
                self.stack.set_visible_child(&*self.nothing_found_view);
            } else {
                self.stack.set_visible_child(&*self.results_view);
            }
            let more_available = n_items > 0 && u64::from(n_items) < self.total_results.get();
            self.obj().set_more_available(more_available);
            self.obj().action_set_enabled(
                "mediathek.load-more",
                more_available && self.requests.borrow().pending_page.is_none(),
            );
        }
        /// Hide previous pages while a replacement search is pending.
        pub(super) fn show_loading(&self) {
            self.stack.set_visible_child(&*self.loading_view);
        }
        pub(super) fn show_error_page(&self) {
            tracing::error!("Mediathek search failed (upstream diagnostics suppressed)");
            self.error_view.set_description(Some(&gettext(
                "Mediathek search failed. Check your connection and retry.",
            )));
            self.stack.set_visible_child(&*self.error_view);
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvMediathekView {
        const NAME: &'static str = "TvMediathekView";
        type Type = super::TvMediathekView;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();

            klass.install_property_action("mediathek.sort-by", "sort-by");
            klass.install_property_action("mediathek.sort-order", "sort-order");
            klass.install_action_async("mediathek.load-more", None, |slf, _, _| async move {
                slf.load_more().await
            })
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for TvMediathekView {
        fn constructed(&self) {
            self.parent_constructed();

            let slf = self.obj();
            let settings = TvSettings::get();
            crate::keyboard::bind_launcher(
                &self.keyboard_button,
                &self.search_entry.get(),
                crate::keyboard::KeyboardOptions::default(),
            );
            settings
                .bind_search_everywhere(&*slf, "search-everywhere")
                .flags(gio::SettingsBindFlags::DEFAULT)
                .build();
            settings
                .bind_include_future(&*slf, "include-future")
                .flags(gio::SettingsBindFlags::DEFAULT)
                .build();
            settings
                .bind_sort_by(&*slf, "sort-by")
                .flags(gio::SettingsBindFlags::DEFAULT)
                .build();
            settings
                .bind_sort_order(&*slf, "sort-order")
                .flags(gio::SettingsBindFlags::DEFAULT)
                .build();

            slf.connect_compact_notify(|slf| {
                let imp = slf.imp();
                if slf.compact() {
                    imp.filters_toolbar
                        .set_orientation(gtk::Orientation::Vertical);
                    imp.status_page.add_css_class("compact");
                } else {
                    imp.filters_toolbar
                        .set_orientation(gtk::Orientation::Horizontal);
                    imp.status_page.remove_css_class("compact");
                }
            });

            slf.set_broadcasters(normalise_channels(
                FALLBACK_BROADCASTERS.iter().map(|name| (*name).to_owned()),
            ));

            let duration_names = [
                gettext("Any duration"),
                gettext("Under 15 minutes"),
                gettext("15–60 minutes"),
                gettext("Over 60 minutes"),
            ];
            let duration_labels: Vec<&str> = duration_names.iter().map(String::as_str).collect();
            self.duration_filter
                .set_model(Some(&gtk::StringList::new(&duration_labels)));

            // GtkDropDown's internal GtkListView can open without a focused
            // row. TV remote users otherwise need one wasted OK press before
            // the first item can be activated. Focus the selected row on map,
            // without selecting another item or starting a new search.
            install_dropdown_popup_focus(&self.broadcaster_filter);
            install_dropdown_popup_focus(&self.duration_filter);

            self.results_list
                .bind_model(Some(&self.shows_model()), |show| {
                    glib::Object::builder::<TvMediathekCard>()
                        .property("show", show)
                        .build()
                        .into()
                });

            self.results_list.connect_row_activated(|_, row| {
                let row = row
                    .downcast_ref::<TvMediathekCard>()
                    .expect("invalid ListBoxRow type");
                row.set_expanded(!row.expanded())
            });

            fn load(slf: &super::TvMediathekView) {
                spawn_clone!(slf => slf.load())
            }

            self.search_entry.connect_search_changed(glib::clone!(
                #[weak]
                slf,
                move |_| spawn(async move { slf.load().await })
            ));
            slf.connect_search_everywhere_notify(load);
            slf.connect_include_future_notify(load);
            slf.connect_sort_by_notify(load);
            slf.connect_sort_order_notify(load);
            self.broadcaster_filter
                .connect_selected_notify(glib::clone!(
                    #[weak]
                    slf,
                    move |_| {
                        if !slf.imp().updating_broadcaster_model.get() {
                            load(&slf);
                        }
                    }
                ));
            self.broadcaster_filter
                .connect_has_focus_notify(glib::clone!(
                    #[weak]
                    slf,
                    move |dropdown| {
                        if !dropdown.has_focus() {
                            let pending = slf.imp().pending_broadcasters.borrow_mut().take();
                            if let Some(channels) = pending {
                                slf.set_broadcasters(channels);
                            }
                        }
                    }
                ));
            self.duration_filter.connect_selected_notify(glib::clone!(
                #[weak]
                slf,
                move |_| load(&slf)
            ));

            slf.refresh_broadcasters();
            slf.connect_map(|slf| {
                let imp = slf.imp();
                if !imp.initial_focus_assigned.replace(true) {
                    // Initial Mediathek focus belongs in search, not a filter.
                    // Only assign it once; later result updates must not steal focus.
                    imp.search_entry.grab_focus();
                }
            });
        }
    }
    impl WidgetImpl for TvMediathekView {}
    impl BinImpl for TvMediathekView {}
}

glib::wrapper! {
    pub struct TvMediathekView(ObjectSubclass<imp::TvMediathekView>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TvMediathekView {
    fn selected_broadcaster(&self) -> Option<String> {
        let imp = self.imp();
        imp.broadcasters
            .borrow()
            .get(imp.broadcaster_filter.selected().checked_sub(1)? as usize)
            .map(|entry| entry.identifier.clone())
    }

    fn set_broadcasters(&self, entries: Vec<Broadcaster>) {
        let imp = self.imp();
        let old_selected = self.selected_broadcaster();
        let selected_index = broadcaster_index(&entries, old_selected.as_deref());
        let selection_disappeared = old_selected.is_some() && selected_index == 0;

        let mut labels = vec![gettext("All broadcasters")];
        labels.extend(entries.iter().map(|entry| entry.label.clone()));
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

        imp.updating_broadcaster_model.set(true);
        imp.broadcasters.replace(entries);
        imp.broadcaster_filter
            .set_model(Some(&gtk::StringList::new(&label_refs)));
        imp.broadcaster_filter.set_selected(selected_index);
        imp.updating_broadcaster_model.set(false);

        if selection_disappeared {
            self.reload();
        }
    }

    fn refresh_broadcasters(&self) {
        let slf = self.clone();
        spawn(async move {
            let result = tokio(async move {
                let response = reqwest::Client::new()
                    .get(CHANNELS_URL)
                    .header(
                        reqwest::header::USER_AGENT,
                        format!("{APP_ID}/{VERSION} ({PROJECT_URL})"),
                    )
                    .timeout(CHANNELS_TIMEOUT)
                    .send()
                    .await?
                    .error_for_status()?;
                decode_channels_bounded(response).await
            })
            .await;

            match result {
                Ok(ChannelsResponse {
                    error: None,
                    channels: Some(channels),
                }) if !channels.is_empty() => {
                    let entries = normalise_channels(channels);
                    if !entries.is_empty() {
                        let imp = slf.imp();
                        if imp.broadcaster_filter.has_focus() {
                            // Avoid replacing the popup model during remote navigation.
                            imp.pending_broadcasters.replace(Some(entries));
                        } else {
                            slf.set_broadcasters(entries);
                        }
                    }
                }
                Ok(_) => {
                    tracing::warn!("Mediathek channel catalogue unavailable; retaining fallback")
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "Could not load Mediathek channel catalogue; retaining fallback"
                    );
                }
            }
        });
    }

    /// Mediathek has two distinct rows of TV controls. Do not rely on GTK's
    /// geometric arrow navigation to guess a destination: in particular,
    /// Search -> results skips Channel/Duration/Sort on a large TV screen.
    ///
    /// Return false at unhandled boundaries (notably Search + Up, which is
    /// handled by TvWindow's global view switcher). Popovers and dialogs are
    /// excluded by the caller so their native navigation remains intact.
    pub fn focus_vertical_navigation(&self, focused: &gtk::Widget, key: gtk::gdk::Key) -> bool {
        let imp = self.imp();
        let search = imp.search_entry.upcast_ref::<gtk::Widget>();
        let keyboard = imp.keyboard_button.upcast_ref::<gtk::Widget>();
        let channel = imp.broadcaster_filter.upcast_ref::<gtk::Widget>();
        let duration = imp.duration_filter.upcast_ref::<gtk::Widget>();
        let sort = imp.sort_button.upcast_ref::<gtk::Widget>();
        let results = imp.results_view.upcast_ref::<gtk::Widget>();

        if key == gtk::gdk::Key::Up {
            if widget_contains_focus(results, focused) {
                if let Some(row) = result_row_for_focus(focused, &imp.results_list) {
                    if row.index() > 0 {
                        // Even an expanded card's Play button can move upward
                        // through the results instead of trapping the remote.
                        return imp
                            .results_list
                            .row_at_index(row.index() - 1)
                            .is_some_and(|previous| previous.grab_focus());
                    }
                    return imp.sort_button.grab_focus();
                }
                // GTK may leave keyboard focus on the scroll container or
                // ListBox itself instead of a row. Let it scroll normally
                // until the beginning, then make Up leave the results.
                let adjustment = imp.results_view.vadjustment();
                if adjustment.value() <= adjustment.lower() + 1.0 {
                    return imp.sort_button.grab_focus();
                }
                return false;
            }
            if widget_contains_focus(sort, focused) {
                return imp.duration_filter.grab_focus();
            }
            if widget_contains_focus(duration, focused) {
                return imp.broadcaster_filter.grab_focus();
            }
            if widget_contains_focus(channel, focused) {
                return imp.keyboard_button.grab_focus();
            }
            if widget_contains_focus(keyboard, focused) {
                return imp.search_entry.grab_focus();
            }
        } else if key == gtk::gdk::Key::Down {
            if widget_contains_focus(search, focused) {
                return imp.keyboard_button.grab_focus();
            }
            if widget_contains_focus(keyboard, focused) {
                return imp.broadcaster_filter.grab_focus();
            }
            if widget_contains_focus(channel, focused) {
                return imp.duration_filter.grab_focus();
            }
            if widget_contains_focus(duration, focused) {
                return imp.sort_button.grab_focus();
            }
            if widget_contains_focus(sort, focused) {
                return imp
                    .results_list
                    .row_at_index(0)
                    .is_some_and(|first| first.grab_focus());
            }
        }
        false
    }

    /// The two search toggles are immediate siblings of the search entry in
    /// the real Blueprint row; the second row contains the three filters.
    /// Geometry, not declaration order, determines each arrow destination.
    pub fn toolbar_focus_targets(&self) -> Vec<gtk::Widget> {
        let imp = self.imp();
        let mut controls = vec![
            imp.search_entry.get().upcast::<gtk::Widget>(),
            imp.keyboard_button.get().upcast::<gtk::Widget>(),
        ];
        let mut sibling = imp.search_entry.upcast_ref::<gtk::Widget>().next_sibling();
        while let Some(item) = sibling {
            if item.is::<gtk::ToggleButton>() {
                controls.push(item.clone());
            }
            sibling = item.next_sibling();
        }
        controls.extend([
            imp.broadcaster_filter.get().upcast(),
            imp.duration_filter.get().upcast(),
            imp.sort_button.get().upcast(),
        ]);
        controls
    }

    /// R07 isolated offline real Mediathek cards. No provider traffic or playback.
    #[cfg(test)]
    pub(crate) fn r07_install_offline_result_cards(&self) {
        use mediathekviewweb::models::Item;

        let imp = self.imp();
        // Invalidate any pending asynchronous initial query before installing
        // deterministic rows. Keep the normal ListBox model binding intact.
        imp.requests.borrow_mut().begin_search();
        let model = imp.shows_model();
        model.remove_all();
        for index in 0..3 {
            model.append(&ShowObject::new(Item {
                id: format!("r07-show-{index}"),
                channel: "R07".to_owned(),
                topic: "Offline navigation".to_owned(),
                title: format!("Offline show {index}"),
                description: Some("Disposable GTK focus fixture".to_owned()),
                timestamp: 0,
                duration: Some(Duration::from_secs(180)),
                size: None,
                url_website: "https://example.invalid/offline".to_owned(),
                url_subtitle: None,
                url_video: "https://example.invalid/offline.mp4".to_owned(),
                url_video_low: None,
                url_video_hd: None,
                filmliste_timestamp: 0,
            }));
        }
        imp.show_results();
    }

    #[cfg(test)]
    pub(crate) fn r07_result_row(&self, index: i32) -> gtk::ListBoxRow {
        self.imp()
            .results_list
            .row_at_index(index)
            .expect("R07 actual Mediathek result row")
    }

    #[cfg(test)]
    pub(crate) fn r07_result_id(&self, index: i32) -> Option<String> {
        self.imp()
            .results_list
            .row_at_index(index)?
            .downcast::<TvMediathekCard>()
            .ok()?
            .show()
            .map(|show| show.title())
    }

    pub fn focus_first_result(&self) -> bool {
        self.imp()
            .results_list
            .row_at_index(0)
            .is_some_and(|row| row.grab_focus())
    }

    pub fn focus_search(&self) {
        self.imp().search_entry.grab_focus();
    }

    /// Return to a graphical control, not the on-screen keyboard.
    pub fn focus_browse(&self) {
        self.imp().broadcaster_filter.grab_focus();
    }

    /// Collapse a focused programme without losing the current result list.
    pub fn collapse_focused(&self, focused: Option<gtk::Widget>) -> bool {
        let mut widget = focused;
        while let Some(current) = widget {
            if let Some(row) = current.downcast_ref::<TvMediathekCard>() {
                if row.expanded() {
                    row.set_expanded(false);
                    row.grab_focus();
                    return true;
                }
                return false;
            }
            widget = current.parent();
        }
        false
    }

    fn client(&self) -> Arc<Mediathek> {
        self.imp()
            .client
            .get_or_init(|| {
                Arc::new(
                    Mediathek::new(
                        format!("{APP_ID}/{VERSION} ({PROJECT_URL})")
                            .try_into()
                            .expect("invalid user agent"),
                    )
                    .expect("failed to initialize HTTP client"),
                )
            })
            .clone()
    }
    fn query_props(&self) -> QueryProperties {
        let duration_index = self.imp().duration_filter.selected();
        let broadcaster = self.selected_broadcaster();
        let query_string =
            compose_query(&self.query_string(), broadcaster.as_deref(), duration_index);
        let (duration_min, duration_max) = duration_bounds(duration_index);

        let search_everywhere = self.search_everywhere();
        let include_future = self.include_future();
        let sort_by = match &*self.sort_by() {
            "channel" => SortField::Channel,
            "date" | "timestamp" => SortField::Timestamp,
            "duration" => SortField::Duration,
            _ => SortField::Timestamp,
        };
        let sort_order = match &*self.sort_order() {
            "asc" | "ascending" => SortOrder::Ascending,
            "desc" | "descending" => SortOrder::Descending,
            _ => SortOrder::Descending,
        };

        QueryProperties {
            query_string,
            broadcaster,
            duration_min,
            duration_max,
            search_everywhere,
            include_future,
            sort_by,
            sort_order,
        }
    }
    async fn load(&self) {
        let imp = self.imp();
        let generation = imp.requests.borrow_mut().begin_search();

        let QueryProperties {
            query_string,
            broadcaster,
            duration_min,
            duration_max,
            search_everywhere,
            include_future,
            sort_by,
            sort_order,
        } = self.query_props();

        // Invalidate every previous page before issuing a replacement request.
        imp.shows_model().remove_all();
        self.set_total_results(0);
        self.set_more_available(false);
        self.action_set_enabled("mediathek.load-more", false);

        if query_string.is_empty()
            && broadcaster.is_none()
            && duration_min.is_none()
            && duration_max.is_none()
        {
            imp.show_status_page();
            return;
        }

        imp.show_loading();
        let client = self.client();

        let result = tokio(async move {
            complete_request(
                apply_duration_bounds(
                    build_search_query(
                        &client,
                        &query_string,
                        search_everywhere,
                        broadcaster.as_deref(),
                    )
                    .include_future(include_future)
                    .size(15)
                    .sort_by(sort_by)
                    .sort_order(sort_order),
                    duration_min,
                    duration_max,
                )
                .send(),
                SEARCH_TIMEOUT,
            )
            .await
        })
        .await;

        // a newer search has started in the meantime
        if !imp.requests.borrow().is_current(generation) {
            return;
        }

        match result {
            Ok(result) => {
                // must be known before the results change
                self.set_total_results(result.query_info.total_results);

                let shows: Vec<ShowObject> =
                    result.results.into_iter().map(ShowObject::new).collect();
                let shows_model = imp.shows_model();
                shows_model.splice(0, shows_model.n_items(), &shows);
                imp.show_results();
            }
            Err(_) => imp.show_error_page(),
        }
    }
    async fn load_more(&self) {
        let imp = self.imp();

        let QueryProperties {
            query_string,
            broadcaster,
            duration_min,
            duration_max,
            search_everywhere,
            include_future,
            sort_by,
            sort_order,
        } = self.query_props();

        if query_string.is_empty()
            && broadcaster.is_none()
            && duration_min.is_none()
            && duration_max.is_none()
        {
            imp.show_status_page();
            return;
        }

        if !self.more_available() {
            return;
        }

        let mut shows_model = imp.shows_model();
        let offset = shows_model.n_items();
        let restore_load_more_focus = imp.load_more_button.has_focus();
        // Also enforce this gate for programmatic actions while the button is
        // disabled: two requests at the same offset must never be issued.
        let Some(generation) = imp.requests.borrow_mut().begin_page(offset) else {
            return;
        };

        self.action_set_enabled("mediathek.load-more", false);
        let client = self.client();
        let result = tokio(async move {
            complete_request(
                apply_duration_bounds(
                    build_search_query(
                        &client,
                        &query_string,
                        search_everywhere,
                        broadcaster.as_deref(),
                    )
                    .include_future(include_future)
                    .size(15)
                    .offset(offset as usize)
                    .sort_by(sort_by)
                    .sort_order(sort_order),
                    duration_min,
                    duration_max,
                )
                .send(),
                SEARCH_TIMEOUT,
            )
            .await
        })
        .await;
        // Only the current page may update UI state. In particular, an
        // obsolete completion must not re-enable a newer page's action.
        if !imp.requests.borrow_mut().finish_page(generation, offset) {
            return;
        }

        match result {
            Ok(result) => {
                self.set_total_results(result.query_info.total_results);
                shows_model.extend(result.results.into_iter().map(ShowObject::new));
                imp.show_results();
                if restore_load_more_focus {
                    if self.more_available() {
                        imp.load_more_button.grab_focus();
                    } else if let Some(row) =
                        imp.results_list.row_at_index(offset as i32).or_else(|| {
                            imp.results_list
                                .row_at_index(shows_model.n_items().saturating_sub(1) as i32)
                        })
                    {
                        // The button disappears on the final page.
                        row.grab_focus();
                    }
                }
            }
            Err(_) => {
                self.action_set_enabled("mediathek.load-more", self.more_available());
                if restore_load_more_focus && self.more_available() {
                    imp.load_more_button.grab_focus();
                }
                show_error(&gettext("Failed to load more Mediathek results"));
            }
        }
    }
    pub fn reload(&self) {
        let slf = self.clone();
        spawn(async move { slf.load().await });
    }
}

/// Compare widget ancestry without depending on GTK's layout heuristics.
fn widget_contains_focus(ancestor: &gtk::Widget, focused: &gtk::Widget) -> bool {
    let mut current = Some(focused.clone());
    while let Some(widget) = current {
        if widget == *ancestor {
            return true;
        }
        current = widget.parent();
    }
    false
}

/// Locate the results row even when an expanded card action holds focus.
fn result_row_for_focus(focused: &gtk::Widget, list: &gtk::ListBox) -> Option<gtk::ListBoxRow> {
    let mut current = Some(focused.clone());
    while let Some(widget) = current {
        if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
            if row.parent().as_ref() == Some(list.upcast_ref()) {
                return Some(row.clone());
            }
        }
        current = widget.parent();
    }
    None
}

/// Find the internal popup belonging to a stock GtkDropDown. We keep GTK's
/// existing rows, models and selection behavior; this is only initial focus.
fn dropdown_popover(widget: &gtk::Widget) -> Option<gtk::Popover> {
    if let Some(popup) = widget.downcast_ref::<gtk::Popover>() {
        return Some(popup.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(popup) = dropdown_popover(&current) {
            return Some(popup);
        }
        child = current.next_sibling();
    }
    None
}

fn dropdown_listview(widget: &gtk::Widget) -> Option<gtk::ListView> {
    if let Some(list) = widget.downcast_ref::<gtk::ListView>() {
        return Some(list.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(list) = dropdown_listview(&current) {
            return Some(list);
        }
        child = current.next_sibling();
    }
    None
}

fn focus_dropdown_row(dropdown: &gtk::DropDown, popover: &gtk::Popover) {
    let Some(list) = dropdown_listview(popover.upcast_ref()) else {
        tracing::debug!("dropdown popup has no GtkListView to focus");
        return;
    };
    let Some(model) = list.model() else {
        return;
    };
    let count = model.n_items();
    if count == 0 {
        return;
    }
    // The selection is deliberately untouched; focus must not emit
    // selected-notify or reissue an upstream Mediathek search.
    list.scroll_to(
        dropdown.selected().min(count - 1),
        gtk::ListScrollFlags::FOCUS,
        None,
    );
}

fn install_dropdown_popup_focus(dropdown: &gtk::DropDown) {
    // The stock popup belongs to the dropdown widget; do not substitute a
    // custom menu or intercept ordinary arrow/Return events.
    let Some(popover) = dropdown_popover(dropdown.upcast_ref()) else {
        tracing::warn!("GtkDropDown popup not found while setting remote focus");
        return;
    };
    let weak_dropdown = dropdown.downgrade();
    popover.connect_map(move |popover| {
        let weak_dropdown = weak_dropdown.clone();
        let popover = popover.clone();
        // GTK can assign its own initial focus during popup mapping. Apply
        // our focus only after the map sequence has completed.
        glib::idle_add_local_once(move || {
            if popover.is_mapped() {
                if let Some(dropdown) = weak_dropdown.upgrade() {
                    focus_dropdown_row(&dropdown, &popover);
                }
            }
        });
    });
}

#[derive(Debug)]
struct QueryProperties {
    query_string: String,
    broadcaster: Option<String>,
    duration_min: Option<Duration>,
    duration_max: Option<Duration>,
    search_everywhere: bool,
    include_future: bool,
    sort_by: SortField,
    sort_order: SortOrder,
}

#[cfg(test)]
mod tests {
    use super::{
        append_channels_bounded, broadcaster_index, compose_query, duration_bounds,
        normalise_channels, separate_search_terms, ChannelsResponse, SearchRequests,
        FALLBACK_BROADCASTERS, MAX_CHANNELS_RESPONSE_BYTES,
    };
    use std::time::Duration;

    /// Verify the real Blueprint toolbar has a satisfiable minimum width
    /// and keeps all seven keyboard/remote destinations mapped at narrow,
    /// formerly failing and normal window sizes.
    #[test]
    #[ignore = "requires a mapped GTK display and isolated GSettings"]
    fn gtk_mediathek_toolbar_narrow_allocations() {
        use super::*;

        let bytes = glib::Bytes::from_static(crate::GRESOURCE_BYTES);
        let resource = gio::Resource::from_data(&bytes).expect("valid resource bundle");
        gio::resources_register(&resource);
        gtk::init().expect("GTK display");
        let provider = gtk::CssProvider::new();
        provider.load_from_resource("/io/github/Certain1989/Broadcaster/broadcaster.css");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        for width in [600, 630, 760, 1280] {
            let view: TvMediathekView = glib::Object::new();
            let window = gtk::Window::new();
            window.set_default_size(width, 720);
            window.set_child(Some(&view));
            window.present();
            for _ in 0..24 {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
            }
            let (minimum, _, _, _) = view.measure(gtk::Orientation::Horizontal, -1);
            assert!(
                minimum <= width,
                "toolbar requests at least {minimum}px within {width}px viewport"
            );
            let imp = view.imp();
            let filter_row = imp
                .broadcaster_filter
                .parent()
                .expect("broadcaster filter group")
                .parent()
                .expect("filter toolbar");
            let panel = filter_row.parent().expect("toolbar panel");
            let search_row = imp.search_entry.parent().expect("search row");
            assert!(
                panel.has_css_class("tv-controls"),
                "keep tinted Mediathek panel"
            );
            assert_eq!(search_row.parent(), Some(panel.clone()));
            // GtkWidget::height() is the CSS content-box allocation; the
            // surface's 10px CSS padding is outside this value. Only the
            // Blueprint's 6px inter-row gap belongs in this measurement.
            let vertical_overhead = panel.height() - search_row.height() - filter_row.height();
            assert!(
                (0..=10).contains(&vertical_overhead),
                "toolbar excessive vertical inset at {width}px: panel={} search={} filters={} extra={vertical_overhead}",
                panel.height(),
                search_row.height(),
                filter_row.height()
            );
            let controls = view.toolbar_focus_targets();
            assert_eq!(controls.len(), 7, "all search/filter controls must remain");
            for control in &controls {
                assert!(control.is_mapped(), "toolbar control unmapped at {width}px");
                let bounds = control
                    .compute_bounds(&view)
                    .expect("mapped control bounds");
                assert!(
                    bounds.x() >= -1.0 && bounds.x() + bounds.width() <= view.width() as f32 + 1.0,
                    "toolbar control clipped at {width}px: x={} width={} view={}",
                    bounds.x(),
                    bounds.width(),
                    view.width()
                );
                assert!(control.grab_focus(), "toolbar control must be reachable");
            }
            let keyboard = view.imp().keyboard_button.get();
            // Remote D-pad focus does not depend on a touch-size hitbox.
            // Test the real mapped button and its accessible focus target.
            assert!(
                keyboard.is_visible() && keyboard.is_sensitive() && keyboard.is_mapped(),
                "remote launcher must be visible, sensitive and mapped at {width}px"
            );
            assert!(
                keyboard.width() > 0 && keyboard.height() > 0,
                "remote launcher must have a nonzero allocation at {width}px"
            );
            assert!(
                !keyboard.has_css_class("tv-compact-keyboard-launcher"),
                "remove the former paint-clipping launcher workaround"
            );
            assert!(
                keyboard.grab_focus() && keyboard.has_focus(),
                "remote launcher must accept visible GTK focus at {width}px"
            );
            // All three icons should have the same compact allocation, rather
            // than a short icon button next to a taller search entry.
            for (name, toggle) in [
                ("Search Everywhere", &controls[2]),
                ("Include Future Shows", &controls[3]),
            ] {
                assert!(
                    (keyboard.width() - toggle.width()).abs() <= 2
                        && (keyboard.height() - toggle.height()).abs() <= 2,
                    "keyboard launcher {}x{} differs from {name} {}x{} at {width}px",
                    keyboard.width(),
                    keyboard.height(),
                    toggle.width(),
                    toggle.height()
                );
            }
            let search_height = controls[0].height();
            let icon_height = keyboard.height();
            assert!(
                (0..=8).contains(&(search_height - icon_height)),
                "search and toolbar icon heights unbalanced at {width}px: search={search_height}, icon={icon_height}"
            );
            window.destroy();
        }
    }

    /// Capture real GTK allocations, rather than comparing height requests,
    /// for the three themed toolbar variants under one display and scale.
    #[test]
    #[ignore = "requires a mapped GTK display and isolated GSettings"]
    fn gtk_three_view_toolbar_geometry() {
        use super::*;

        let bytes = glib::Bytes::from_static(crate::GRESOURCE_BYTES);
        let resource = gio::Resource::from_data(&bytes).expect("valid resource bundle");
        gio::resources_register(&resource);
        gtk::init().expect("GTK display");
        let provider = gtk::CssProvider::new();
        provider.load_from_resource("/io/github/Certain1989/Broadcaster/broadcaster.css");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        // Live's async channel loader expects the normal application context,
        // even when only its toolbar is being measured.
        let _app = crate::application::TvApplication::new();

        fn allocate(widget: &impl IsA<gtk::Widget>, width: i32) -> gtk::Window {
            let window = gtk::Window::new();
            window.set_default_size(width, 720);
            window.set_child(Some(widget));
            window.present();
            for _ in 0..24 {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
            }
            window
        }

        fn report(
            name: &str,
            width: i32,
            panel: &gtk::Widget,
            entry: &gtk::Widget,
            keyboard: &gtk::Widget,
            search_row: &gtk::Widget,
            second_row: Option<&gtk::Widget>,
        ) {
            let entry_bounds = entry.compute_bounds(panel).expect("mapped entry");
            let button_bounds = keyboard.compute_bounds(panel).expect("mapped keyboard");
            println!(
                "UX02 geometry {name} width={width} panel={}x{} row={} entry={}x{} at {:.0},{:.0} keyboard={}x{} at {:.0},{:.0} second_row={}",
                panel.width(), panel.height(), search_row.height(), entry.width(), entry.height(),
                entry_bounds.x(), entry_bounds.y(), keyboard.width(), keyboard.height(),
                button_bounds.x(), button_bounds.y(), second_row.map_or(0, gtk::Widget::height)
            );
        }

        for width in [600, 630, 760, 1280] {
            let mediathek: TvMediathekView = glib::Object::new();
            let window = allocate(&mediathek, width);
            let targets = mediathek.toolbar_focus_targets();
            let search_row = targets[0].parent().expect("Mediathek search row");
            let panel = search_row.parent().expect("Mediathek panel");
            let mediathek_entry_height = targets[0].height();
            let mediathek_keyboard_size = (targets[1].width(), targets[1].height());
            let mediathek_icon_sizes = [
                (targets[2].width(), targets[2].height()),
                (targets[3].width(), targets[3].height()),
            ];
            let mediathek_channel_height = targets[4].height();
            let mediathek_duration_height = targets[5].height();
            let mediathek_sort_height = targets[6].height();
            let mediathek_toggle_heights = [targets[2].height(), targets[3].height()];
            let filter_row = mediathek
                .imp()
                .broadcaster_filter
                .parent()
                .expect("Mediathek group")
                .parent()
                .expect("Mediathek filters");
            report(
                "Mediathek",
                width,
                &panel,
                &targets[0],
                &targets[1],
                &search_row,
                Some(&filter_row),
            );
            let mediathek_panel_width = panel.width();
            window.destroy();

            let live: crate::live::TvLiveView = glib::Object::new();
            let window = allocate(&live, width);
            // Let the initial asynchronous reload begin, then explicitly
            // show the offline test toolbar before the GDK allocation tick.
            live.show_toolbar_for_test();
            window.queue_allocate();
            for _ in 0..20 {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
            let targets = live.toolbar_focus_targets();
            assert!(
                targets.iter().all(gtk::Widget::is_mapped),
                "Live controls must be mapped before comparing allocations at {width}px"
            );
            let live_entry_height = targets[0].height();
            let live_keyboard_size = (targets[1].width(), targets[1].height());
            let panel = targets[0].parent().expect("Live panel");
            report(
                "Live",
                width,
                &panel,
                &targets[0],
                &targets[1],
                &panel,
                None,
            );
            let live_panel_width = panel.width();
            window.destroy();

            let streams: crate::streams::view::TvStreamsView = glib::Object::new();
            let window = allocate(&streams, width);
            let targets = streams.toolbar_focus_targets();
            let streams_entry_height = targets[0].height();
            let streams_keyboard = targets.last().expect("My Channels keyboard");
            let streams_keyboard_size = (streams_keyboard.width(), streams_keyboard.height());
            let search_row = targets[0].parent().expect("My Channels search row");
            let panel = search_row.parent().expect("My Channels panel");
            let filter_row = targets[1].parent().expect("My Channels filters");
            report(
                "My Channels",
                width,
                &panel,
                &targets[0],
                targets.last().expect("My Channels keyboard"),
                &search_row,
                Some(&filter_row),
            );
            let streams_panel_width = panel.width();
            let streams_filter_height = targets[1].height();
            let streams_sort_height = targets[3].height();
            let streams_action_height = targets[7].height();
            window.destroy();

            // All three launchers use the same compact size requests, but
            // actual GTK allocations (and their relationship to the search
            // entries) establish the visual parity; the two Mediathek toggles
            // must also match the keyboard in the narrow-allocation test.
            println!(
                "UX02 launcher width={width} mapped Mediathek={mediathek_keyboard_size:?} adjacent={mediathek_icon_sizes:?} Live={live_keyboard_size:?} My Channels={streams_keyboard_size:?}"
            );
            let mediathek_gap = mediathek_entry_height - mediathek_keyboard_size.1;
            for (name, reference, reference_gap) in [
                (
                    "Live",
                    live_keyboard_size,
                    live_entry_height - live_keyboard_size.1,
                ),
                (
                    "My Channels",
                    streams_keyboard_size,
                    streams_entry_height - streams_keyboard_size.1,
                ),
            ] {
                assert!(
                    (mediathek_keyboard_size.0 - reference.0).abs() <= 2
                        && (mediathek_keyboard_size.1 - reference.1).abs() <= 2,
                    "Mediathek keyboard {mediathek_keyboard_size:?} differs from {name} {reference:?} at {width}px"
                );
                assert!(
                    (mediathek_gap - reference_gap).abs() <= 4,
                    "search-to-keyboard height balance differs at {width}px: Mediathek gap={mediathek_gap}, {name} gap={reference_gap}"
                );
            }

            // GtkWidget widths exclude CSS padding. At equal viewport sizes,
            // Mediathek and My Channels use identical outer 12px margins and
            // must expose identical content widths; Live's 6px margins leave
            // exactly 12px more available for its corresponding content.
            println!(
                "UX02 compact width={width} panel content widths: Mediathek={mediathek_panel_width}, Live={live_panel_width}, My Channels={streams_panel_width}; heights: channel={mediathek_channel_height}, duration={mediathek_duration_height}, sort={mediathek_sort_height}, toggles={mediathek_toggle_heights:?}; reference: filter={streams_filter_height}, sort={streams_sort_height}, action={streams_action_height}"
            );
            assert!(
                mediathek_panel_width == streams_panel_width
                    && live_panel_width == mediathek_panel_width + 12,
                "different panel content widths at {width}px: Mediathek={mediathek_panel_width}, Live={live_panel_width}, My Channels={streams_panel_width}"
            );
            assert!(
                (mediathek_channel_height - streams_filter_height).abs() <= 6
                    && (mediathek_duration_height - streams_filter_height).abs() <= 6
                    && (mediathek_sort_height - streams_sort_height).abs() <= 6
                    // The search-row icon toggles are comparable to the
                    // My Channels search-row keyboard, not to its unrelated
                    // smaller Clear Filters / Manage Sources action buttons.
                    && mediathek_toggle_heights
                        .iter()
                        .all(|h| (*h - streams_keyboard_size.1).abs() <= 2),
                "Mediathek filters/search icons must match compact My Channels at {width}px"
            );

            // Compare mapped GtkSearchEntry allocations, not width/height requests.
            // Distinct theme styling may differ by a few pixels; a stretched
            // Mediathek field must not be as tall as its keyboard row.
            assert!(
                (mediathek_entry_height - live_entry_height).abs() <= 4
                    && (mediathek_entry_height - streams_entry_height).abs() <= 4,
                "search height mismatch at {width}px: Mediathek={mediathek_entry_height}, Live={live_entry_height}, My Channels={streams_entry_height}"
            );
        }
    }

    #[test]
    fn catalogue_body_limit_accepts_exact_boundary_across_chunks() {
        let mut body = Vec::new();
        append_channels_bounded(&mut body, &vec![b'a'; MAX_CHANNELS_RESPONSE_BYTES - 1]).unwrap();
        append_channels_bounded(&mut body, b"b").unwrap();
        assert_eq!(body.len(), MAX_CHANNELS_RESPONSE_BYTES);
    }

    #[test]
    fn catalogue_body_limit_rejects_oversize_chunk_without_appending() {
        let mut body = vec![b'a'; MAX_CHANNELS_RESPONSE_BYTES - 1];
        let error = append_channels_bounded(&mut body, b"bc").unwrap_err();
        assert!(error.to_string().contains("exceeds the allowed size"));
        assert_eq!(body.len(), MAX_CHANNELS_RESPONSE_BYTES - 1);
    }

    #[test]
    fn catalogue_body_limit_rejects_single_oversize_chunk() {
        let mut body = Vec::new();
        assert!(
            append_channels_bounded(&mut body, &vec![0; MAX_CHANNELS_RESPONSE_BYTES + 1]).is_err()
        );
        assert!(body.is_empty());
    }

    /// Exercise the *real* Mediathek template and GTK focus instead of only
    /// checking which ListBoxRow owns a nested widget. This specifically
    /// catches the previous Search -> Results shortcut that skipped filters.
    #[test]
    #[ignore = "needs GTK/Xvfb and isolated GSettings; CI runs it with a display"]
    fn gtk_tv_remote_vertical_focus_follows_all_filters() {
        use super::*;

        let bytes = glib::Bytes::from_static(crate::GRESOURCE_BYTES);
        let resource = gio::Resource::from_data(&bytes).expect("valid resource bundle");
        gio::resources_register(&resource);
        gtk::init().expect("GTK display");

        let view: TvMediathekView = glib::Object::new();
        let window = gtk::Window::new();
        window.set_default_size(1360, 900);
        window.set_child(Some(&view));

        // Detach the normal search model and simulate two results offline.
        // Actual template controls and focus behavior are still exercised.
        let imp = view.imp();
        imp.results_list.bind_model(None::<&gio::ListStore>, |_| {
            gtk::ListBoxRow::new().upcast::<gtk::Widget>()
        });
        let first = gtk::ListBoxRow::new();
        let first_action = gtk::Button::with_label("Play");
        first.set_child(Some(&first_action));
        let second = gtk::ListBoxRow::new();
        let second_action = gtk::Button::with_label("Download");
        second.set_child(Some(&second_action));
        imp.results_list.append(&first);
        imp.results_list.append(&second);
        imp.stack.set_visible_child(&*imp.results_view);
        window.present();
        for _ in 0..24 {
            if !glib::MainContext::default().iteration(false) {
                break;
            }
        }

        let focused = || gtk::prelude::GtkWindowExt::focus(&window).expect("a widget has focus");
        let assert_focus = |expected: &gtk::Widget| {
            assert!(
                widget_contains_focus(expected, &focused()),
                "focus must be on the requested toolbar control/row"
            );
        };
        let down = || view.focus_vertical_navigation(&focused(), gtk::gdk::Key::Down);
        let up = || view.focus_vertical_navigation(&focused(), gtk::gdk::Key::Up);

        assert!(imp.search_entry.grab_focus());
        assert!(down());
        assert_focus(imp.keyboard_button.upcast_ref());
        assert!(down());
        assert_focus(imp.broadcaster_filter.upcast_ref());
        assert!(down());
        assert_focus(imp.duration_filter.upcast_ref());
        assert!(down());
        assert_focus(imp.sort_button.upcast_ref());
        assert!(down());
        assert_focus(first.upcast_ref());

        assert!(second_action.grab_focus());
        assert!(up());
        assert_focus(first.upcast_ref());
        assert!(first_action.grab_focus());
        assert!(up());
        assert_focus(imp.sort_button.upcast_ref());
        assert!(up());
        assert_focus(imp.duration_filter.upcast_ref());
        assert!(up());
        assert_focus(imp.broadcaster_filter.upcast_ref());
        assert!(up());
        assert_focus(imp.keyboard_button.upcast_ref());
        assert!(up());
        assert_focus(imp.search_entry.upcast_ref());

        // Search + Up belongs to the window's page-switcher, not the view.
        assert!(!up());
        window.close();
    }

    #[tokio::test]
    async fn stalled_backend_returns_a_timeout_instead_of_hanging() {
        let stalled = std::future::pending::<Result<(), std::io::Error>>();
        let error = super::complete_request(stalled, Duration::from_millis(5))
            .await
            .expect_err("stalled request must time out");
        assert!(format!("{error:#}").contains("timed out"));
    }

    #[tokio::test]
    async fn backend_failure_preserves_an_actionable_error() {
        let request = async { Err::<(), _>(std::io::Error::other("upstream unavailable")) };
        let error = super::complete_request(request, Duration::from_secs(1))
            .await
            .expect_err("backend failure must propagate");
        assert!(format!("{error:#}").contains("upstream unavailable"));
    }

    #[test]
    fn pagination_allows_only_one_page_at_a_time() {
        let mut state = SearchRequests::default();
        let generation = state.begin_search();
        assert_eq!(state.begin_page(15), Some(generation));
        assert_eq!(state.begin_page(15), None);
        assert_eq!(state.begin_page(30), None);
        assert!(state.finish_page(generation, 15));
        assert_eq!(state.begin_page(30), Some(generation));
        assert!(state.finish_page(generation, 30));
    }

    #[test]
    fn stale_page_cannot_release_a_new_search_page() {
        let mut state = SearchRequests::default();
        let previous = state.begin_search();
        assert_eq!(state.begin_page(15), Some(previous));

        let current = state.begin_search();
        assert!(!state.is_current(previous));
        assert!(state.is_current(current));
        assert_eq!(state.begin_page(15), Some(current));

        assert!(!state.finish_page(previous, 15));
        assert_eq!(state.begin_page(30), None);
        assert!(state.finish_page(current, 15));
        assert_eq!(state.begin_page(30), Some(current));
    }

    #[test]
    fn stale_page_offset_cannot_clear_current_request() {
        let mut state = SearchRequests::default();
        let generation = state.begin_search();
        assert_eq!(state.begin_page(15), Some(generation));
        assert!(!state.finish_page(generation, 30));
        assert_eq!(state.begin_page(30), None);
        assert!(state.finish_page(generation, 15));
    }

    #[test]
    fn selectors_do_not_leak_into_plain_text_search() {
        assert_eq!(
            separate_search_terms("Tatort !ZDF >60"),
            ("!ZDF >60".to_string(), "Tatort".to_string())
        );
        assert_eq!(
            separate_search_terms("#Wissen +Schokolade ARTE"),
            ("#Wissen +Schokolade".to_string(), "ARTE".to_string())
        );
    }

    #[test]
    fn plain_words_remain_one_expression() {
        assert_eq!(
            separate_search_terms("Natur und Technik !ARD"),
            ("!ARD".to_string(), "Natur und Technik".to_string())
        );
        assert_eq!(
            separate_search_terms("!WDR >soon <later <60"),
            ("!WDR <60".to_string(), ">soon <later".to_string())
        );
        assert_eq!(separate_search_terms(""), (String::new(), String::new()));
    }

    #[test]
    fn plain_and_advanced_text_remain_available() {
        assert_eq!(compose_query("  Tatort  ", None, 0), "Tatort");
        assert_eq!(compose_query("!ZDF #heute >30", None, 0), "!ZDF #heute >30");
    }

    #[test]
    fn indexed_catalogue_is_deterministic_and_deduplicated() {
        let channels = normalise_channels([
            "phoenix".to_string(),
            " ZDF ".to_string(),
            "".to_string(),
            "ARD".to_string(),
            "zdf".to_string(),
            "Radio Bremen TV".to_string(),
        ]);
        let identifiers: Vec<&str> = channels
            .iter()
            .map(|item| item.identifier.as_str())
            .collect();
        assert_eq!(identifiers, ["ARD", "phoenix", "Radio Bremen TV", "ZDF"]);
        assert!(channels.iter().all(|item| !item.identifier.is_empty()));
        assert!(channels.iter().all(|item| item.label == item.identifier));
        assert!(FALLBACK_BROADCASTERS.contains(&"phoenix"));
    }

    #[test]
    fn selection_is_preserved_across_catalogue_refreshes() {
        let previous = normalise_channels(["phoenix".into(), "ZDF".into()]);
        let updated = normalise_channels(["ARD".into(), "phoenix".into(), "ZDF".into()]);
        assert_eq!(broadcaster_index(&previous, Some("phoenix")), 1);
        assert_eq!(broadcaster_index(&updated, Some("phoenix")), 2);
        assert_eq!(broadcaster_index(&updated, Some("removed")), 0);
        assert_eq!(broadcaster_index(&updated, None), 0);
    }

    #[test]
    fn upstream_channel_response_matches_documented_shape() {
        let response: ChannelsResponse =
            serde_json::from_str(r#"{"error":null,"channels":["phoenix","Radio Bremen TV"]}"#)
                .unwrap();
        assert!(response.error.is_none());
        assert_eq!(response.channels.unwrap(), ["phoenix", "Radio Bremen TV"]);
        let failed: ChannelsResponse =
            serde_json::from_str(r#"{"error":"backend unavailable","channels":null}"#).unwrap();
        assert!(failed.channels.is_none());
    }

    #[test]
    fn filter_only_searches_use_backend_selectors() {
        assert_eq!(compose_query("", Some("ZDF"), 1), "");
        assert_eq!(compose_query("", None, 3), "");
    }

    #[test]
    fn graphical_filters_override_only_their_own_dimension() {
        assert_eq!(compose_query("!ARD #Tatort >80", Some("ZDF"), 2), "#Tatort");
        assert_eq!(compose_query("!ARD #Tatort", None, 0), "!ARD #Tatort");
    }

    #[test]
    fn query_composition_retains_other_advanced_selectors() {
        assert_eq!(
            compose_query("#Wissen +Schokolade", Some("ARTE"), 0),
            "#Wissen +Schokolade"
        );
    }

    #[test]
    fn duration_preset_bounds_are_second_accurate() {
        assert_eq!(duration_bounds(0), (None, None));
        assert_eq!(duration_bounds(1), (None, Some(Duration::from_secs(900))));
        assert_eq!(duration_bounds(2).0, Some(Duration::from_secs(899)));
        assert_eq!(duration_bounds(2).1, Some(Duration::from_secs(3601)));
        assert_eq!(duration_bounds(3), (Some(Duration::from_secs(3600)), None));
    }

    #[test]
    fn manual_duration_syntax_survives_without_a_preset() {
        assert_eq!(
            compose_query("!ARD #Tatort >80 <120", None, 0),
            "!ARD #Tatort >80 <120"
        );
        assert_eq!(
            compose_query("!ARD #Tatort >80 <120", None, 2),
            "!ARD #Tatort"
        );
    }

    #[test]
    fn graphical_duration_retains_nonnumeric_prefix_text() {
        assert_eq!(
            compose_query("!ARD >soon <later >30", None, 2),
            "!ARD >soon <later"
        );
    }

    /// Exercise the real mediathekviewweb request serializer against a local
    /// HTTP endpoint. An error response avoids relying on any live archive.
    async fn capture_backend_request(
        text: &str,
        broadcaster: Option<&str>,
        duration: u32,
        offset: Option<usize>,
    ) -> serde_json::Value {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];

            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0, "request terminated before body arrived");
                request.extend_from_slice(&buffer[..count]);

                let Some(headers_end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let headers = std::str::from_utf8(&request[..headers_end]).unwrap();
                assert!(headers.starts_with("POST /api/query HTTP/1.1"));
                let content_length = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .expect("missing Content-Length");

                let body_start = headers_end + 4;
                if request.len() < body_start + content_length {
                    continue;
                }

                let payload =
                    serde_json::from_slice(&request[body_start..body_start + content_length])
                        .unwrap();
                socket
                    .write_all(
                        b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .unwrap();
                return payload;
            }
        });

        let client = mediathekviewweb::Mediathek::new_with_url(
            format!("http://{address}"),
            "Broadcaster request test".parse().unwrap(),
        )
        .unwrap();
        let query_text = compose_query(text, broadcaster, duration);
        let (min, max) = duration_bounds(duration);
        let mut query = super::apply_duration_bounds(
            super::build_search_query(&client, &query_text, false, broadcaster)
                .include_future(false)
                .size(15)
                .sort_by(mediathekviewweb::models::SortField::Timestamp)
                .sort_order(mediathekviewweb::models::SortOrder::Descending),
            min,
            max,
        );
        if let Some(offset) = offset {
            query = query.offset(offset);
        }

        let response = tokio::time::timeout(Duration::from_secs(10), query.send())
            .await
            .expect("mock request timed out");
        assert!(response.is_err(), "mock endpoint must reject the request");

        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("mock server timed out")
            .expect("mock server failed")
    }

    #[tokio::test]
    async fn backend_request_separates_advanced_terms_and_preserves_pagination() {
        let payload = capture_backend_request(
            "!ARD #Wissen >80 Natur und Technik",
            Some("ZDF"),
            2,
            Some(15),
        )
        .await;
        let clauses: Vec<&str> = payload["queries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|clause| clause["query"].as_str().unwrap())
            .collect();

        assert_eq!(clauses, ["Wissen", "ZDF", "Natur und Technik"]);
        assert_eq!(payload["duration_min"].as_u64(), Some(899));
        assert_eq!(payload["duration_max"].as_u64(), Some(3601));
        assert_eq!(payload["size"].as_u64(), Some(15));
        assert_eq!(payload["offset"].as_u64(), Some(15));
        assert_eq!(payload["future"].as_bool(), Some(false));
        assert_eq!(payload["sortBy"], "timestamp");
        assert_eq!(payload["sortOrder"], "desc");
        assert_eq!(
            payload["queries"][0]["fields"],
            serde_json::json!(["topic"])
        );
        assert_eq!(
            payload["queries"][1]["fields"],
            serde_json::json!(["channel"])
        );
        assert_eq!(
            payload["queries"][2]["fields"],
            serde_json::json!(["topic", "title"])
        );
    }

    #[tokio::test]
    async fn backend_request_allows_duration_only_search_without_text() {
        let payload = capture_backend_request("", None, 1, None).await;
        assert!(payload["queries"].as_array().unwrap().is_empty());
        assert_eq!(payload["duration_max"].as_u64(), Some(900));
        assert!(payload.get("duration_min").is_none());
        assert!(payload.get("offset").is_none());
        assert_eq!(payload["size"].as_u64(), Some(15));
    }
    #[tokio::test]
    async fn backend_request_accepts_multitoken_channel_identifier() {
        let payload = capture_backend_request("", Some("Radio Bremen TV"), 0, None).await;
        assert_eq!(payload["queries"].as_array().unwrap().len(), 1);
        assert_eq!(payload["queries"][0]["query"], "Radio Bremen TV");
        assert_eq!(
            payload["queries"][0]["fields"],
            serde_json::json!(["channel"])
        );
    }

    #[tokio::test]
    async fn backend_request_plain_text_and_broadcaster_only() {
        let plain = capture_backend_request("Natur und Technik", None, 0, None).await;
        assert_eq!(plain["queries"].as_array().unwrap().len(), 1);
        assert_eq!(plain["queries"][0]["query"], "Natur und Technik");
        assert_eq!(
            plain["queries"][0]["fields"],
            serde_json::json!(["topic", "title"])
        );

        let broadcaster = capture_backend_request("", Some("ZDF"), 0, None).await;
        assert_eq!(broadcaster["queries"].as_array().unwrap().len(), 1);
        assert_eq!(broadcaster["queries"][0]["query"], "ZDF");
        assert_eq!(
            broadcaster["queries"][0]["fields"],
            serde_json::json!(["channel"])
        );
    }
}

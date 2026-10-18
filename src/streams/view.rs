// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Playable My Channels vertical slice. Network/SQLite calls are exclusively
//! through the asynchronous service; stale generations cannot mutate GTK.
use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::{HashMap, HashSet},
    time::Duration,
};

use adw::{glib, gtk, prelude::*, subclass::prelude::*};
use gettextrs::gettext;

use crate::{
    application::{TvApplication, VideoInfo},
    utils::spawn,
};

use super::{
    model::{
        ChannelId, ChannelPage, ChannelQuery, FacetSelection, FacetValue, FilterDimension,
        FilterOptions, SortOrder, Source,
    },
    rows::{StreamRows, StreamSlot, PAGE_SIZE},
    service::StreamsService,
};

#[derive(Clone)]
struct RowUi {
    title: gtk::Label,
    summary: gtk::Label,
    details: gtk::Label,
    reveal: gtk::Revealer,
    play: gtk::Button,
    edit: gtk::Button,
}

struct BoundRow {
    ui: RowUi,
    connection: Option<(StreamSlot, glib::SignalHandlerId)>,
}

struct InstalledFacets {
    sources: Vec<Source>,
    source_options: FilterOptions,
    group_options: FilterOptions,
    language_options: FilterOptions,
    country_options: FilterOptions,
    region_options: FilterOptions,
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(file = "src/streams/view.blp")]
    pub struct TvStreamsView {
        #[template_child]
        pub(super) search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub(super) filter_bar: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) advanced_filter_bar: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) actions_bar: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) source_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub(super) group_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub(super) language_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub(super) country_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub(super) region_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub(super) clear_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) manage_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) sort_filter: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub(super) add_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) count_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) results_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(super) results_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub(super) empty_page: TemplateChild<adw::StatusPage>,

        pub(super) compact: Cell<bool>,
        pub(super) service: RefCell<Option<StreamsService>>,
        pub(super) rows: OnceCell<StreamRows>,
        pub(super) selection: OnceCell<gtk::SingleSelection>,
        pub(super) query: RefCell<ChannelQuery>,
        pub(super) generation: Cell<u64>,
        pub(super) search_serial: Cell<u64>,
        pub(super) suppress_controls: Cell<bool>,
        pub(super) in_flight: RefCell<HashSet<u32>>,
        pub(super) source_values: RefCell<Vec<Option<String>>>,
        pub(super) group_values: RefCell<Vec<FacetSelection>>,
        pub(super) language_values: RefCell<Vec<FacetSelection>>,
        pub(super) country_values: RefCell<Vec<FacetSelection>>,
        pub(super) region_values: RefCell<Vec<FacetSelection>>,
        pub(super) selected_id: RefCell<Option<ChannelId>>,
        pub(super) expanded_id: RefCell<Option<ChannelId>>,
        pub(super) last_index: Cell<u32>,
        pub(super) bindings: RefCell<HashMap<usize, BoundRow>>,
        #[cfg(test)]
        pub(super) fake_player_enabled: Cell<bool>,
        #[cfg(test)]
        pub(super) fake_player_calls: Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvStreamsView {
        const NAME: &'static str = "TvStreamsView";
        type Type = super::TvStreamsView;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }
        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TvStreamsView {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPS: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPS.get_or_init(|| vec![glib::ParamSpecBoolean::builder("compact").build()])
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "compact" => self.compact.get().to_value(),
                _ => unreachable!("unknown streams property"),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "compact" => {
                    let compact = value.get::<bool>().expect("compact is a boolean");
                    self.compact.set(compact);
                    let orientation = if compact {
                        gtk::Orientation::Vertical
                    } else {
                        gtk::Orientation::Horizontal
                    };
                    self.filter_bar.set_orientation(orientation);
                    self.advanced_filter_bar.set_orientation(orientation);
                    self.actions_bar.set_orientation(orientation);
                }
                _ => unreachable!("unknown streams property"),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().initialize();
        }
    }
    impl WidgetImpl for TvStreamsView {}
    impl BinImpl for TvStreamsView {}
}

glib::wrapper! {
    pub struct TvStreamsView(ObjectSubclass<imp::TvStreamsView>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TvStreamsView {
    fn initialize(&self) {
        let imp = self.imp();
        let rows = StreamRows::new();
        let selection = gtk::SingleSelection::new(Some(rows.clone()));
        selection.set_autoselect(true);
        selection.set_can_unselect(false);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, item| view.setup_row(item)
        ));
        factory.connect_bind(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, item| view.bind_row(item)
        ));
        factory.connect_unbind(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, item| view.unbind_row(item)
        ));
        factory.connect_teardown(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, item| view.teardown_row(item)
        ));
        imp.results_list.set_factory(Some(&factory));
        imp.results_list.set_model(Some(&selection));
        imp.rows.set(rows).expect("rows only initialized once");
        imp.selection
            .set(selection.clone())
            .expect("selection only initialized once");

        imp.source_values.replace(vec![None]);
        imp.group_values.replace(vec![FacetSelection::Any]);
        imp.language_values.replace(vec![FacetSelection::Any]);
        imp.country_values.replace(vec![FacetSelection::Any]);
        imp.region_values.replace(vec![FacetSelection::Any]);
        self.update_dropdown(&imp.source_filter, &[gettext("All sources")], 0);
        self.update_dropdown(&imp.group_filter, &[gettext("All groups")], 0);
        self.update_dropdown(&imp.language_filter, &[gettext("All languages")], 0);
        self.update_dropdown(&imp.country_filter, &[gettext("All countries")], 0);
        self.update_dropdown(&imp.region_filter, &[gettext("All regions")], 0);
        self.update_dropdown(
            &imp.sort_filter,
            &[gettext("Playlist order"), gettext("Name"), gettext("Group")],
            0,
        );
        imp.source_filter.connect_selected_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |dropdown| {
                if view.imp().suppress_controls.get() {
                    return;
                }
                let selected = dropdown.selected() as usize;
                if let Some(source) = view.imp().source_values.borrow().get(selected).cloned() {
                    view.imp().query.borrow_mut().source_id = source;
                    view.reload();
                }
            }
        ));
        imp.group_filter.connect_selected_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |dropdown| {
                if view.imp().suppress_controls.get() {
                    return;
                }
                let selected = dropdown.selected() as usize;
                if let Some(group) = view.imp().group_values.borrow().get(selected).cloned() {
                    view.imp().query.borrow_mut().group = group;
                    view.reload();
                }
            }
        ));
        imp.language_filter.connect_selected_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |dropdown| {
                if view.imp().suppress_controls.get() {
                    return;
                }
                let index = dropdown.selected() as usize;
                if let Some(selected) = view.imp().language_values.borrow().get(index).cloned() {
                    view.imp().query.borrow_mut().language = selected;
                    view.reload();
                }
            }
        ));
        imp.country_filter.connect_selected_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |dropdown| {
                if view.imp().suppress_controls.get() {
                    return;
                }
                let index = dropdown.selected() as usize;
                if let Some(selected) = view.imp().country_values.borrow().get(index).cloned() {
                    view.imp().query.borrow_mut().country = selected;
                    view.reload();
                }
            }
        ));
        imp.region_filter.connect_selected_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |dropdown| {
                if view.imp().suppress_controls.get() {
                    return;
                }
                let index = dropdown.selected() as usize;
                if let Some(selected) = view.imp().region_values.borrow().get(index).cloned() {
                    view.imp().query.borrow_mut().region = selected;
                    view.reload();
                }
            }
        ));
        imp.sort_filter.connect_selected_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |dropdown| {
                if view.imp().suppress_controls.get() {
                    return;
                }
                view.imp().query.borrow_mut().sort = match dropdown.selected() {
                    1 => SortOrder::Name,
                    2 => SortOrder::Group,
                    _ => SortOrder::PlaylistOrder,
                };
                view.reload();
            }
        ));
        imp.clear_button.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                view.imp().suppress_controls.set(true);
                view.imp()
                    .search_serial
                    .set(view.imp().search_serial.get().wrapping_add(1));
                *view.imp().query.borrow_mut() = ChannelQuery::default();
                view.imp().search_entry.set_text("");
                view.imp().suppress_controls.set(false);
                view.reload();
            }
        ));
        imp.search_entry.connect_search_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                if view.imp().suppress_controls.get() {
                    return;
                }
                let serial = view.imp().search_serial.get().wrapping_add(1);
                view.imp().search_serial.set(serial);
                glib::timeout_add_local_once(
                    Duration::from_millis(250),
                    glib::clone!(
                        #[weak]
                        view,
                        move || {
                            if view.imp().search_serial.get() != serial {
                                return;
                            }
                            let text = view.imp().search_entry.text().to_string();
                            let text: String = text.chars().take(256).collect();
                            view.imp().query.borrow_mut().search =
                                (!text.is_empty()).then_some(text);
                            view.reload();
                        }
                    ),
                );
            }
        ));
        selection.connect_selected_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |selection| {
                if view.imp().suppress_controls.get() {
                    return;
                }
                let index = selection.selected();
                if index == gtk::INVALID_LIST_POSITION {
                    return;
                }
                view.imp().last_index.set(index);
                if let Some(channel) = view.imp().rows.get().and_then(|r| r.channel(index)) {
                    view.imp().selected_id.replace(Some(channel.id));
                } else {
                    view.request_page(index);
                }
            }
        ));
        imp.results_list.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, position| view.activate_position(position)
        ));
        imp.add_button.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| super::management::show_add_menu(&view)
        ));

        imp.manage_button.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| super::management::show_manager(&view)
        ));
        imp.results_stack.set_visible_child_name("loading");
        #[cfg(not(test))]
        spawn(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                match StreamsService::open().await {
                    Ok(service) => {
                        view.imp().service.replace(Some(service));
                        view.reload();
                    }
                    Err(_) => view.imp().results_stack.set_visible_child_name("error"),
                }
            }
        ));
    }

    fn update_dropdown(&self, dropdown: &gtk::DropDown, labels: &[String], selected: u32) {
        let references: Vec<&str> = labels.iter().map(String::as_str).collect();
        let model = gtk::StringList::new(&references);
        dropdown.set_model(Some(&model));
        dropdown.set_selected(selected);
    }

    pub fn reload(&self) {
        let imp = self.imp();
        let Some(service) = imp.service.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get().wrapping_add(1);
        imp.generation.set(generation);
        imp.in_flight.borrow_mut().clear();
        imp.results_stack.set_visible_child_name("loading");
        let mut query = imp.query.borrow().clone();
        query.limit = PAGE_SIZE;
        query.offset = 0;
        spawn(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let (
                    page,
                    sources,
                    source_options,
                    group_options,
                    language_options,
                    country_options,
                    region_options,
                ) = futures_util::join!(
                    service.query(query.clone()),
                    service.list_sources(),
                    service.filter_options(query.clone(), FilterDimension::Source),
                    service.filter_options(query.clone(), FilterDimension::Group),
                    service.filter_options(query.clone(), FilterDimension::Language),
                    service.filter_options(query.clone(), FilterDimension::Country),
                    service.filter_options(query.clone(), FilterDimension::Region),
                );
                if view.imp().generation.get() != generation {
                    return;
                }
                let (
                    Ok(page),
                    Ok(sources),
                    Ok(source_options),
                    Ok(group_options),
                    Ok(language_options),
                    Ok(country_options),
                    Ok(region_options),
                ) = (
                    page,
                    sources,
                    source_options,
                    group_options,
                    language_options,
                    country_options,
                    region_options,
                )
                else {
                    view.imp().results_stack.set_visible_child_name("error");
                    return;
                };
                view.apply_results(
                    query,
                    generation,
                    page,
                    InstalledFacets {
                        sources,
                        source_options,
                        group_options,
                        language_options,
                        country_options,
                        region_options,
                    },
                );
            }
        ));
    }

    fn apply_results(
        &self,
        query: ChannelQuery,
        generation: u64,
        page: ChannelPage,
        facets: InstalledFacets,
    ) {
        let imp = self.imp();
        let previously_selected = imp.selected_id.borrow().clone();
        let previous_index = imp.last_index.get();
        imp.suppress_controls.set(true);
        let has_sources = !facets.sources.is_empty();
        self.install_facets(&query, facets);
        let total = page.total.min(u32::MAX as u64) as u32;
        let rows = imp.rows.get().expect("initialized rows");
        rows.reset(total);
        rows.insert_page(0, page.items);
        imp.count_label
            .set_label(&format!("{total} {}", gettext("channels")));
        if total == 0 {
            imp.expanded_id.replace(None);
            let active_filters = query.search.is_some()
                || query.source_id.is_some()
                || query.group != FacetSelection::Any
                || query.language != FacetSelection::Any
                || query.country != FacetSelection::Any
                || query.region != FacetSelection::Any;
            imp.empty_page.set_title(&if active_filters {
                gettext("No Matching Channels")
            } else if !has_sources {
                gettext("No My Channels Yet")
            } else {
                gettext("No Enabled Channels")
            });
            imp.empty_page.set_description(Some(&if active_filters {
                gettext("Change or clear the search and filters")
            } else {
                gettext("Use source management to enable channels or add a playlist")
            }));
            imp.results_stack.set_visible_child_name("empty");
        } else {
            imp.results_stack.set_visible_child_name("results");
            let fallback = previous_index.min(total - 1);
            imp.selection
                .get()
                .expect("selection")
                .set_selected(fallback);
        }
        imp.suppress_controls.set(false);
        if total != 0 {
            if let Some(id) = previously_selected {
                self.select_channel_id(generation, query, id, previous_index);
            } else {
                let index = imp.selection.get().expect("selection").selected();
                self.request_page(index);
            }
        }
    }

    fn install_facets(
        &self,
        query: &ChannelQuery,
        facets: InstalledFacets,
    ) {
        let InstalledFacets {
            sources,
            source_options,
            group_options,
            language_options,
            country_options,
            region_options,
        } = facets;
        let imp = self.imp();
        let names: HashMap<&str, &str> = sources
            .iter()
            .map(|source| (source.id.as_str(), source.name.as_str()))
            .collect();
        let mut source_labels = vec![gettext("All sources")];
        let mut source_values = vec![None];
        for choice in source_options.choices {
            if let FacetValue::Known(id) = choice.value {
                if let Some(name) = names.get(id.as_str()) {
                    source_labels.push(format!("{} ({})", name, choice.count));
                    source_values.push(Some(id));
                }
            }
        }
        if let Some(id) = query.source_id.as_ref() {
            if !source_values.iter().any(|value| value.as_ref() == Some(id)) {
                source_labels.push(format!(
                    "{} (0)",
                    names
                        .get(id.as_str())
                        .copied()
                        .unwrap_or("Unavailable source")
                ));
                source_values.push(Some(id.clone()));
            }
        }
        let source_index = source_values
            .iter()
            .position(|value| value == &query.source_id)
            .unwrap_or(0) as u32;

        let mut group_labels = vec![gettext("All groups")];
        let mut group_values = vec![FacetSelection::Any];
        for choice in group_options.choices {
            let (label, selection) = match choice.value {
                FacetValue::Known(name) => (name.clone(), FacetSelection::Value(name)),
                FacetValue::Unknown => (gettext("Unknown"), FacetSelection::Unknown),
            };
            group_labels.push(format!("{label} ({})", choice.count));
            group_values.push(selection);
        }
        if !group_values.contains(&query.group) {
            group_labels.push(format!("{} (0)", gettext("Previous group")));
            group_values.push(query.group.clone());
        }
        let group_index = group_values
            .iter()
            .position(|value| value == &query.group)
            .unwrap_or(0) as u32;

        imp.source_values.replace(source_values);
        imp.group_values.replace(group_values);
        self.update_dropdown(&imp.source_filter, &source_labels, source_index);
        self.update_dropdown(&imp.group_filter, &group_labels, group_index);
        let (language_labels, language_values, language_index) =
            Self::facet_dropdown(&gettext("All languages"), &query.language, language_options);
        let (country_labels, country_values, country_index) =
            Self::facet_dropdown(&gettext("All countries"), &query.country, country_options);
        let (region_labels, region_values, region_index) =
            Self::facet_dropdown(&gettext("All regions"), &query.region, region_options);
        let show_region = region_values
            .iter()
            .any(|value| matches!(value, FacetSelection::Value(_)))
            || query.region != FacetSelection::Any;
        imp.region_filter.set_visible(show_region);
        imp.language_values.replace(language_values);
        imp.country_values.replace(country_values);
        imp.region_values.replace(region_values);
        self.update_dropdown(&imp.language_filter, &language_labels, language_index);
        self.update_dropdown(&imp.country_filter, &country_labels, country_index);
        self.update_dropdown(&imp.region_filter, &region_labels, region_index);
        imp.sort_filter.set_selected(match query.sort {
            SortOrder::PlaylistOrder => 0,
            SortOrder::Name => 1,
            SortOrder::Group => 2,
        });
    }

    fn facet_dropdown(
        all: &str,
        selected: &FacetSelection,
        options: FilterOptions,
    ) -> (Vec<String>, Vec<FacetSelection>, u32) {
        let mut labels = vec![all.to_owned()];
        let mut values = vec![FacetSelection::Any];
        for choice in options.choices {
            let (label, value) = match choice.value {
                FacetValue::Known(value) => (value.clone(), FacetSelection::Value(value)),
                FacetValue::Unknown => (gettext("Unknown"), FacetSelection::Unknown),
            };
            labels.push(format!("{label} ({})", choice.count));
            values.push(value);
        }
        if !values.contains(selected) {
            labels.push(format!("{} (0)", gettext("Previous selection")));
            values.push(selected.clone());
        }
        let index = values
            .iter()
            .position(|value| value == selected)
            .unwrap_or(0) as u32;
        (labels, values, index)
    }

    fn select_channel_id(
        &self,
        generation: u64,
        query: ChannelQuery,
        id: ChannelId,
        fallback: u32,
    ) {
        let Some(service) = self.imp().service.borrow().clone() else {
            return;
        };
        spawn(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let position = service.channel_position(query, &id).await;
                if view.imp().generation.get() != generation {
                    return;
                }
                let total = view.imp().rows.get().expect("rows").n_items();
                if total == 0 {
                    return;
                }
                let target = match position {
                    Ok(Some(position)) if position < total as u64 => position as u32,
                    Ok(None) => fallback.min(total - 1),
                    _ => return,
                };
                view.imp()
                    .selection
                    .get()
                    .expect("selection")
                    .set_selected(target);
                view.request_page(target);
                view.imp()
                    .results_list
                    .scroll_to(target, gtk::ListScrollFlags::NONE, None);
            }
        ));
    }

    fn request_page(&self, position: u32) {
        let imp = self.imp();
        let Some(rows) = imp.rows.get() else {
            return;
        };
        if position >= rows.n_items() {
            return;
        }
        let index = position / PAGE_SIZE;
        if rows.has_page(index) || !imp.in_flight.borrow_mut().insert(index) {
            return;
        }
        let Some(service) = imp.service.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get();
        let mut query = imp.query.borrow().clone();
        query.offset = index.saturating_mul(PAGE_SIZE);
        query.limit = PAGE_SIZE;
        spawn(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let response = service.query(query).await;
                if view.imp().generation.get() != generation {
                    return;
                }
                view.imp().in_flight.borrow_mut().remove(&index);
                if let Ok(response) = response {
                    view.imp()
                        .rows
                        .get()
                        .expect("rows")
                        .insert_page(index, response.items);
                    let selected = view.imp().selection.get().expect("selection").selected();
                    if selected / PAGE_SIZE == index {
                        if let Some(channel) =
                            view.imp().rows.get().and_then(|r| r.channel(selected))
                        {
                            view.imp().selected_id.replace(Some(channel.id));
                        }
                    }
                }
            }
        ));
    }

    fn setup_row(&self, item: &glib::Object) {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.set_margin_top(5);
        root.set_margin_bottom(5);
        root.set_margin_start(8);
        root.set_margin_end(8);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let icon = gtk::Image::from_icon_name("tv-symbolic");
        icon.set_pixel_size(28);
        header.append(&icon);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&title);
        let summary = gtk::Label::new(None);
        summary.set_xalign(0.0);
        summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        summary.add_css_class("dim-label");
        text.append(&summary);
        header.append(&text);
        root.append(&header);
        let reveal = gtk::Revealer::new();
        let actions = gtk::Box::new(gtk::Orientation::Vertical, 5);
        let details = gtk::Label::new(None);
        details.set_wrap(true);
        details.set_xalign(0.0);
        details.set_selectable(false);
        actions.append(&details);
        let play = gtk::Button::with_label(&gettext("Play"));
        play.add_css_class("suggested-action");
        play.set_halign(gtk::Align::Start);
        actions.append(&play);
        let edit = gtk::Button::with_label(&gettext("Edit Metadata"));
        edit.set_halign(gtk::Align::Start);
        actions.append(&edit);
        reveal.set_child(Some(&actions));
        root.append(&reveal);
        play.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            item,
            move |_| view.play_position(item.position())
        ));
        edit.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            item,
            move |_| {
                if let Some(channel) = view
                    .imp()
                    .rows
                    .get()
                    .and_then(|r| r.channel(item.position()))
                {
                    view.edit_selected(channel.id);
                }
            }
        ));
        item.set_child(Some(&root));
        self.imp().bindings.borrow_mut().insert(
            item.as_ptr() as usize,
            BoundRow {
                ui: RowUi {
                    title,
                    summary,
                    details,
                    reveal,
                    play,
                    edit,
                },
                connection: None,
            },
        );
    }

    fn bind_row(&self, item: &glib::Object) {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(slot) = item.item().and_downcast::<StreamSlot>() else {
            return;
        };
        let key = item.as_ptr() as usize;
        self.unbind_row(item.upcast_ref());
        if let Some(binding) = self.imp().bindings.borrow_mut().get_mut(&key) {
            Self::paint_row(
                &binding.ui,
                &slot,
                self.imp().expanded_id.borrow().as_deref(),
            );
            let ui = binding.ui.clone();
            let weak_view = self.downgrade();
            let weak_slot = slot.downgrade();
            let handler = slot.connect_notify_local(Some("ready"), move |_, _| {
                if let (Some(view), Some(slot)) = (weak_view.upgrade(), weak_slot.upgrade()) {
                    Self::paint_row(&ui, &slot, view.imp().expanded_id.borrow().as_deref());
                }
            });
            binding.connection = Some((slot.clone(), handler));
        }
        self.request_page(slot.position());
    }

    fn unbind_row(&self, item: &glib::Object) {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some(binding) = self
            .imp()
            .bindings
            .borrow_mut()
            .get_mut(&(item.as_ptr() as usize))
        {
            if let Some((slot, handler)) = binding.connection.take() {
                slot.disconnect(handler);
            }
            binding.ui.title.set_text("");
            binding.ui.summary.set_text("");
            binding.ui.details.set_text("");
            binding.ui.reveal.set_reveal_child(false);
            binding.ui.play.set_sensitive(false);
            binding.ui.edit.set_sensitive(false);
        }
    }

    fn teardown_row(&self, item: &glib::Object) {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        self.unbind_row(item.upcast_ref());
        self.imp()
            .bindings
            .borrow_mut()
            .remove(&(item.as_ptr() as usize));
    }

    fn paint_row(ui: &RowUi, slot: &StreamSlot, expanded: Option<&str>) {
        let Some(channel) = slot.channel() else {
            ui.title.set_text(&gettext("Loading channel…"));
            ui.summary.set_text("");
            ui.details.set_text("");
            ui.reveal.set_reveal_child(false);
            ui.play.set_sensitive(false);
            ui.edit.set_sensitive(false);
            return;
        };
        ui.title.set_text(&channel.visible_name);
        let mut brief = Vec::new();
        if let Some(group) = channel.group.as_ref() {
            brief.push(group.as_str());
        }
        brief.push(&channel.source_name);
        ui.summary.set_text(&brief.join(" · "));
        let mut details = Vec::new();
        if !channel.languages.is_empty() {
            details.push(format!(
                "{}: {}",
                gettext("Language"),
                channel
                    .languages
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !channel.countries.is_empty() {
            details.push(format!(
                "{}: {}",
                gettext("Country"),
                channel
                    .countries
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(region) = &channel.region {
            details.push(format!(
                "{}: {}",
                gettext("Region"),
                region
                    .label
                    .as_deref()
                    .or(region.code.as_deref())
                    .unwrap_or("")
            ));
        }
        ui.details.set_text(&details.join(" · "));
        ui.reveal
            .set_reveal_child(expanded == Some(channel.id.as_str()));
        ui.play.set_sensitive(true);
        ui.edit.set_sensitive(true);
    }

    fn repaint_bound_rows(&self) {
        let expanded = self.imp().expanded_id.borrow().clone();
        for binding in self.imp().bindings.borrow().values() {
            if let Some((slot, _)) = &binding.connection {
                Self::paint_row(&binding.ui, slot, expanded.as_deref());
            }
        }
    }

    fn activate_position(&self, position: u32) {
        let Some(channel) = self
            .imp()
            .rows
            .get()
            .and_then(|rows| rows.channel(position))
        else {
            self.request_page(position);
            return;
        };
        // The explicit Play button owns Return when it has focus.
        if self
            .imp()
            .bindings
            .borrow()
            .values()
            .any(|binding| binding.ui.play.has_focus() || binding.ui.edit.has_focus())
        {
            return;
        }
        if self.imp().expanded_id.borrow().as_deref() == Some(channel.id.as_str()) {
            self.play_position(position);
        } else {
            self.imp().expanded_id.replace(Some(channel.id.clone()));
            self.imp().selected_id.replace(Some(channel.id));
            self.repaint_bound_rows();
        }
    }

    fn play_position(&self, position: u32) {
        let Some(channel) = self
            .imp()
            .rows
            .get()
            .and_then(|rows| rows.channel(position))
        else {
            return;
        };
        if self.imp().expanded_id.borrow().as_deref() != Some(channel.id.as_str()) {
            return;
        }
        // Storage/parser constrain schemes; enforce it again at the presentation boundary.
        let safe = reqwest::Url::parse(&channel.stream_url)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some());
        if !safe {
            return;
        }
        self.imp().selected_id.replace(Some(channel.id));
        #[cfg(test)]
        if self.imp().fake_player_enabled.get() {
            let calls = self.imp().fake_player_calls.get();
            self.imp().fake_player_calls.set(calls + 1);
            return;
        }
        let video = VideoInfo::Stream {
            title: channel.visible_name,
            uri: channel.stream_url,
        };
        spawn(async move { TvApplication::get().play(video).await });
    }

    pub fn show_add(&self) {
        super::management::show_add_menu(self);
    }

    pub fn show_manager(&self) {
        super::management::show_manager(self);
    }

    pub(crate) fn service(&self) -> Option<StreamsService> {
        self.imp().service.borrow().clone()
    }

    pub(crate) fn edit_selected(&self, id: ChannelId) {
        super::editor::show_metadata_editor(self, id);
    }

    fn show_import_placeholder(&self) {
        if let Some(window) = self.root().and_downcast::<gtk::Window>() {
            if let Some(overlay) = window
                .first_child()
                .and_then(|_| self.ancestor(adw::ToastOverlay::static_type()))
                .and_downcast::<adw::ToastOverlay>()
            {
                overlay.add_toast(adw::Toast::new(&gettext(
                    "Playlist import and source management arrive in the next stage",
                )));
            }
        }
    }

    /// Exercises the actual GtkListView activate signal with a fabricated
    /// channel and a fake player. No network/DB/mpv process is used.
    #[cfg(test)]
    pub(crate) fn assert_fake_player_navigation(&self) {
        use super::model::ChannelEffective;
        use std::{collections::BTreeSet, time::Instant};
        // An *unallocated/unmapped* GtkListView has no viewport and may
        // request every row while measuring its natural size. Benchmark the
        // lazy data model detached from that GTK size-request artifact;
        // real mapped viewport/TV performance still needs device validation.
        let model = StreamRows::new();
        for count in [100, 1_000, 10_000] {
            let start = Instant::now();
            model.reset(count);
            assert_eq!(model.n_items(), count);
            assert_eq!(model.created_slots(), 0, "model allocated all slots");
            let last = model.item(count - 1).expect("far channel position");
            let slot = last.downcast::<StreamSlot>().expect("slot object");
            assert_eq!(slot.position(), count - 1);
            assert_eq!(
                model.created_slots(),
                1,
                "far lookup allocated more than one"
            );
            assert_eq!(model.cached_pages(), 0);
            eprintln!(
                "S4 detached lazy model {count} positions: 1 slot, duration={:?}",
                start.elapsed()
            );
        }
        let rows = self.imp().rows.get().expect("rows");
        let id = uuid::Uuid::new_v4().to_string();
        let channel = ChannelEffective {
            id: id.clone(),
            source_id: uuid::Uuid::new_v4().to_string(),
            source_name: "Fabricated Source".into(),
            visible_name: "Fabricated Channel".into(),
            stream_url: "https://example.invalid/fake.m3u8".into(),
            group: Some("News".into()),
            languages: BTreeSet::new(),
            countries: BTreeSet::new(),
            region: None,
            logo_url: None,
            channel_number: None,
            has_user_overrides: false,
        };
        rows.reset(1);
        rows.insert_page(0, vec![channel]);
        self.imp().fake_player_enabled.set(true);
        self.imp().fake_player_calls.set(0);
        self.imp()
            .results_list
            .emit_by_name::<()>("activate", &[&0u32]);
        assert_eq!(
            self.imp().expanded_id.borrow().as_deref(),
            Some(id.as_str())
        );
        assert_eq!(
            self.imp().fake_player_calls.get(),
            0,
            "first Enter must never play"
        );
        self.imp()
            .results_list
            .emit_by_name::<()>("activate", &[&0u32]);
        assert_eq!(
            self.imp().fake_player_calls.get(),
            1,
            "second Enter plays once"
        );
        self.play_position(0); // The explicit Play button's single callback.
        assert_eq!(self.imp().fake_player_calls.get(), 2);
        assert!(self.collapse_expanded());
        assert!(!self.collapse_expanded());
        self.play_position(0);
        assert_eq!(
            self.imp().fake_player_calls.get(),
            2,
            "collapsed row cannot play"
        );
        assert_eq!(
            self.imp().selected_id.borrow().as_deref(),
            Some(id.as_str())
        );
    }

    pub fn focus_search(&self) {
        if !self.imp().search_entry.grab_focus() {
            self.imp().add_button.grab_focus();
        }
    }

    pub fn focus_vertical_navigation(&self, focused: &gtk::Widget, key: gtk::gdk::Key) -> bool {
        if !matches!(key, gtk::gdk::Key::Up | gtk::gdk::Key::Down) {
            return false;
        }
        let imp = self.imp();
        let controls: [&gtk::Widget; 11] = [
            imp.search_entry.upcast_ref(),
            imp.source_filter.upcast_ref(),
            imp.group_filter.upcast_ref(),
            imp.language_filter.upcast_ref(),
            imp.country_filter.upcast_ref(),
            imp.region_filter.upcast_ref(),
            imp.sort_filter.upcast_ref(),
            imp.clear_button.upcast_ref(),
            imp.manage_button.upcast_ref(),
            imp.add_button.upcast_ref(),
            imp.results_list.upcast_ref(),
        ];
        for (index, widget) in controls.iter().enumerate() {
            if *widget != focused && !widget.is_ancestor(focused) {
                continue;
            }
            if index == controls.len() - 1 {
                // GTK owns row-to-row movement. Only escape from the first row.
                if key == gtk::gdk::Key::Up
                    && imp.selection.get().is_some_and(|s| s.selected() == 0)
                {
                    return controls[controls.len() - 2].grab_focus();
                }
                return false;
            }
            if key == gtk::gdk::Key::Up {
                return controls[..index]
                    .iter()
                    .rev()
                    .find(|w| w.is_visible() && w.is_sensitive())
                    .is_some_and(|w| w.grab_focus());
            }
            let next = index + 1;
            if next == controls.len() - 1 && imp.rows.get().is_some_and(|rows| rows.n_items() == 0)
            {
                return imp.add_button.grab_focus();
            }
            return controls[next..]
                .iter()
                .find(|w| w.is_visible() && w.is_sensitive())
                .is_some_and(|w| w.grab_focus());
        }
        false
    }

    pub fn collapse_expanded(&self) -> bool {
        if self.imp().expanded_id.borrow_mut().take().is_none() {
            return false;
        }
        self.repaint_bound_rows();
        self.imp().results_list.grab_focus();
        true
    }

    pub fn restore_selection_by_id(&self) {
        let id = self.imp().selected_id.borrow().clone();
        let generation = self.imp().generation.get();
        if let Some(id) = id {
            let query = self.imp().query.borrow().clone();
            self.select_channel_id(generation, query, id, self.imp().last_index.get());
        }
        self.imp().results_list.grab_focus();
    }
}

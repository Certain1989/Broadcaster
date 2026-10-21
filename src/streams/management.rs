// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Source controls. Never display source locators in status messages or logs.
//! All I/O and SQLite writes are performed by StreamsService outside GTK.
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use adw::{gio, glib, gtk, prelude::*};
use gettextrs::gettext;

use crate::{
    streams::{
        facet_picker::FacetPicker,
        model::{FacetDimension, OriginKind, Region, Source, SourceDefaults},
        service::{
            choose_local_file, RefreshOutcome, RefreshStage, RefreshTicket, ServiceError,
            ServiceEvent, SourceSelection, StreamsService,
        },
        storage::ConfirmedSourceDeletion,
        view::TvStreamsView,
    },
    utils::spawn,
};

fn parent(view: &TvStreamsView) -> Option<gtk::Window> {
    view.root().and_downcast::<gtk::Window>()
}

fn sheet(title: &str) -> (adw::Dialog, gtk::Box, gtk::Label) {
    let dialog = adw::Dialog::new();
    dialog.set_title(title);
    dialog.set_content_width(520);
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 10);
    box_.set_margin_top(16);
    box_.set_margin_bottom(16);
    box_.set_margin_start(16);
    box_.set_margin_end(16);
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    dialog.set_child(Some(&box_));
    (dialog, box_, status)
}

// Add Playlist has a short ordinary state and a longer Advanced state.
// Keep the action footer outside the scroll area and adapt the requested
// height to the state, rather than retaining unused space when collapsed.
fn source_form_layout(
    dialog: &adw::Dialog,
    body: &gtk::Box,
    advanced: &gtk::Expander,
) -> (gtk::ScrolledWindow, gtk::Box) {
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_vexpand(true);

    // sheet() temporarily owns the editable body; reparent it inside the
    // scroller before building the permanent dialog container.
    dialog.set_child(None::<&gtk::Widget>);
    scroll.set_child(Some(body));

    let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shell.append(&scroll);
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    footer.set_margin_top(8);
    footer.set_margin_bottom(16);
    footer.set_margin_start(16);
    footer.set_margin_end(16);
    shell.append(&footer);
    dialog.set_child(Some(&shell));

    dialog.set_content_height(if advanced.is_expanded() { 620 } else { 360 });
    advanced.connect_notify_local(
        Some("expanded"),
        glib::clone!(
            #[weak]
            dialog,
            move |expander, _| {
                dialog.set_content_height(if expander.is_expanded() { 620 } else { 360 });
            }
        ),
    );
    (scroll, footer)
}

fn row(label: &str, control: &impl IsA<gtk::Widget>, box_: &gtk::Box) {
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_wrap(true);
    box_.append(&text);
    box_.append(control);
}

// Compact, D-pad-reachable launcher for a concrete writable source field.
// Up from the icon returns to the entry; Down from its native text editor
// exposes the icon without taking over Left/Right caret navigation.
fn editable_row(
    label: &str,
    field: &gtk::Entry,
    parent: &gtk::Box,
    options: crate::keyboard::KeyboardOptions,
) -> gtk::Button {
    let title = gtk::Label::new(Some(label));
    title.set_xalign(0.0);
    title.set_wrap(true);
    parent.append(&title);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.append(field);
    let keyboard = gtk::Button::new();
    keyboard.set_icon_name("input-keyboard-symbolic");
    keyboard.set_size_request(48, 44);
    keyboard.set_valign(gtk::Align::Center);
    crate::keyboard::bind_launcher(&keyboard, field, options);
    controls.append(&keyboard);
    parent.append(&controls);

    let down = gtk::EventControllerKey::new();
    down.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_keyboard = keyboard.downgrade();
    down.connect_key_pressed(move |_, key, _, modifiers| {
        if crate::keyboard::unmodified_remote(modifiers)
            && matches!(key, gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down)
            && weak_keyboard
                .upgrade()
                .is_some_and(|button| button.grab_focus())
        {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    field.add_controller(down);

    let up = gtk::EventControllerKey::new();
    up.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_field = field.downgrade();
    up.connect_key_pressed(move |_, key, _, modifiers| {
        if crate::keyboard::unmodified_remote(modifiers)
            && matches!(key, gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up)
            && weak_field.upgrade().is_some_and(|entry| entry.grab_focus())
        {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    keyboard.add_controller(up);
    keyboard
}

fn entry(value: &str) -> gtk::Entry {
    let field = gtk::Entry::new();
    field.set_text(value);
    field.set_hexpand(true);
    field
}

fn line(box_: &gtk::Box) -> gtk::Box {
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    box_.append(&buttons);
    buttons
}

fn button(text: &str, container: &gtk::Box) -> gtk::Button {
    let button = gtk::Button::with_label(text);
    container.append(&button);
    button
}

fn message(view: &TvStreamsView, value: &str) {
    if let Some(win) = view.root().and_downcast::<crate::window::TvWindow>() {
        win.add_toast(adw::Toast::new(value));
    }
}

fn fail(view: &TvStreamsView, error: ServiceError) {
    // ServiceError has a deliberately redacted Display implementation.
    message(
        view,
        &format!("{}: {error}", gettext("Playlist operation failed")),
    );
}

fn region(code: &str, label: &str) -> Result<Option<Region>, &'static str> {
    let code = code.trim();
    let label = label.trim();
    let result = if code.is_empty() && label.is_empty() {
        None
    } else {
        Some(Region {
            code: (!code.is_empty()).then(|| code.to_owned()),
            label: (!label.is_empty()).then(|| label.to_owned()),
        })
    };
    if result
        .as_ref()
        .is_some_and(|value| value.validate().is_err())
    {
        return Err("Invalid region (maximum 64 code / 128 label bytes)");
    }
    Ok(result)
}

struct DefaultsForm {
    languages: FacetPicker,
    countries: FacetPicker,
    code: gtk::Entry,
    label: gtk::Entry,
    region_error: gtk::Label,
}

impl DefaultsForm {
    fn new(box_: &gtk::Box, defaults: &SourceDefaults) -> Self {
        let languages = FacetPicker::new(FacetDimension::Language, &defaults.languages);
        row(
            &gettext("Default languages (optional)"),
            &languages.root,
            box_,
        );
        let countries = FacetPicker::new(FacetDimension::Country, &defaults.countries);
        row(
            &gettext("Default countries (optional)"),
            &countries.root,
            box_,
        );
        let explanation = gtk::Label::new(Some(&gettext(
            "These defaults are used only when channel metadata is missing from the playlist.",
        )));
        explanation.set_wrap(true);
        explanation.set_xalign(0.0);
        box_.append(&explanation);
        let code = entry(
            defaults
                .region
                .as_ref()
                .and_then(|r| r.code.as_deref())
                .unwrap_or(""),
        );
        code.set_placeholder_text(Some(&gettext("Optional: DE-BY")));
        let label = entry(
            defaults
                .region
                .as_ref()
                .and_then(|r| r.label.as_deref())
                .unwrap_or(""),
        );
        label.set_placeholder_text(Some(&gettext("Optional: Bavaria")));
        editable_row(
            &gettext("Default region code (optional)"),
            &code,
            box_,
            crate::keyboard::KeyboardOptions::default(),
        );
        editable_row(
            &gettext("Default region name (optional)"),
            &label,
            box_,
            crate::keyboard::KeyboardOptions::default(),
        );
        let region_error = gtk::Label::new(None);
        region_error.set_wrap(true);
        region_error.set_xalign(0.0);
        region_error.add_css_class("error");
        box_.append(&region_error);
        Self {
            languages,
            countries,
            code,
            label,
            region_error,
        }
    }

    fn collect(&self) -> Result<SourceDefaults, String> {
        let languages = self.languages.values(false)?;
        let countries = self.countries.values(false)?;
        let region = match region(&self.code.text(), &self.label.text()) {
            Ok(region) => {
                self.region_error.set_text("");
                region
            }
            Err(_) => {
                let reason = gettext(
                    "Invalid region: use a name or code (e.g. Bavaria / DE-BY), max 128/64 bytes.",
                );
                self.region_error.set_text(&reason);
                self.code.grab_focus();
                return Err(reason);
            }
        };
        let defaults = SourceDefaults {
            languages,
            countries,
            region,
        };
        defaults
            .validate()
            .map_err(|_| gettext("Invalid metadata defaults"))?;
        Ok(defaults)
    }
}

pub(crate) fn show_add_menu(view: &TvStreamsView) {
    let Some(window) = parent(view) else {
        return;
    };
    let (dialog, body, _) = sheet(&gettext("Add Playlist"));
    let file = button(&gettext("Choose Local M3U File"), &body);
    let url = button(&gettext("Enter Playlist URL"), &body);
    // A navigation-only resource, not an embedded catalogue or a playlist
    // Broadcaster silently subscribes to. GtkLinkButton is D-pad focusable.
    let directory = gtk::LinkButton::with_label(
        "https://github.com/iptv-org/iptv/blob/master/PLAYLISTS.md",
        &gettext("Find public IPTV playlists (IPTV-org)"),
    );
    directory.set_halign(gtk::Align::Start);
    body.append(&directory);
    let note = gtk::Label::new(Some(&gettext(
        "External directory; stream availability and regional access vary.",
    )));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");
    body.append(&note);
    file.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        move |_| {
            dialog.close();
            show_file_picker(&view, None);
        }
    ));
    url.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        move |_| {
            dialog.close();
            show_source_form(&view, None, None);
        }
    ));
    dialog.present(Some(&window));
}

fn show_file_picker(view: &TvStreamsView, replacement: Option<Source>) {
    let Some(window) = parent(view) else {
        return;
    };
    spawn(glib::clone!(
        #[weak]
        view,
        async move {
            if let Some(file) = choose_local_file(&window).await {
                if let Some(source) = replacement {
                    replace_origin(&view, source.id, SourceSelection::Local(file));
                } else {
                    show_source_form(&view, Some(file), None);
                }
            }
        }
    ));
}

fn show_source_form(view: &TvStreamsView, file: Option<gio::File>, defaults_for: Option<Source>) {
    let Some(window) = parent(view) else {
        return;
    };
    let Some(service) = view.service() else {
        return;
    };
    let is_new = defaults_for.is_none();
    let initial_name = defaults_for
        .as_ref()
        .map(|s| s.name.clone())
        .unwrap_or_else(|| {
            file.as_ref()
                .and_then(|f| f.basename())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let initial_defaults = defaults_for
        .as_ref()
        .map(|s| s.defaults.clone())
        .unwrap_or_default();
    let (dialog, body, status) = sheet(&if is_new {
        gettext("Add Playlist")
    } else {
        gettext("Edit Source Defaults")
    });
    let name = entry(&initial_name);
    let name_error = gtk::Label::new(None);
    name_error.set_wrap(true);
    name_error.set_xalign(0.0);
    name_error.add_css_class("error");
    if is_new {
        let keyboard = editable_row(
            &gettext("Unique source name"),
            &name,
            &body,
            crate::keyboard::KeyboardOptions::default(),
        );
        if file.is_none() {
            // Match the adjacent URL row's tested launcher/entry dimensions.
            crate::keyboard::style_add_playlist_field_launcher(&keyboard);
        }
        body.append(&name_error);
    }
    let url_error = gtk::Label::new(None);
    url_error.set_wrap(true);
    url_error.set_xalign(0.0);
    url_error.add_css_class("error");
    let url = if is_new && file.is_none() {
        let field = entry("");
        field.set_input_purpose(gtk::InputPurpose::Url);
        let keyboard = editable_row(
            &gettext("Playlist HTTP(S) URL"),
            &field,
            &body,
            crate::keyboard::KeyboardOptions::url(),
        );
        // The source-name and URL rows share the existing tested geometry.
        crate::keyboard::style_add_playlist_field_launcher(&keyboard);
        body.append(&url_error);
        Some(field)
    } else {
        None
    };
    // Ordinary imports need no metadata defaults.
    let advanced = gtk::Expander::new(Some(&gettext(
        "Advanced · Source metadata defaults (optional)",
    )));
    advanced.set_expanded(!is_new);
    let advanced_body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let defaults = DefaultsForm::new(&advanced_body, &initial_defaults);
    advanced.set_child(Some(&advanced_body));
    body.append(&advanced);
    // The form scrolls independently; actions/status remain visible when
    // Advanced expands beyond the available dialog height.
    let (_scroll, footer) = source_form_layout(&dialog, &body, &advanced);
    let buttons = line(&footer);
    let cancel = button(&gettext("Cancel"), &buttons);
    let save = button(&gettext("Save"), &buttons);
    save.add_css_class("suggested-action");
    footer.append(&status);
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    save.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        #[weak]
        status,
        #[weak]
        save,
        move |_| {
            let new_defaults = match defaults.collect() {
                Ok(v) => v,
                Err(text) => {
                    status.set_text(&text);
                    return;
                }
            };
            let new_name = name.text().trim().to_owned();
            if is_new && (new_name.is_empty() || new_name.len() > 128) {
                let reason = gettext("Enter a source name (1–128 bytes)");
                name_error.set_text(&reason);
                name.grab_focus();
                return;
            }
            name_error.set_text("");
            let url_value = url.as_ref().map(|url| url.text().to_string());
            if let Some(ref url_value) = url_value {
                let valid = reqwest::Url::parse(url_value).is_ok_and(|value| {
                    matches!(value.scheme(), "http" | "https") && value.host().is_some()
                });
                if !valid {
                    url_error.set_text(&gettext("Enter a valid HTTP(S) playlist URL"));
                    if let Some(url) = &url {
                        url.grab_focus();
                    }
                    return;
                }
            }
            url_error.set_text("");
            save.set_sensitive(false);
            let service = service.clone();
            let file = file.clone();
            let source = defaults_for.clone();
            spawn(glib::clone!(
                #[weak]
                view,
                #[weak]
                dialog,
                #[weak]
                save,
                #[weak]
                status,
                async move {
                    let result = if let Some(source) = source {
                        service
                            .set_source_defaults(&source.id, new_defaults)
                            .await
                            .map(|_| ())
                    } else {
                        let order = service
                            .list_sources()
                            .await
                            .ok()
                            .and_then(|items| items.iter().map(|s| s.order).max())
                            .unwrap_or(0)
                            .saturating_add(1);
                        if let Some(file) = file {
                            service
                                .add_local(&new_name, file, order, new_defaults)
                                .await
                                .map(|_| ())
                        } else if let Some(url_value) = url_value {
                            service
                                .add_url(&new_name, url_value, order, new_defaults)
                                .await
                                .map(|_| ())
                        } else {
                            Err(ServiceError::WorkerFailed)
                        }
                    };
                    match result {
                        Ok(()) => {
                            dialog.close();
                            view.source_mutated();
                            message(&view, &gettext("Playlist saved"));
                        }
                        Err(e) => {
                            status.set_text(&e.to_string());
                            save.set_sensitive(true);
                        }
                    }
                }
            ));
        }
    ));
    dialog.present(Some(&window));
}

pub(crate) fn show_manager(view: &TvStreamsView) {
    let Some(window) = parent(view) else {
        return;
    };
    let Some(service) = view.service() else {
        return;
    };
    let (dialog, body, status) = sheet(&gettext("Manage Sources"));
    dialog.set_content_height(650);
    let actions = line(&body);
    let add = button(&gettext("Add Playlist"), &actions);
    let close = button(&gettext("Close"), &actions);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    scroll.set_child(Some(&list));
    body.append(&scroll);
    body.append(&status);
    add.connect_clicked(glib::clone!(
        #[weak]
        view,
        move |_| show_add_menu(&view)
    ));
    close.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    view.remember_source_manager(&list, &status);
    view.refresh_source_manager();
    dialog.connect_closed(glib::clone!(
        #[weak]
        view,
        move |_| view.forget_source_manager()
    ));
    attach_progress(&dialog, &status, service);
    dialog.present(Some(&window));
}

fn stage_name(stage: RefreshStage) -> String {
    match stage {
        RefreshStage::Idle => gettext("Ready"),
        RefreshStage::Loading => gettext("Loading playlist"),
        RefreshStage::Parsing => gettext("Reading channels"),
        RefreshStage::Previewing => gettext("Checking channel changes"),
        RefreshStage::AwaitingConfirmation => gettext("Waiting for confirmation"),
        RefreshStage::Committing => gettext("Saving channels"),
        RefreshStage::Succeeded => gettext("Refresh completed"),
        RefreshStage::Failed => gettext("Refresh failed"),
        RefreshStage::Cancelled => gettext("Refresh cancelled"),
    }
}

/// The receiver checks current service state rather than relying on the
/// ordering of delivered events. A timer bounds its lifetime after close.
fn attach_progress(dialog: &adw::Dialog, status: &gtk::Label, service: StreamsService) {
    let stopped = Rc::new(Cell::new(false));
    dialog.connect_closed({
        let stopped = stopped.clone();
        move |_| stopped.set(true)
    });
    let mut events = service.subscribe();
    spawn(glib::clone!(
        #[weak]
        status,
        async move {
            loop {
                if stopped.get() {
                    break;
                }
                let received = futures_util::future::select(
                    Box::pin(events.recv()),
                    Box::pin(glib::timeout_future(Duration::from_secs(1))),
                )
                .await;
                let futures_util::future::Either::Left((event, _)) = received else {
                    continue;
                };
                let source_id = match event {
                    Ok(ServiceEvent::State { source_id, .. })
                    | Ok(ServiceEvent::Preview { source_id, .. })
                    | Ok(ServiceEvent::Finished { source_id, .. }) => source_id,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Read current authoritative stage after missed events.
                        if let Ok(sources) = service.list_sources().await {
                            for source in sources {
                                let stage = service.stage(&source.id).await;
                                if !matches!(stage, RefreshStage::Idle) {
                                    status.set_text(&format!(
                                        "{}: {}",
                                        source.name,
                                        stage_name(stage)
                                    ));
                                }
                            }
                        }
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                let stage = service.stage(&source_id).await;
                if let Ok(source) = service.source(&source_id).await {
                    status.set_text(&format!("{}: {}", source.name, stage_name(stage)));
                } else {
                    status.set_text(&stage_name(stage));
                }
            }
        }
    ));
}

pub(crate) fn load_manager(
    view: &TvStreamsView,
    service: &StreamsService,
    list: &gtk::ListBox,
    status: &gtk::Label,
) {
    let service = service.clone();
    let generation = view.next_manager_generation();
    spawn(glib::clone!(
        #[weak]
        view,
        #[weak]
        list,
        #[weak]
        status,
        async move {
            let sources = match service.list_sources().await {
                Ok(sources) => sources,
                Err(error) => {
                    status.set_text(&error.to_string());
                    return;
                }
            };
            let counts = futures_util::future::join_all(
                sources
                    .iter()
                    .map(|source| service.source_counts(&source.id)),
            )
            .await;
            if view.manager_generation() != generation {
                return;
            }
            while let Some(item) = list.row_at_index(0) {
                list.remove(&item);
            }
            if sources.is_empty() {
                status.set_text(&gettext("No playlists imported"));
            } else {
                status.set_text("");
            }
            for (index, source) in sources.iter().enumerate() {
                let source: Source = source.clone();
                let panel = gtk::Box::new(gtk::Orientation::Vertical, 5);
                panel.set_margin_top(10);
                panel.set_margin_bottom(10);
                panel.set_margin_start(10);
                panel.set_margin_end(10);
                let title = gtk::Label::new(Some(&source.name));
                title.set_xalign(0.0);
                title.set_ellipsize(gtk::pango::EllipsizeMode::End);
                title.add_css_class("heading");
                panel.append(&title);
                let origin = match source.origin.kind {
                    OriginKind::LocalFile => gettext("Local file"),
                    OriginKind::RemoteUrl => gettext("Remote playlist"),
                };
                let state = source
                    .last_error_code
                    .as_ref()
                    .map_or_else(|| gettext("No recorded error"), |code| code.clone());
                let (present, retained) = counts
                    .get(index)
                    .and_then(|result| result.as_ref().ok().copied())
                    .unwrap_or((0, 0));
                let last_attempt = source
                    .last_attempt_at
                    .and_then(crate::utils::format_timestamp_full)
                    .map_or_else(|| gettext("Never"), |date| date.to_string());
                let last_success = source
                    .last_success_at
                    .and_then(crate::utils::format_timestamp_full)
                    .map_or_else(|| gettext("Never"), |date| date.to_string());
                let summary = gtk::Label::new(Some(&format!(
                    "{origin} · {state} · {}: {present} / {retained}\n{}: {last_success} · {}: {last_attempt}",
                    gettext("Active / retained channels"),
                    gettext("Last success"),
                    gettext("Last attempt"),
                )));
                summary.set_xalign(0.0);
                summary.set_wrap(true);
                summary.add_css_class("dim-label");
                panel.append(&summary);
                let upper_actions = gtk::Box::new(gtk::Orientation::Horizontal, 5);
                let lower_actions = gtk::Box::new(gtk::Orientation::Horizontal, 5);
                let enabled = gtk::Switch::new();
                enabled.set_active(source.enabled);
                enabled.set_tooltip_text(Some(&gettext("Enable or disable source")));
                upper_actions.append(&enabled);
                let refresh = button(&gettext("Refresh"), &upper_actions);
                let edit = button(&gettext("Defaults"), &upper_actions);
                let rename = button(&gettext("Rename"), &upper_actions);
                let change = button(&gettext("Replace"), &lower_actions);
                let remove = button(&gettext("Delete"), &lower_actions);
                // Atomic order swaps handle equal and dense source_order values.
                let up = button("↑", &lower_actions);
                let down = button("↓", &lower_actions);
                up.set_sensitive(index != 0);
                down.set_sensitive(index + 1 < sources.len());
                panel.append(&upper_actions);
                panel.append(&lower_actions);
                let row = gtk::ListBoxRow::new();
                row.set_child(Some(&panel));
                list.append(&row);
                let src = source.clone();
                let svc = service.clone();
                enabled.connect_active_notify(glib::clone!(
                    #[weak]
                    view,
                    move |switch| {
                        let target = switch.is_active();
                        let service = svc.clone();
                        let id = src.id.clone();
                        spawn(glib::clone!(
                            #[weak]
                            view,
                            async move {
                                match service.toggle_source(&id, target).await {
                                    Ok(()) => view.source_mutated(),
                                    Err(error) => {
                                        fail(&view, error);
                                        view.refresh_source_manager();
                                    }
                                }
                            }
                        ));
                    }
                ));
                let refresh_id = source.id.clone();
                refresh.connect_clicked(glib::clone!(
                    #[weak]
                    view,
                    move |_| {
                        refresh_source(&view, refresh_id.clone());
                    }
                ));
                let edit_source = source.clone();
                edit.connect_clicked(glib::clone!(
                    #[weak]
                    view,
                    move |_| {
                        show_source_form(&view, None, Some(edit_source.clone()));
                    }
                ));
                let source_for_rename = source.clone();
                rename.connect_clicked(glib::clone!(
                    #[weak]
                    view,
                    move |_| {
                        rename_source(&view, source_for_rename.clone());
                    }
                ));
                let source_for_change = source.clone();
                change.connect_clicked(glib::clone!(
                    #[weak]
                    view,
                    move |_| {
                        change_source(&view, source_for_change.clone());
                    }
                ));
                let remove_source = source.clone();
                remove.connect_clicked(glib::clone!(
                    #[weak]
                    view,
                    move |_| {
                        confirm_delete(&view, remove_source.clone());
                    }
                ));
                for (control, direction) in [(up, -1), (down, 1)] {
                    let service = service.clone();
                    let id = source.id.clone();
                    control.connect_clicked(glib::clone!(
                        #[weak]
                        view,
                        move |_| {
                            let service = service.clone();
                            let id = id.clone();
                            spawn(glib::clone!(
                                #[weak]
                                view,
                                async move {
                                    match service.move_source(&id, direction).await {
                                        Ok(()) => view.source_mutated(),
                                        Err(error) => fail(&view, error),
                                    }
                                }
                            ));
                        }
                    ));
                }
            }
        }
    ));
}

fn rename_source(view: &TvStreamsView, source: Source) {
    let (Some(window), Some(service)) = (parent(view), view.service()) else {
        return;
    };
    let (dialog, body, status) = sheet(&gettext("Rename Source"));
    let name = entry(&source.name);
    editable_row(
        &gettext("Source name"),
        &name,
        &body,
        crate::keyboard::KeyboardOptions::default(),
    );
    let actions = line(&body);
    let cancel = button(&gettext("Cancel"), &actions);
    let save = button(&gettext("Save"), &actions);
    body.append(&status);
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    save.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        #[weak]
        save,
        #[weak]
        status,
        move |_| {
            let value = name.text().trim().to_owned();
            if value.is_empty() || value.len() > 128 {
                status.set_text(&gettext("Enter a source name (1–128 bytes)"));
                return;
            }
            save.set_sensitive(false);
            let service = service.clone();
            let id = source.id.clone();
            spawn(glib::clone!(
                #[weak]
                view,
                #[weak]
                dialog,
                #[weak]
                save,
                #[weak]
                status,
                async move {
                    match service.rename_source(&id, &value).await {
                        Ok(()) => {
                            dialog.close();
                            view.source_mutated();
                            message(&view, &gettext("Source renamed"));
                        }
                        Err(error) => {
                            status.set_text(&error.to_string());
                            save.set_sensitive(true);
                        }
                    }
                }
            ));
        }
    ));
    dialog.present(Some(&window));
}

fn change_source(view: &TvStreamsView, source: Source) {
    let Some(window) = parent(view) else {
        return;
    };
    let (dialog, body, _) = sheet(&gettext("Replace Playlist"));
    let file = button(&gettext("Reselect Local File"), &body);
    let url = button(&gettext("Replace HTTP(S) URL"), &body);
    let file_source = source.clone();
    let url_source_id = source.id.clone();
    file.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        move |_| {
            dialog.close();
            show_file_picker(&view, Some(file_source.clone()));
        }
    ));
    url.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        move |_| {
            dialog.close();
            replace_url_form(&view, url_source_id.clone());
        }
    ));
    dialog.present(Some(&window));
}

fn replace_url_form(view: &TvStreamsView, id: String) {
    let Some(window) = parent(view) else {
        return;
    };
    let (dialog, body, status) = sheet(&gettext("Replace Playlist URL"));
    let input = entry("");
    input.set_input_purpose(gtk::InputPurpose::Url);
    editable_row(
        &gettext("New HTTP(S) URL (not shown in status messages)"),
        &input,
        &body,
        crate::keyboard::KeyboardOptions::url(),
    );
    let actions = line(&body);
    let cancel = button(&gettext("Cancel"), &actions);
    let save = button(&gettext("Replace"), &actions);
    body.append(&status);
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    save.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        #[weak]
        status,
        move |_| {
            let value = input.text().to_string();
            let valid = reqwest::Url::parse(&value)
                .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some());
            if !valid {
                status.set_text(&gettext("Enter a valid HTTP(S) playlist URL"));
                return;
            }
            dialog.close();
            replace_origin(&view, id.clone(), SourceSelection::Remote(value));
        }
    ));
    dialog.present(Some(&window));
}

fn replace_origin(view: &TvStreamsView, id: String, origin: SourceSelection) {
    let Some(service) = view.service() else {
        return;
    };
    spawn(glib::clone!(
        #[weak]
        view,
        async move {
            match service.replace_origin(&id, origin).await {
                Ok(result) => handle_refresh(&view, &service, result),
                Err(error) => fail(&view, error),
            }
        }
    ));
}

fn refresh_source(view: &TvStreamsView, id: String) {
    let Some(service) = view.service() else {
        return;
    };
    spawn(glib::clone!(
        #[weak]
        view,
        async move {
            match service.refresh(&id).await {
                Ok(result) => handle_refresh(&view, &service, result),
                Err(ServiceError::NeedsReauthorization) => {
                    reauthorize(&view, id);
                }
                Err(error) => fail(&view, error),
            }
        }
    ));
}

fn reauthorize(view: &TvStreamsView, id: String) {
    let Some(window) = parent(view) else {
        return;
    };
    let (dialog, body, _) = sheet(&gettext("Local Playlist Permission Expired"));
    let title = gtk::Label::new(Some(&gettext(
        "The cached channels are safe. Reselect the file to refresh this source.",
    )));
    title.set_wrap(true);
    body.append(&title);
    let actions = line(&body);
    let cancel = button(&gettext("Keep Existing"), &actions);
    let choose = button(&gettext("Reselect File"), &actions);
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    choose.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        move |_| {
            dialog.close();
            let Some(window) = parent(&view) else {
                return;
            };
            let id = id.clone();
            spawn(glib::clone!(
                #[weak]
                view,
                async move {
                    if let Some(file) = choose_local_file(&window).await {
                        replace_origin(&view, id, SourceSelection::Local(file));
                    }
                }
            ));
        }
    ));
    dialog.present(Some(&window));
}

fn handle_refresh(view: &TvStreamsView, service: &StreamsService, outcome: RefreshOutcome) {
    match outcome {
        RefreshOutcome::Committed(_) => {
            view.source_mutated();
            message(view, &gettext("Playlist refreshed"));
        }
        RefreshOutcome::ConfirmationRequired(ticket) => {
            confirm_preview(view, service.clone(), ticket)
        }
    }
}

fn confirm_preview(view: &TvStreamsView, service: StreamsService, ticket: RefreshTicket) {
    let Some(window) = parent(view) else {
        return;
    };
    let (dialog, body, _) = sheet(&gettext("Confirm Major Channel Reduction"));
    let preview = &ticket.preview;
    let warning = gtk::Label::new(Some(&format!(
        "{}\n{}: {} → {}\n{}: {} · {}: {} · {}: {}",
        gettext("Many existing channels would disappear. Keep existing is the safe default."),
        gettext("Channels"),
        preview.old_present,
        preview.new_present,
        gettext("Missing"),
        preview.missing,
        gettext("Added"),
        preview.added,
        gettext("Matched"),
        preview.matched,
    )));
    warning.set_wrap(true);
    warning.set_xalign(0.0);
    body.append(&warning);
    let actions = line(&body);
    let decline = button(&gettext("Keep Existing"), &actions);
    let approve = button(&gettext("Replace Channels"), &actions);
    approve.add_css_class("destructive-action");
    let pending = Rc::new(RefCell::new(Some(ticket)));
    decline.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    approve.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        #[strong]
        pending,
        #[strong]
        service,
        move |_| {
            if let Some(ticket) = pending.borrow_mut().take() {
                dialog.close();
                let service = service.clone();
                spawn(glib::clone!(
                    #[weak]
                    view,
                    async move {
                        match service.confirm_refresh(ticket, true).await {
                            Ok(_) => {
                                view.source_mutated();
                                message(&view, &gettext("Playlist replaced"));
                            }
                            Err(error) => fail(&view, error),
                        }
                    }
                ));
            }
        }
    ));
    dialog.connect_closed(move |_| {
        if let Some(ticket) = pending.borrow_mut().take() {
            let service = service.clone();
            spawn(async move {
                let _ = service.confirm_refresh(ticket, false).await;
            });
        }
    });
    dialog.present(Some(&window));
}

fn confirm_delete(view: &TvStreamsView, source: Source) {
    let Some(window) = parent(view) else {
        return;
    };
    let (dialog, body, status) = sheet(&gettext("Delete Source?"));
    let prompt = gtk::Label::new(Some(&format!(
        "{}\n{}",
        source.name,
        gettext("Remove this source and all of its imported channels? This cannot be undone.")
    )));
    prompt.set_wrap(true);
    body.append(&prompt);
    let actions = line(&body);
    let cancel = button(&gettext("Keep Source"), &actions);
    let delete = button(&gettext("Delete Permanently"), &actions);
    delete.add_css_class("destructive-action");
    body.append(&status);
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    delete.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        #[weak]
        status,
        #[weak]
        delete,
        move |_| {
            let Some(service) = view.service() else {
                return;
            };
            let confirmation = ConfirmedSourceDeletion::after_user_confirmation(&source);
            delete.set_sensitive(false);
            spawn(glib::clone!(
                #[weak]
                view,
                #[weak]
                dialog,
                #[weak]
                status,
                #[weak]
                delete,
                async move {
                    match service.delete_source(confirmation).await {
                        Ok(()) => {
                            dialog.close();
                            view.source_mutated();
                            message(&view, &gettext("Source deleted"));
                        }
                        Err(error) => {
                            status.set_text(&error.to_string());
                            delete.set_sensitive(true);
                        }
                    }
                }
            ));
        }
    ));
    dialog.present(Some(&window));
}

#[cfg(test)]
pub(crate) fn assert_synthetic_source_defaults_form() {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let defaults = DefaultsForm::new(&body, &SourceDefaults::default());
    let empty = defaults
        .collect()
        .expect("optional source defaults may be empty");
    assert!(empty.languages.is_empty() && empty.countries.is_empty() && empty.region.is_none());
    defaults.languages.choose("de");
    defaults.languages.choose("en");
    defaults.countries.choose("DE");
    defaults.countries.choose("AT");
    defaults.code.set_text("DE-BY");
    defaults.label.set_text("Bavaria");
    defaults.languages.pending_manual("German");
    assert!(
        defaults.collect().is_err(),
        "unapplied manual text must not be silently ignored"
    );
    defaults.languages.pending_manual("");
    let validated = defaults.collect().expect("source defaults from GTK fields");
    assert_eq!(validated.languages.len(), 2);
    assert_eq!(validated.countries.len(), 2);
    assert_eq!(
        validated.region.and_then(|region| region.code).as_deref(),
        Some("DE-BY")
    );
    defaults.countries.deselect("AT");
    assert_eq!(defaults.collect().unwrap().countries.len(), 1);
}

#[cfg(test)]
mod source_form_tests {
    use super::*;

    #[test]
    #[ignore = "requires a mapped display and isolated test GSettings"]
    fn osk_mapped_gtk_add_playlist_paired_rows_and_footer() {
        adw::init().expect("mapped dialog test needs GTK");
        let win = adw::Window::new();
        win.set_default_size(960, 720);
        win.present();

        let (dialog, body, status) = sheet("Add Playlist");
        let name = entry("");
        let url = entry("");
        url.set_input_purpose(gtk::InputPurpose::Url);
        let name_keyboard = editable_row(
            "Unique source name",
            &name,
            &body,
            crate::keyboard::KeyboardOptions::default(),
        );
        let url_keyboard = editable_row(
            "Playlist HTTP(S) URL",
            &url,
            &body,
            crate::keyboard::KeyboardOptions::url(),
        );
        for keyboard in [&name_keyboard, &url_keyboard] {
            crate::keyboard::style_add_playlist_field_launcher(keyboard);
        }

        let advanced = gtk::Expander::new(Some("Advanced"));
        let advanced_body = gtk::Box::new(gtk::Orientation::Vertical, 8);
        // Make the expanded state taller than the available window so the
        // scroller and pinned footer must coexist.
        for _ in 0..35 {
            advanced_body.append(&gtk::Label::new(Some("Optional source metadata")));
        }
        advanced.set_child(Some(&advanced_body));
        body.append(&advanced);
        let (scroll, footer) = source_form_layout(&dialog, &body, &advanced);
        let actions = line(&footer);
        let cancel = button("Cancel", &actions);
        let save = button("Save", &actions);
        footer.append(&status);

        dialog.present(Some(&win));
        let spin = || {
            for _ in 0..20 {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
                std::thread::sleep(Duration::from_millis(12));
            }
        };
        spin();

        assert_eq!(dialog.property::<i32>("content-height"), 360);
        for control in [
            name.clone().upcast::<gtk::Widget>(),
            url.clone().upcast::<gtk::Widget>(),
            name_keyboard.clone().upcast(),
            url_keyboard.clone().upcast(),
        ] {
            assert!(control.is_mapped() && control.is_sensitive());
            assert!(control.grab_focus(), "paired field control is not focusable");
        }
        assert!(
            name_keyboard.width() >= 48 && name_keyboard.height() >= 44,
            "source-name keyboard target too small: {}x{}",
            name_keyboard.width(),
            name_keyboard.height()
        );
        assert_eq!(
            (name_keyboard.width(), name_keyboard.height()),
            (url_keyboard.width(), url_keyboard.height()),
            "paired Add Playlist keyboard launchers must have matching allocations"
        );
        assert_eq!(
            name.height(),
            url.height(),
            "paired source-name and URL entries must be equally tall"
        );
        let name_rect = name.compute_bounds(&body).expect("name entry bounds");
        let url_rect = url.compute_bounds(&body).expect("URL entry bounds");
        assert!(
            (name_rect.x() - url_rect.x()).abs() <= 2.0
                && (name_rect.width() - url_rect.width()).abs() <= 2.0,
            "paired entries must align and have equal widths"
        );

        let footer_visible = || {
            let bounds = footer.compute_bounds(&dialog).expect("footer bounds");
            assert!(
                footer.is_mapped()
                    && bounds.y() >= -1.0
                    && bounds.y() + bounds.height() <= dialog.height() as f32 + 1.0,
                "footer must remain inside dialog viewport: y={} height={} dialog={}",
                bounds.y(),
                bounds.height(),
                dialog.height()
            );
            assert!(cancel.is_mapped() && save.is_mapped());
        };
        footer_visible();
        advanced.set_expanded(true);
        spin();
        assert_eq!(dialog.property::<i32>("content-height"), 620);
        let adjustment = scroll.vadjustment();
        assert!(
            adjustment.upper() > adjustment.page_size(),
            "expanded optional fields must remain scrollable"
        );
        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        spin();
        footer_visible();
        assert!(save.grab_focus(), "Save must remain D-pad focusable after scroll");
        assert!(cancel.grab_focus(), "Cancel must remain D-pad focusable after scroll");

        advanced.set_expanded(false);
        spin();
        assert_eq!(dialog.property::<i32>("content-height"), 360);
        footer_visible();
        dialog.close();
        win.destroy();
    }

    #[test]
    fn source_default_codes_and_optional_region_validate() {
        assert_eq!(
            crate::streams::model::normalize_facet("de", FacetDimension::Language).unwrap(),
            "de"
        );
        assert_eq!(
            crate::streams::model::normalize_facet("at", FacetDimension::Country).unwrap(),
            "AT"
        );
        assert!(crate::streams::model::normalize_facet("de-DE", FacetDimension::Country).is_err());
        assert!(region("", "").unwrap().is_none());
        assert!(region("DE-BY", "Bavaria").unwrap().is_some());
        assert!(region(" ", &"R".repeat(129)).is_err());
    }
}

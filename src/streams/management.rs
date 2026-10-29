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
        let code_keyboard = editable_row(
            &gettext("Default region code (optional)"),
            &code,
            box_,
            crate::keyboard::KeyboardOptions::default(),
        );
        let name_keyboard = editable_row(
            &gettext("Default region name (optional)"),
            &label,
            box_,
            crate::keyboard::KeyboardOptions::default(),
        );
        // Advanced region editors belong to the same source form: their
        // input/launcher heights must match the source-name and URL fields.
        for keyboard in [&code_keyboard, &name_keyboard] {
            crate::keyboard::style_add_playlist_field_launcher(keyboard);
        }
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
    // Marks the only modal whose Back key returns a focused source action
    // to its owning row. Other dialogs keep their normal close behavior.
    dialog.add_css_class("broadcaster-source-manager");
    dialog.set_content_height(650);
    let actions = line(&body);
    let add = button(&gettext("Add Playlist"), &actions);
    let guides = button(&gettext("Programme Guides"), &actions);
    let close = button(&gettext("Close"), &actions);
    guides.connect_clicked(glib::clone!(
        #[weak]
        view,
        move |_| {
            if let Some(window) = parent(&view) {
                crate::epg::desktop::show_guide_manager_with_close(
                    &window,
                    glib::clone!(
                        #[weak]
                        view,
                        move || view.epg_changed()
                    ),
                );
            }
        }
    ));
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
    view.remember_source_manager(&list, &status, &add);
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

// A row is the non-destructive portal; semantic slots are upper 0..4,
// lower 4..8. Navigation is explicit because GTK's geometric fallback can
// leave the two compact rows or land on a disabled edge move button.
const MANAGER_ROW: usize = 8;

fn manager_target(controls: &[glib::WeakRef<gtk::Widget>; 8], slot: usize) -> Option<gtk::Widget> {
    controls
        .get(slot)
        .and_then(glib::WeakRef::upgrade)
        .filter(|button| button.is_sensitive())
}

fn nearest_manager_target(
    controls: &[glib::WeakRef<gtk::Widget>; 8],
    slot: usize,
    start: usize,
    end: usize,
) -> Option<gtk::Widget> {
    (start..end)
        .filter(|candidate| {
            controls[*candidate]
                .upgrade()
                .is_some_and(|widget| widget.is_sensitive())
        })
        .min_by_key(|candidate| candidate.abs_diff(slot))
        .and_then(|candidate| manager_target(controls, candidate))
}

fn bind_manager_navigation(
    view: &TvStreamsView,
    list: &gtk::ListBox,
    row: &gtk::ListBoxRow,
    controls: &[gtk::Widget; 8],
    source_id: &str,
    index: i32,
) {
    row.set_focusable(true);
    // Focus entry captures identity even for ordinary GTK Tab navigation,
    // pointer clicks, and switch activation—not only handled D-pad keys.
    for (slot, target) in std::iter::once((MANAGER_ROW, row.clone().upcast::<gtk::Widget>()))
        .chain(controls.iter().cloned().enumerate())
    {
        let focus = gtk::EventControllerFocus::new();
        let id = source_id.to_owned();
        focus.connect_enter(glib::clone!(
            #[weak]
            view,
            #[weak]
            target,
            move |_| {
                if target.has_focus() {
                    view.set_manager_focus(&id, slot, index);
                }
            }
        ));
        target.add_controller(focus);

        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        // Only weak references in controllers: rebuilding rows must not leak
        // the old widgets through a controller -> button -> row cycle.
        let actions = std::array::from_fn(|i| controls[i].downgrade());
        let weak_list = list.downgrade();
        let weak_row = row.downgrade();
        key.connect_key_pressed(move |_, pressed, _, modifiers| {
            let (Some(list), Some(row)) = (weak_list.upgrade(), weak_row.upgrade()) else {
                return glib::Propagation::Proceed;
            };
            // A ListBoxRow's capture controller also sees events dispatched to
            // its children; the portal is active only while the row itself
            // actually owns focus, never while an action owns it.
            if slot == MANAGER_ROW && !row.has_focus() {
                return glib::Propagation::Proceed;
            }
            if !crate::keyboard::unmodified_remote(modifiers) {
                return glib::Propagation::Proceed;
            }
            use gtk::gdk::Key;
            let next = match pressed {
                Key::BackSpace if slot != MANAGER_ROW => Some(row.clone().upcast()),
                Key::Return | Key::KP_Enter | Key::space | Key::Right | Key::KP_Right
                    if slot == MANAGER_ROW =>
                {
                    manager_target(&actions, 0)
                }
                Key::Left | Key::KP_Left if slot != MANAGER_ROW => {
                    let first = if slot < 4 { 0 } else { 4 };
                    if slot == first {
                        Some(row.clone().upcast())
                    } else {
                        (first..slot)
                            .rev()
                            .find_map(|i| manager_target(&actions, i))
                            .or_else(|| Some(row.clone().upcast()))
                    }
                }
                Key::Right | Key::KP_Right if slot != MANAGER_ROW => {
                    let end = if slot < 4 { 4 } else { 8 };
                    ((slot + 1)..end).find_map(|i| manager_target(&actions, i))
                }
                Key::Down | Key::KP_Down if slot < 4 => {
                    nearest_manager_target(&actions, slot + 4, 4, 8)
                }
                Key::Up | Key::KP_Up if (4..8).contains(&slot) => {
                    nearest_manager_target(&actions, slot - 4, 0, 4)
                }
                Key::Up | Key::KP_Up if slot < 4 => Some(row.clone().upcast()),
                Key::Down | Key::KP_Down if slot == MANAGER_ROW || (4..8).contains(&slot) => {
                    list.row_at_index(index + 1).map(|item| item.upcast())
                }
                Key::Up | Key::KP_Up if slot == MANAGER_ROW => {
                    list.row_at_index(index - 1).map(|item| item.upcast())
                }
                _ => None,
            };
            if let Some(next) = next {
                if next.grab_focus() {
                    return glib::Propagation::Stop;
                }
            }
            // Escape/Home remain under the existing modal/window router.
            glib::Propagation::Proceed
        });
        target.add_controller(key);
    }
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
            let restore = view.manager_focus();
            view.manager_rebuilding(true);
            let mut focus_rows: Vec<(String, gtk::ListBoxRow, [gtk::Widget; 8])> = Vec::new();
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
                // Keep both action lines aligned in compact and TV layouts.
                upper_actions.set_homogeneous(true);
                lower_actions.set_homogeneous(true);
                panel.append(&upper_actions);
                panel.append(&lower_actions);
                let row = gtk::ListBoxRow::new();
                row.set_child(Some(&panel));
                let controls: [gtk::Widget; 8] = [
                    enabled.clone().upcast(),
                    refresh.clone().upcast(),
                    edit.clone().upcast(),
                    rename.clone().upcast(),
                    change.clone().upcast(),
                    remove.clone().upcast(),
                    up.clone().upcast(),
                    down.clone().upcast(),
                ];
                bind_manager_navigation(&view, &list, &row, &controls, &source.id, index as i32);
                focus_rows.push((source.id.clone(), row.clone(), controls));
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
            view.manager_rebuilding(false);
            if focus_rows.is_empty() {
                view.focus_empty_source_manager();
            } else {
                // Preserve the logical source across asynchronous switch, rename,
                // replace and reorder. A deletion selects the adjacent surviving
                // row at the previous position, or the previous last row.
                let chosen = restore
                    .as_ref()
                    .and_then(|(id, _, _)| {
                        focus_rows
                            .iter()
                            .position(|(source_id, _, _)| source_id == id)
                    })
                    .or_else(|| {
                        restore
                            .as_ref()
                            .map(|(_, _, index)| (*index).max(0) as usize)
                            .map(|index| index.min(focus_rows.len() - 1))
                    })
                    .unwrap_or(0);
                let (id, row, controls) = &focus_rows[chosen];
                let slot = restore
                    .as_ref()
                    .map_or(MANAGER_ROW, |(_, action, _)| *action);
                let references = std::array::from_fn(|i| controls[i].downgrade());
                let target = if slot == MANAGER_ROW {
                    None
                } else {
                    manager_target(&references, slot)
                };
                let restored_slot = if target.is_some_and(|target| target.grab_focus()) {
                    slot
                } else {
                    row.grab_focus();
                    MANAGER_ROW
                };
                view.set_manager_focus(id, restored_slot, chosen as i32);
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
        let defaults = DefaultsForm::new(&advanced_body, &SourceDefaults::default());
        // Make the expanded state taller than the available window so the
        // scroller and pinned footer must coexist.
        for _ in 0..25 {
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
            assert!(
                control.grab_focus(),
                "paired field control is not focusable"
            );
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
        for field in [&defaults.code, &defaults.label] {
            let launcher = field
                .parent()
                .expect("Advanced region row")
                .last_child()
                .expect("Advanced region keyboard")
                .downcast::<gtk::Button>()
                .expect("GTK button in region row");
            assert_eq!(
                (launcher.width(), launcher.height()),
                (name_keyboard.width(), name_keyboard.height()),
                "Advanced region keyboard launcher must match paired form rows"
            );
            assert_eq!(
                field.height(),
                name.height(),
                "Advanced region field height must match source-name field"
            );
            assert!(launcher.is_mapped() && launcher.is_sensitive());
        }
        let adjustment = scroll.vadjustment();
        assert!(
            adjustment.upper() > adjustment.page_size(),
            "expanded optional fields must remain scrollable"
        );
        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        spin();
        footer_visible();
        assert!(
            save.grab_focus(),
            "Save must remain D-pad focusable after scroll"
        );
        assert!(
            cancel.grab_focus(),
            "Cancel must remain D-pad focusable after scroll"
        );

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
#[cfg(test)]
mod c1_mapped_native_diagnostic {
    use super::*;
    use crate::{application::TvApplication, streams::view::TvStreamsView};
    use std::{thread, time::Duration};

    fn spin() {
        for _ in 0..20 {
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
            thread::sleep(Duration::from_millis(16));
        }
    }

    fn inject(key: &str) {
        let script = r#"
import ctypes as c, sys
x,t = c.CDLL('libX11.so.6'),c.CDLL('libXtst.so.6')
x.XOpenDisplay.argtypes=[c.c_char_p];x.XOpenDisplay.restype=c.c_void_p
d=x.XOpenDisplay(None); assert d
x.XDefaultRootWindow.argtypes=[c.c_void_p]; x.XDefaultRootWindow.restype=c.c_ulong
x.XQueryTree.argtypes=[c.c_void_p,c.c_ulong,c.POINTER(c.c_ulong),c.POINTER(c.c_ulong),c.POINTER(c.POINTER(c.c_ulong)),c.POINTER(c.c_uint)];x.XQueryTree.restype=c.c_int
root=x.XDefaultRootWindow(d)
a,b,n=c.c_ulong(),c.c_ulong(),c.c_uint();ch=c.POINTER(c.c_ulong)()
assert x.XQueryTree(d,root,c.byref(a),c.byref(b),c.byref(ch),c.byref(n))
assert n.value
x.XSetInputFocus.argtypes=[c.c_void_p,c.c_ulong,c.c_int,c.c_ulong]
if sys.argv[2]=='initial': x.XSetInputFocus(d,ch[n.value-1],1,0)
x.XFree.argtypes=[c.c_void_p];x.XFree(c.cast(ch,c.c_void_p))
x.XStringToKeysym.argtypes=[c.c_char_p];x.XStringToKeysym.restype=c.c_ulong
x.XKeysymToKeycode.argtypes=[c.c_void_p,c.c_ulong];x.XKeysymToKeycode.restype=c.c_uint
sym=x.XStringToKeysym(sys.argv[1].encode());code=x.XKeysymToKeycode(d,sym);assert code,(sys.argv[1],sym)
t.XTestFakeKeyEvent.argtypes=[c.c_void_p,c.c_uint,c.c_int,c.c_ulong]
assert t.XTestFakeKeyEvent(d,code,1,0); assert t.XTestFakeKeyEvent(d,code,0,0)
x.XFlush.argtypes=[c.c_void_p];x.XFlush(d);x.XCloseDisplay.argtypes=[c.c_void_p];x.XCloseDisplay(d)
"#;
        let mode = if key == "Return" { "initial" } else { "keep" };
        let r = std::process::Command::new("python3")
            .args(["-c", script, key, mode])
            .output()
            .expect("XTest");
        assert!(
            r.status.success(),
            "XTest {key}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        spin();
    }

    fn find_view(widget: &gtk::Widget) -> Option<TvStreamsView> {
        if let Ok(v) = widget.clone().downcast::<TvStreamsView>() {
            return Some(v);
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Some(v) = find_view(&w) {
                return Some(v);
            }
            child = w.next_sibling();
        }
        None
    }

    #[test]
    #[ignore = "native XTest mapped synthetic manager; diagnostic only"]
    fn c1_actual_remote_back_trace() {
        adw::init().expect("display");
        let resource =
            gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
        gio::resources_register(&resource);
        let app = TvApplication::new();
        app.register(None::<&gio::Cancellable>)
            .expect("application startup");
        let win = crate::window::TvWindow::new(&app);
        win.set_default_size(640, 480);
        win.present();
        spin();
        let view = find_view(win.upcast_ref()).expect("stream view");
        let (dialog, body, _status) = sheet("C1 mapped native diagnostic");
        dialog.add_css_class("broadcaster-source-manager");
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        let row = gtk::ListBoxRow::new();
        let upper = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let enabled = gtk::Switch::new();
        upper.append(&enabled);
        let mut buttons: Vec<gtk::Button> = Vec::new();
        for i in 1..8 {
            let b = gtk::Button::with_label(&format!("Action{i}"));
            if i < 4 {
                upper.append(&b)
            };
            buttons.push(b);
        }
        let lower = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        for b in buttons.iter().skip(3) {
            lower.append(b)
        }
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 3);
        panel.append(&upper);
        panel.append(&lower);
        row.set_child(Some(&panel));
        list.append(&row);
        body.append(&list);
        let actions: [gtk::Widget; 8] = std::array::from_fn(|i| {
            if i == 0 {
                enabled.clone().upcast()
            } else {
                buttons[i - 1].clone().upcast()
            }
        });
        bind_manager_navigation(&view, &list, &row, &actions, "fixture-source", 0);
        dialog.present(Some(&win));
        spin();
        assert!(row.grab_focus(), "portal focus");
        inject("Return");
        assert!(
            enabled.has_focus(),
            "row OK portal must focus Switch without toggling"
        );
        inject("XF86Back");
        assert!(
            row.has_focus(),
            "native XF86Back from switch returns to owning source row"
        );
        assert!(dialog.is_visible(), "first Back keeps manager open");
        inject("XF86Back");
        assert!(!row.has_focus(), "second Back on row dismisses manager");
    }
}

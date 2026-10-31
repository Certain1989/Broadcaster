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
            show_file_picker(&view, None, None);
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

fn show_file_picker(
    view: &TvStreamsView,
    replacement: Option<Source>,
    retired_picker: Option<glib::WeakRef<gtk::Button>>,
) {
    let Some(window) = parent(view) else {
        return;
    };
    spawn(glib::clone!(
        #[weak]
        view,
        async move {
            if let Some(file) = choose_local_file(&window).await {
                if let Some(source) = replacement {
                    replace_origin_with_picker_focus(
                        &view,
                        source.id,
                        SourceSelection::Local(file),
                        retired_picker,
                    );
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

// A directional hop can converge on the same enabled lower control when the
// corresponding Move button is disabled. Keep the immediate inverse locally:
// independent nearest-neighbour searches are not reversible at those edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManagerVerticalHop {
    from: usize,
    to: usize,
    reverse_up: bool,
}

impl ManagerVerticalHop {
    fn return_slot(self, slot: usize, pressed: gtk::gdk::Key) -> Option<usize> {
        use gtk::gdk::Key;
        let opposite = if self.reverse_up {
            matches!(pressed, Key::Up | Key::KP_Up)
        } else {
            matches!(pressed, Key::Down | Key::KP_Down)
        };
        (self.to == slot && opposite).then_some(self.from)
    }
}

fn nearest_manager_slot(
    slot: usize,
    start: usize,
    end: usize,
    mut enabled: impl FnMut(usize) -> bool,
) -> Option<usize> {
    (start..end)
        .filter(|candidate| enabled(*candidate))
        .min_by_key(|candidate| candidate.abs_diff(slot))
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
    // Owned only by this row's controllers, never by an async service or a
    // reusable widget pointer. Rebinding drops the old row and its history.
    let vertical_hop: Rc<RefCell<Option<ManagerVerticalHop>>> = Rc::new(RefCell::new(None));
    // Observe direct focus even if the toplevel temporarily lacks global
    // input focus. The focus controller's is-focus property changes for Tab,
    // pointer and programmatic focus moves; enter/Widget::has_focus can miss
    // them when the window is not globally focused or signal order differs.
    for (slot, target) in std::iter::once((MANAGER_ROW, row.clone().upcast::<gtk::Widget>()))
        .chain(controls.iter().cloned().enumerate())
    {
        let id = source_id.to_owned();
        let focus_hop = vertical_hop.clone();
        let focus = gtk::EventControllerFocus::new();
        focus.connect_notify_local(
            Some("is-focus"),
            glib::clone!(
                #[weak]
                view,
                move |controller, _| {
                    if controller.is_focus() {
                        // An external move invalidates the old hop. For a
                        // handled vertical key, the key handler records the
                        // new hop after grab_focus has updated this property.
                        focus_hop.borrow_mut().take();
                        view.set_manager_focus(&id, slot, index);
                    }
                }
            ),
        );
        target.add_controller(focus);

        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        // Only weak widget references in controllers: rebuilding rows must
        // not leak old widgets through a controller -> row cycle.
        let actions = std::array::from_fn(|i| controls[i].downgrade());
        let weak_list = list.downgrade();
        let weak_row = row.downgrade();
        let key_hop = vertical_hop.clone();
        key.connect_key_pressed(move |_, pressed, _, modifiers| {
            let (Some(list), Some(row)) = (weak_list.upgrade(), weak_row.upgrade()) else {
                key_hop.borrow_mut().take();
                return glib::Propagation::Proceed;
            };
            // A ListBoxRow capture controller also sees keys on its children.
            // Let the actual focused action own these events.
            if slot == MANAGER_ROW && !row.has_focus() {
                return glib::Propagation::Proceed;
            }
            let previous = key_hop.borrow_mut().take();
            if !crate::keyboard::unmodified_remote(modifiers) {
                return glib::Propagation::Proceed;
            }
            use gtk::gdk::Key;
            // A single immediate reverse key goes to the *actual* previous
            // control, even when two upper actions share one lower fallback.
            if let Some(from) = previous.and_then(|hop| hop.return_slot(slot, pressed)) {
                if manager_target(&actions, from).is_some_and(|target| target.grab_focus()) {
                    return glib::Propagation::Stop;
                }
            }
            let vertical_slot = match pressed {
                Key::Down | Key::KP_Down if slot < 4 => {
                    nearest_manager_slot(slot + 4, 4, 8, |i| manager_target(&actions, i).is_some())
                }
                Key::Up | Key::KP_Up if (4..8).contains(&slot) => {
                    nearest_manager_slot(slot - 4, 0, 4, |i| manager_target(&actions, i).is_some())
                }
                _ => None,
            };
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
                    vertical_slot.and_then(|i| manager_target(&actions, i))
                }
                Key::Up | Key::KP_Up if (4..8).contains(&slot) => {
                    vertical_slot.and_then(|i| manager_target(&actions, i))
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
                    if let Some(to) = vertical_slot {
                        key_hop.replace(Some(ManagerVerticalHop {
                            from: slot,
                            to,
                            reverse_up: matches!(pressed, Key::Down | Key::KP_Down),
                        }));
                    }
                    return glib::Propagation::Stop;
                }
            }
            // Escape/Home and unhandled keys stay with the modal/window.
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
                let restored_slot = if target.as_ref().is_some_and(|target| target.grab_focus()) {
                    slot
                } else {
                    row.grab_focus();
                    MANAGER_ROW
                };
                view.set_manager_focus(id, restored_slot, chosen as i32);
                // Closing an editor can finish unmapping its modal after the
                // service has already rebuilt the manager rows. The initial
                // grab_focus above may succeed transiently, then GTK clears
                // the focus owner as the editor closes. Restore only if focus
                // is still absent, and only for this live manager generation.
                // Never steal a subsequently mapped user-chosen focus target.
                let bound_row = row.clone();
                let expected = id.clone();
                let weak_view = view.downgrade();
                let preferred = target.unwrap_or_else(|| row.clone().upcast());
                glib::timeout_add_local_once(Duration::from_millis(50), move || {
                    let Some(view) = weak_view.upgrade() else {
                        return;
                    };
                    if view.manager_generation() != generation || !bound_row.is_mapped() {
                        return;
                    }
                    if view
                        .manager_focus()
                        .is_none_or(|(current, _, _)| current != expected)
                    {
                        return;
                    }
                    let Some(window) = bound_row.root().and_downcast::<gtk::Window>() else {
                        return;
                    };
                    if gtk::prelude::GtkWindowExt::focus(&window)
                        .is_none_or(|focused| !focused.is_mapped())
                        && !preferred.grab_focus()
                    {
                        bound_row.grab_focus();
                    }
                });
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
    let retired_picker = file.downgrade();
    file.connect_clicked(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        move |_| {
            let retired_picker = retired_picker.clone();
            // GtkFileDialog is modal. Opening it before this AdwDialog has
            // actually closed can leave its former button as the GTK focus
            // owner even after the chooser returns to the source manager.
            // Start the native chooser only after GTK completes the close.
            let replacement = file_source.clone();
            dialog.connect_closed(glib::clone!(
                #[weak]
                view,
                move |_| show_file_picker(
                    &view,
                    Some(replacement.clone()),
                    Some(retired_picker.clone())
                )
            ));
            dialog.close();
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
    replace_origin_with_picker_focus(view, id, origin, None);
}

// Native GTK file choosers may leave a previously closed editor button as
// the focused widget. Only repair this specific stale focus owner, and only
// after the source replacement and manager rebuild have completed.
fn replace_origin_with_picker_focus(
    view: &TvStreamsView,
    id: String,
    origin: SourceSelection,
    retired_picker: Option<glib::WeakRef<gtk::Button>>,
) {
    let Some(service) = view.service() else {
        return;
    };
    spawn(glib::clone!(
        #[weak]
        view,
        async move {
            match service.replace_origin(&id, origin).await {
                Ok(result) => {
                    let committed = matches!(result, RefreshOutcome::Committed(_));
                    handle_refresh(&view, &service, result);
                    if committed {
                        if let Some(picker) = retired_picker {
                            restore_manager_after_picker(&view, &id, picker);
                        }
                    }
                }
                Err(error) => fail(&view, error),
            }
        }
    ));
}

fn restore_manager_after_picker(
    view: &TvStreamsView,
    expected_id: &str,
    picker: glib::WeakRef<gtk::Button>,
) {
    let weak_view = view.downgrade();
    let expected_id = expected_id.to_owned();
    let mut attempts = 0;
    glib::timeout_add_local(Duration::from_millis(100), move || {
        attempts += 1;
        let Some(view) = weak_view.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Some(retired) = picker.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Some(window) = parent(&view) else {
            return glib::ControlFlow::Break;
        };
        // Never steal a valid focus target chosen after the chooser finished.
        let stale = gtk::prelude::GtkWindowExt::focus(&window)
            .and_downcast::<gtk::Button>()
            .is_some_and(|focus| {
                focus == retired
                    || focus.label().as_deref() == Some(&gettext("Reselect Local File"))
            });
        if !stale {
            // The native chooser may relinquish focus before GTK finishes
            // restoring the old (now-retired) editor focus owner. Observe
            // the bounded handoff without overriding any newer user focus.
            return if attempts >= 15 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            };
        }
        let Some((current, _, index)) = view.manager_focus() else {
            return glib::ControlFlow::Break;
        };
        if current != expected_id {
            return glib::ControlFlow::Break;
        }
        if let Some(list) = view.live_source_manager_list() {
            if let Some(manager) = list
                .ancestor(adw::Dialog::static_type())
                .and_downcast::<adw::Dialog>()
            {
                if manager.is_mapped() {
                    if let Some(row) = list.row_at_index(index) {
                        manager.present(Some(&window));
                        if row.grab_focus() {
                            return glib::ControlFlow::Break;
                        }
                    }
                }
            }
        }
        if attempts >= 15 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
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
mod c1_vertical_reversibility_tests {
    use super::*;

    #[test]
    fn disabled_edge_fallbacks_are_immediately_reversible() {
        use gtk::gdk::Key;
        let mut checked = 0;
        for count in [1_usize, 3] {
            for position in 0..count {
                let mut enabled = [true; 8];
                enabled[6] = position > 0;
                enabled[7] = position + 1 < count;
                for origin in 0..4 {
                    let dest = nearest_manager_slot(origin + 4, 4, 8, |i| enabled[i])
                        .expect("lower action exists");
                    let hop = ManagerVerticalHop {
                        from: origin,
                        to: dest,
                        reverse_up: true,
                    };
                    assert_eq!(hop.return_slot(dest, Key::Up), Some(origin));
                    assert_eq!(hop.return_slot(dest, Key::KP_Up), Some(origin));
                    assert_eq!(hop.return_slot(dest, Key::Down), None);
                    assert_eq!(hop.return_slot(origin, Key::Up), None);
                    checked += 1;
                }
                for origin in 4..8 {
                    if !enabled[origin] {
                        continue;
                    }
                    let dest = nearest_manager_slot(origin - 4, 0, 4, |i| enabled[i])
                        .expect("upper action exists");
                    let hop = ManagerVerticalHop {
                        from: origin,
                        to: dest,
                        reverse_up: false,
                    };
                    assert_eq!(hop.return_slot(dest, Key::Down), Some(origin));
                    assert_eq!(hop.return_slot(dest, Key::KP_Down), Some(origin));
                    assert_eq!(hop.return_slot(dest, Key::Up), None);
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 28, "all one-/three-source enabled vertical edges");
    }
}

#[cfg(test)]
mod c1_mapped_native_diagnostic {
    use super::*;
    use crate::{application::TvApplication, streams::view::TvStreamsView};
    use std::{thread, time::Duration};

    pub(super) fn spin() {
        for _ in 0..20 {
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
            thread::sleep(Duration::from_millis(16));
        }
    }

    pub(super) fn inject(key: &str) {
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
# Let a real window manager own input focus. A root child is not always a
# focusable client window (e.g. Openbox supporting windows).
import subprocess,shutil
has_wm = shutil.which('xprop') is not None and subprocess.run(
    ['xprop','-root','_NET_SUPPORTING_WM_CHECK'],
    capture_output=True,text=True).stdout.find('window id #') >= 0
# A nested GTK dialog may add an unmapped override-redirect root child.
# XSetInputFocus on that child raises BadMatch before XTest can deliver a
# key. Focus the largest *viewable* InputOutput client, not the last child.
class Attr(c.Structure):
    _fields_=[('x',c.c_int),('y',c.c_int),('width',c.c_int),('height',c.c_int),
      ('border_width',c.c_int),('depth',c.c_int),('visual',c.c_void_p),
      ('root',c.c_ulong),('class_',c.c_int),('bit_gravity',c.c_int),
      ('win_gravity',c.c_int),('backing_store',c.c_int),
      ('backing_planes',c.c_ulong),('backing_pixel',c.c_ulong),
      ('save_under',c.c_int),('colormap',c.c_ulong),('map_installed',c.c_int),
      ('map_state',c.c_int),('all_event_masks',c.c_long),
      ('your_event_mask',c.c_long),('do_not_propagate_mask',c.c_long),
      ('override_redirect',c.c_int),('screen',c.c_void_p)]
x.XGetWindowAttributes.argtypes=[c.c_void_p,c.c_ulong,c.POINTER(Attr)]
x.XGetWindowAttributes.restype=c.c_int
if sys.argv[2]=='initial' and not has_wm:
    usable=[]
    for i in range(n.value):
        a=Attr()
        if x.XGetWindowAttributes(d,ch[i],c.byref(a)) and a.map_state==2 and a.class_==1:
            usable.append((a.width*a.height,int(ch[i])))
    assert usable,'no mapped X11 client window'
    x.XSetInputFocus(d,max(usable)[1],1,0)
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

    pub(super) fn find_view(widget: &gtk::Widget) -> Option<TvStreamsView> {
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
        run_mapped_manager(640, 480);
    }

    #[test]
    #[ignore = "native XTest mapped synthetic manager, TV viewport; diagnostic only"]
    fn c1_actual_remote_back_trace_tv() {
        run_mapped_manager(1280, 800);
    }

    /// C4 keeps the existing physical XTest injector. Direct router calls
    /// cannot prove that a mapped list actually receives the remote key.
    fn c4_focused_button(win: &crate::window::TvWindow) -> Option<gtk::Button> {
        gtk::prelude::GtkWindowExt::focus(win)?
            .downcast::<gtk::Button>()
            .ok()
    }

    fn c4_native_cards(width: i32, height: i32, negative: bool) {
        adw::init().expect("GTK display");
        let resource =
            gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
        gio::resources_register(&resource);
        let app = TvApplication::new();
        app.register(None::<&gio::Cancellable>)
            .expect("application register");
        let win = crate::window::TvWindow::new(&app);
        win.set_default_size(width, height);
        win.present();
        spin();
        fn stack(w: &gtk::Widget) -> Option<adw::ViewStack> {
            if let Ok(stack) = w.clone().downcast::<adw::ViewStack>() {
                return Some(stack);
            }
            let mut child = w.first_child();
            while let Some(widget) = child {
                if let Some(stack) = stack(&widget) {
                    return Some(stack);
                }
                child = widget.next_sibling();
            }
            None
        }
        let pages = stack(win.upcast_ref()).expect("main navigation stack");
        pages.set_visible_child_name("streams");
        spin();
        let view = find_view(win.upcast_ref()).expect("My Channels");
        let ids = view.install_remote_action_test_rows();
        spin();
        for (index, id) in ids.iter().enumerate() {
            view.remote_action_test_select(index as u32);
            spin();
            let previous = view.remote_action_test_counts().0;
            // First Return seeds X11 window focus; subsequent directions
            // genuinely traverse the GTK action graph.
            if index == 0 {
                inject("Return");
            } else {
                inject("Right");
            }
            let play = c4_focused_button(&win).unwrap_or_else(|| {
                panic!(
                    "Play action focus missing on row {index}: {}",
                    view.remote_action_test_state()
                )
            });
            assert_eq!(play.label().as_deref(), Some("Play"));
            assert!(play.is_mapped() && play.has_focus());
            view.assert_remote_action_focus_owner(play.upcast_ref(), id);
            assert!(
                view.card_mapping_context(id).is_some(),
                "bound mapping context"
            );
            assert_eq!(
                view.remote_action_test_counts().0,
                previous,
                "entry auto-played"
            );
            assert_eq!(
                view.remote_action_test_selected_id().as_deref(),
                Some(id.as_str())
            );
            if negative {
                assert_eq!(
                    play.label().as_deref(),
                    Some("Edit Metadata"),
                    "C4_EXPECTED_NEGATIVE_ORACLE: Right/OK entry cannot focus Edit"
                );
            }
            inject("Return");
            assert_eq!(view.remote_action_test_counts().0, previous + 1);
            inject("Left");
            let focused = gtk::prelude::GtkWindowExt::focus(&win).expect("row");
            view.assert_remote_action_focus_owner(&focused, id);
            inject("XF86Back");
            assert!(view.remote_action_test_state().contains("expanded=None"));
            assert_eq!(pages.visible_child_name().as_deref(), Some("streams"));
            inject("Left");
            assert_eq!(
                c4_focused_button(&win)
                    .and_then(|button| button.label())
                    .as_deref(),
                Some("☆"),
                "collapsed star without expansion"
            );
            inject("Right");
            let focused = gtk::prelude::GtkWindowExt::focus(&win).expect("row after star");
            view.assert_remote_action_focus_owner(&focused, id);
            inject("Right");
            assert_eq!(
                c4_focused_button(&win)
                    .and_then(|button| button.label())
                    .as_deref(),
                Some("Play")
            );
            inject("Down");
            assert_eq!(
                c4_focused_button(&win)
                    .and_then(|button| button.label())
                    .as_deref(),
                Some("Edit Metadata")
            );
            inject("Down");
            assert_eq!(
                c4_focused_button(&win)
                    .and_then(|button| button.label())
                    .as_deref(),
                Some("Programme mapping")
            );
            inject("Left");
            let focused = gtk::prelude::GtkWindowExt::focus(&win).expect("mapping return");
            view.assert_remote_action_focus_owner(&focused, id);
            inject("XF86Back");
            assert!(view.remote_action_test_state().contains("expanded=None"));
            assert!(
                view.card_mapping_context(id).is_none(),
                "collapsed picker context"
            );
        }
    }

    #[test]
    #[ignore = "native XTest mapped C4 cards"]
    fn c4_native_cards_640_480() {
        c4_native_cards(640, 480, false);
    }

    #[test]
    #[ignore = "native XTest mapped C4 cards"]
    fn c4_native_cards_1280_800() {
        c4_native_cards(1280, 800, false);
    }

    #[test]
    #[ignore = "deliberately wrong physical focus oracle"]
    fn c4_native_negative_oracle() {
        c4_native_cards(640, 480, true);
    }

    /// Test the real star click, persisted SQLite preference, filter and
    /// zero-results focus return, not merely a fabricated glyph/focus route.
    /// The CI wrapper must provide a fresh isolated XDG_DATA_HOME.
    fn c4_native_favourites(width: i32, height: i32, painted: bool, negative: bool) {
        use crate::streams::{
            model::{ChannelQuery, IncomingChannel, SourceDraft, SourceOrigin, ValidatedSnapshot},
            storage::Storage,
        };
        use std::collections::BTreeSet;

        let data_home = std::env::var("XDG_DATA_HOME").expect("isolated C4 XDG_DATA_HOME");
        assert!(
            data_home.contains("broadcaster-c4-fixture-"),
            "refusing to use a real user's My Channels database"
        );
        let store = Storage::new();
        store.initialize().expect("disposable SQLite database");
        let source_id = uuid::Uuid::new_v4().to_string();
        let source = SourceDraft {
            name: "C4 disposable source".into(),
            origin: SourceOrigin {
                kind: OriginKind::RemoteUrl,
                locator: "https://example.invalid/c4.m3u".into(),
            },
            enabled: true,
            order: 0,
            defaults: SourceDefaults::default(),
        };
        let entries: Vec<IncomingChannel> = (0..3)
            .map(|i| IncomingChannel {
                order: i,
                imported_name: format!("C4 channel {i}"),
                stream_url: format!("https://example.invalid/c4/{i}.m3u8"),
                tvg_id: Some(format!("c4-{i}")),
                tvg_name: None,
                group_title: None,
                logo_url: None,
                channel_number: None,
                region: None,
                languages: BTreeSet::new(),
                countries: BTreeSet::new(),
                user_agent: None,
                referer: None,
            })
            .collect();
        let snapshot = ValidatedSnapshot::new(source_id.clone(), entries).unwrap();
        store
            .create_source_with_initial_snapshot(&source_id, &source, &snapshot, 1_800_000_000)
            .unwrap();
        let all = store.query_channels(&ChannelQuery::default()).unwrap();
        assert_eq!(all.total, 3);
        let first_id = all.items[0].id.clone();
        let service = glib::MainContext::default()
            .block_on(StreamsService::with_storage(store.clone()))
            .expect("service-backed test");
        adw::init().expect("GTK display");
        let resource =
            gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
        gio::resources_register(&resource);
        let app = TvApplication::new();
        app.register(None::<&gio::Cancellable>)
            .expect("application register");
        let win = crate::window::TvWindow::new(&app);
        win.set_default_size(width, height);
        win.present();
        spin();
        fn stack(w: &gtk::Widget) -> Option<adw::ViewStack> {
            if let Ok(stack) = w.clone().downcast::<adw::ViewStack>() {
                return Some(stack);
            }
            let mut child = w.first_child();
            while let Some(widget) = child {
                if let Some(stack) = stack(&widget) {
                    return Some(stack);
                }
                child = widget.next_sibling();
            }
            None
        }
        // The isolated mapped-X11 run does not execute main.rs startup CSS;
        // register the real application stylesheet for painted-focus oracles.
        if painted {
            let css = gtk::CssProvider::new();
            css.load_from_resource("/io/github/Certain1989/Broadcaster/broadcaster.css");
            gtk::style_context_add_provider_for_display(
                &gtk::prelude::WidgetExt::display(&win),
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        let pages = stack(win.upcast_ref()).expect("main navigation");
        pages.set_visible_child_name("streams");
        spin();
        let view = find_view(win.upcast_ref()).expect("My Channels");
        view.install_remote_action_test_service(service, false);
        let wait_until = |predicate: &dyn Fn() -> bool, reason: &str| {
            for _ in 0..40 {
                spin();
                if predicate() {
                    return;
                }
            }
            panic!("C4 timed out: {reason}");
        };
        wait_until(
            &|| {
                view.remote_action_test_visible_count() == 3
                    && view.remote_action_test_row_star(&first_id).is_some()
            },
            "SQLite rows bound",
        );
        // R07: use only one real Search focus seed. Descend through the
        // mapped toolbar/filters into the actual three-row virtualized list
        // with XTest, never the show_and_focus_test_results helper.
        let search = view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus(), "R07 My Channels Search entry");
        spin();
        for _ in 0..16 {
            let owner = gtk::prelude::GtkWindowExt::focus(&win)
                .expect("R07 mapped My Channels focus owner");
            if view.contains_results_focus(&owner) {
                break;
            }
            inject("Down");
        }
        let owner =
            gtk::prelude::GtkWindowExt::focus(&win).expect("R07 physical Search-to-results owner");
        assert!(
            owner.is_mapped() && view.contains_results_focus(&owner),
            "R07 Search must reach real virtualized SQLite results through D-pad"
        );
        let row_ids: Vec<_> = all.items.iter().map(|row| row.id.clone()).collect();
        // Traverse and reverse three distinct source-qualified SQLite rows;
        // while each is collapsed, enter and leave that same row's real Star
        // with physical Left/Right without changing persisted preferences.
        for (index, id) in row_ids.iter().enumerate() {
            if index > 0 {
                inject("Down");
            }
            assert_eq!(
                view.remote_action_test_selected_id().as_deref(),
                Some(id.as_str()),
                "R07 source-qualified native row selection {index}"
            );
            inject("Left");
            let star = view
                .remote_action_test_row_star(id)
                .expect("R07 mapped star in actual selected SQLite row");
            assert!(star.has_focus() && star.is_mapped());
            assert_eq!(star.label().as_deref(), Some("☆"));
            inject("Right");
            let owner =
                gtk::prelude::GtkWindowExt::focus(&win).expect("R07 star-to-row physical return");
            view.assert_remote_action_focus_owner(&owner, id);
            // E08: every *real SQLite-bound* first/middle/last card must
            // offer all three actions and restore the exact card identity.
            // Fake player/editor/picker counters must remain untouched.
            inject("Return");
            for label in ["Play", "Edit Metadata", "Programme mapping"] {
                let action = c4_focused_button(&win).expect("R07 bound card action");
                assert_eq!(action.label().as_deref(), Some(label));
                assert!(action.is_mapped() && action.is_sensitive());
                view.assert_remote_action_focus_owner(action.upcast_ref(), id);
                if label != "Programme mapping" {
                    inject("Down");
                }
            }
            inject("Left");
            view.assert_remote_action_focus_owner(
                &gtk::prelude::GtkWindowExt::focus(&win).expect("R07 card inverse"),
                id,
            );
            inject("XF86Back");
            assert!(view.remote_action_test_state().contains("expanded=None"));
            assert_eq!(view.remote_action_test_counts(), (0, 0, 0));
        }
        for index in [1, 0] {
            inject("Up");
            assert_eq!(
                view.remote_action_test_selected_id().as_deref(),
                Some(row_ids[index].as_str()),
                "R07 physical reverse row selection {index}"
            );
        }
        assert_eq!(view.remote_action_test_counts(), (0, 0, 0));
        inject("Return");
        assert_eq!(
            c4_focused_button(&win).and_then(|b| b.label()).as_deref(),
            Some("Play")
        );
        view.assert_remote_action_focus_owner(
            &gtk::prelude::GtkWindowExt::focus(&win).unwrap(),
            &first_id,
        );
        // R07 E08: these are the three actual source-qualified SQLite card
        // actions, reached physically without triggering player, editor or
        // mapping writes. Keep the persisted-favourite journey below intact.
        for label in ["Edit Metadata", "Programme mapping"] {
            inject("Down");
            let focused = c4_focused_button(&win).expect("R07 real SQLite action focus");
            assert_eq!(focused.label().as_deref(), Some(label));
            assert!(focused.is_mapped() && focused.is_sensitive());
            view.assert_remote_action_focus_owner(focused.upcast_ref(), &first_id);
        }
        assert_eq!(view.remote_action_test_counts(), (0, 0, 0));
        inject("Left");
        let row_owner = gtk::prelude::GtkWindowExt::focus(&win).expect("R07 row return");
        view.assert_remote_action_focus_owner(&row_owner, &first_id);
        inject("XF86Back");
        inject("Left");
        assert_eq!(
            c4_focused_button(&win).and_then(|b| b.label()).as_deref(),
            Some("☆")
        );
        inject("Return");
        wait_until(
            &|| {
                store
                    .query_channels(&ChannelQuery::default())
                    .is_ok_and(|page| page.items[0].is_favourite)
                    && view
                        .remote_action_test_row_star(&first_id)
                        .is_some_and(|star| star.label().as_deref() == Some("★"))
            },
            "star toggle, reload and persisted favourite",
        );
        assert!(
            view.remote_action_test_row_star(&first_id)
                .is_some_and(|star| star.has_focus()),
            "the toggled star must reclaim focus after SQLite reload"
        );
        // F02: move back into the exact real card, expand it using physical
        // Return and reach its (still mapped) Star through physical keyboard
        // focus traversal. Do not grab_focus() the star or use a model toggle.
        inject("Right");
        view.assert_remote_action_focus_owner(
            &gtk::prelude::GtkWindowExt::focus(&win).unwrap(),
            &first_id,
        );
        inject("Return");
        wait_until(
            &|| c4_focused_button(&win).is_some_and(|b| b.label().as_deref() == Some("Play")),
            "F02 expanded real card Play action",
        );
        let expanded_star = view
            .remote_action_test_row_star(&first_id)
            .expect("F02 same bound expanded row Star");
        let mut reached_star = false;
        for _ in 0..32 {
            inject("Tab");
            if expanded_star.has_focus() {
                reached_star = true;
                break;
            }
        }
        assert!(
            reached_star,
            "F02 physical Tab must reach expanded card Star"
        );
        inject("Return");
        wait_until(
            &|| {
                store
                    .query_channels(&ChannelQuery::default())
                    .is_ok_and(|page| !page.items[0].is_favourite)
                    && view
                        .remote_action_test_row_star(&first_id)
                        .is_some_and(|star| {
                            star.label().as_deref() == Some("☆") && star.has_focus()
                        })
            },
            "F02 expanded-row star OFF persists and focus returns after async reload",
        );
        assert_eq!(
            view.remote_action_test_selected_id().as_deref(),
            Some(first_id.as_str()),
            "F02 no cross-card focus or UUID after reload"
        );
        // Restore the established favourite-ON fixture using the same
        // physically focused Star; retain the F03/F04 footer/restart journey.
        inject("Return");
        wait_until(
            &|| {
                store
                    .query_channels(&ChannelQuery::default())
                    .is_ok_and(|page| page.items[0].is_favourite)
                    && view
                        .remote_action_test_row_star(&first_id)
                        .is_some_and(|star| {
                            star.label().as_deref() == Some("★") && star.has_focus()
                        })
            },
            "F02 inverse ON persists before favourite-filter regression",
        );
        let (favourites, show_all) = view.remote_action_test_favourite_buttons();
        if painted {
            // Enter the real footer via physical Tab after the remapped
            // favourite Star, rather than synthesizing its focus state.
            for _ in 0..40 {
                if favourites.has_focus() {
                    break;
                }
                inject("Tab");
            }
            assert!(
                favourites.has_focus(),
                "G04 native Tab must reach favourites footer"
            );
        } else {
            assert!(favourites.grab_focus());
        }
        let favourite_pixels = if painted {
            crate::window::r07_native_focus_tests::r07_painted_probe(
                &win,
                favourites.upcast_ref(),
                "G04 favourites footer",
            )
        } else {
            0
        };
        // The original diagnostic injector forcibly reselects the newest
        // X11 root child on Return. Screen capture tools can create other
        // root children; Space activates a button without that unrelated
        // XSetInputFocus side effect, while remaining genuine XTest input.
        inject(if painted { "space" } else { "Return" });
        wait_until(
            &|| store.favourites_only() == Ok(true) && view.remote_action_test_visible_count() == 1,
            "favourites-only toggle and SQL filter",
        );
        // F04: a new OS process (not just another Rust service handle)
        // must see the true saved favourites-only mode on the same private DB.
        // The reusable Arch toolchain may invoke this test through its
        // dynamic loader rather than a native host ELF interpreter. In that
        // bounded local-only mode, launch the *same test binary* explicitly;
        // normal CI still runs std::env::current_exe() unchanged.
        let mut process = if let Ok(loader) = std::env::var("R07_FOCUS_LOCAL_LOADER") {
            let libs = std::env::var("R07_FOCUS_LOCAL_LIBS")
                .expect("local loader needs explicit recovered library paths");
            let binary = std::env::var("R07_FOCUS_LOCAL_BINARY")
                .expect("local loader must name the current test binary");
            let mut command = std::process::Command::new(loader);
            command.args(["--library-path", &libs, &binary]);
            command
        } else {
            std::process::Command::new(std::env::current_exe().unwrap())
        };
        let output = process
            .args([
                "r07_native_favourites_restart_child",
                "--ignored",
                "--test-threads=1",
            ])
            .env("R07_C4_PROCESS_CHILD", "1")
            .output()
            .expect("spawn real independent test process");
        assert!(
            output.status.success(),
            "R07 F04 process restart: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "R07 F04 child process must execute its SQLite restart oracle"
        );
        let reopened = glib::MainContext::default()
            .block_on(StreamsService::with_storage(store.clone()))
            .expect("reopen disposable SQLite");
        assert_eq!(store.favourites_only(), Ok(true));
        view.install_remote_action_test_service(reopened, true);
        wait_until(
            &|| {
                view.remote_action_test_visible_count() == 1
                    && view.remote_action_test_row_star(&first_id).is_some()
            },
            "favourite visible after database reopen",
        );
        view.remote_action_test_select(0);
        spin();
        inject("Left");
        assert_eq!(
            c4_focused_button(&win).and_then(|b| b.label()).as_deref(),
            Some("★")
        );
        inject(if painted { "space" } else { "Return" });
        let filtered = ChannelQuery {
            favourites_only: true,
            ..ChannelQuery::default()
        };
        wait_until(
            &|| {
                store
                    .query_channels(&filtered)
                    .is_ok_and(|page| page.total == 0)
                    && view.remote_action_test_visible_count() == 0
                    && show_all.has_focus()
            },
            "last favourite removal restores Show All focus",
        );
        assert!(show_all.is_mapped() && show_all.has_focus());
        if painted {
            // Real async filter/rebind animation can outlive the SQL count
            // change. Give the exact mapped focus owner time to paint a
            // stable frame before comparing suppression and restoration.
            for _ in 0..6 {
                spin();
                assert!(show_all.is_mapped() && show_all.has_focus());
            }
        }
        let fallback_pixels = if painted {
            crate::window::r07_native_focus_tests::r07_painted_probe(
                &win,
                show_all.upcast_ref(),
                "G04 last-favourite Show All fallback",
            )
        } else {
            0
        };
        if painted {
            eprintln!("R07_G04_PAINTED {width}x{height} footer={favourite_pixels} fallback={fallback_pixels}");
            if negative {
                assert_eq!(
                    favourite_pixels, 0,
                    "R07_G04_EXPECTED_NEGATIVE_PAINT: focused favourites footer has pixels"
                );
                unreachable!("invalid G04 visual oracle was accepted");
            }
            assert!(
                favourite_pixels >= 15,
                "R07_G04_MISSING_PAINTED_FOOTER: real favourites action had no reversible outline"
            );
            assert!(fallback_pixels >= 15,
                "R07_G04_MISSING_PAINTED_FALLBACK: real Show All async return had no reversible outline");
        }
        inject(if painted { "space" } else { "Return" });
        wait_until(
            &|| {
                store.favourites_only() == Ok(false) && view.remote_action_test_visible_count() == 3
            },
            "Show All exits empty favourites",
        );
        assert_eq!(pages.visible_child_name().as_deref(), Some("streams"));
        win.destroy();
    }

    #[test]
    #[ignore = "R07 G04 real X11 favourites footer/fallback painted focus compact"]
    fn r07_native_favourites_paint_640_480() {
        c4_native_favourites(640, 480, true, false);
    }

    #[test]
    #[ignore = "R07 G04 real X11 favourites footer/fallback painted focus TV"]
    fn r07_native_favourites_paint_1280_800() {
        c4_native_favourites(1280, 800, true, false);
    }

    #[test]
    #[ignore = "R07 G04 deliberately wrong visible footer focus oracle"]
    fn r07_native_favourites_paint_negative() {
        c4_native_favourites(640, 480, true, true);
    }

    #[test]
    #[ignore = "child-process persisted SQLite favourite mode oracle"]
    fn r07_native_favourites_restart_child() {
        assert_eq!(std::env::var("R07_C4_PROCESS_CHILD").as_deref(), Ok("1"));
        let home = std::env::var("XDG_DATA_HOME").expect("private C4 data directory");
        assert!(home.contains("broadcaster-c4-fixture-"));
        let storage = crate::streams::storage::Storage::new();
        storage
            .initialize()
            .expect("reopened private DB after process restart");
        assert_eq!(
            storage.favourites_only(),
            Ok(true),
            "F04 persisted favourite mode"
        );
    }

    #[test]
    #[ignore = "real SQLite-backed mapped C4 favourite focus"]
    fn c4_native_favourites_640_480() {
        c4_native_favourites(640, 480, false, false);
    }

    #[test]
    #[ignore = "real SQLite-backed mapped C4 favourite focus"]
    fn c4_native_favourites_1280_800() {
        c4_native_favourites(1280, 800, false, false);
    }

    // Walk the live GTK widget tree, not test-fabricated action widgets.
    fn c4_picker_buttons(widget: &gtk::Widget) -> Vec<gtk::Button> {
        fn visit(widget: &gtk::Widget, found: &mut Vec<gtk::Button>) {
            if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
                found.push(button);
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                visit(&next, found);
                child = next.next_sibling();
            }
        }
        let mut buttons = Vec::new();
        visit(widget, &mut buttons);
        buttons
    }

    fn c4_picker_button(root: &gtk::Widget, name: &str) -> gtk::Button {
        c4_picker_buttons(root)
            .into_iter()
            .find(|button| button.label().as_deref() == Some(name))
            .unwrap_or_else(|| panic!("C4 modal button absent: {name}"))
    }

    fn c4_native_focus_within(win: &crate::window::TvWindow, ancestor: &gtk::Widget) -> bool {
        let mut focused = gtk::prelude::GtkWindowExt::focus(win);
        while let Some(widget) = focused {
            if widget == *ancestor {
                return true;
            }
            focused = widget.parent();
        }
        false
    }

    // A mapped widget can still be clipped by a ScrolledWindow or extend
    // outside the physical Xvfb window. Check its true window-relative GTK
    // allocation and every ancestor scroll viewport; used only after real
    // XTest directional keys have moved focus and scrolled the control in.
    fn c4_native_visible_in_viewport(win: &crate::window::TvWindow, control: &gtk::Widget) -> bool {
        if !control.is_mapped() || !control.is_visible() || !control.is_sensitive() {
            return false;
        }
        let Some(bounds) = control.compute_bounds(win.upcast_ref::<gtk::Widget>()) else {
            return false;
        };
        let (x, y, w, h) = (bounds.x(), bounds.y(), bounds.width(), bounds.height());
        if w < 8.0
            || h < 8.0
            || x < -1.0
            || y < -1.0
            || x + w > win.width() as f32 + 1.0
            || y + h > win.height() as f32 + 1.0
        {
            return false;
        }
        let mut ancestor = control.parent();
        while let Some(widget) = ancestor {
            if widget.is::<gtk::ScrolledWindow>() {
                let Some(viewport) = widget.compute_bounds(win.upcast_ref::<gtk::Widget>()) else {
                    return false;
                };
                if x < viewport.x() - 1.0
                    || y < viewport.y() - 1.0
                    || x + w > viewport.x() + viewport.width() + 1.0
                    || y + h > viewport.y() + viewport.height() + 1.0
                {
                    return false;
                }
            }
            ancestor = widget.parent();
        }
        true
    }

    fn c4_picker_entry(widget: &gtk::Widget) -> Option<gtk::Entry> {
        if let Ok(entry) = widget.clone().downcast::<gtk::Entry>() {
            return Some(entry);
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if let Some(entry) = c4_picker_entry(&next) {
                return Some(entry);
            }
            child = next.next_sibling();
        }
        None
    }

    fn c4_picker_modal(widget: &gtk::Widget, css: &str) -> Option<adw::Dialog> {
        if let Ok(dialog) = widget.clone().downcast::<adw::Dialog>() {
            if dialog.has_css_class(css) && dialog.is_mapped() {
                return Some(dialog);
            }
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if let Some(dialog) = c4_picker_modal(&next, css) {
                return Some(dialog);
            }
            child = next.next_sibling();
        }
        None
    }

    fn c4_picker_has_label(widget: &gtk::Widget, expected: &str) -> bool {
        if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
            if label.text().contains(expected) {
                return true;
            }
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            if c4_picker_has_label(&next, expected) {
                return true;
            }
            child = next.next_sibling();
        }
        false
    }

    /// Real selected-guide SQLite, async snapshot, GTK modal, XTest and
    /// transaction CAS. Never use an owner's playlist or non-disposable guide.
    fn c4_native_mapping_picker(
        width: i32,
        height: i32,
        painted: bool,
        negative: bool,
        guide_race: bool,
        wrong_race: bool,
        scenario: (bool, bool, bool, bool),
    ) {
        let (source_race, advanced_exception, false_exception, e08_actions) = scenario;
        use crate::streams::{
            editor,
            model::{
                ChannelQuery, IncomingChannel, OriginKind, SourceDraft, SourceOrigin,
                ValidatedSnapshot,
            },
            storage::Storage,
        };
        use rusqlite::params;
        use std::collections::BTreeSet;

        let data_home = std::env::var("XDG_DATA_HOME").expect("isolated XDG_DATA_HOME");
        assert!(data_home.contains("broadcaster-c4-fixture-"));
        let store = Storage::new();
        store.initialize().unwrap();
        let playlist_id = uuid::Uuid::new_v4().to_string();
        let entries: Vec<_> = [
            ("Zero match", "zero.original"),
            ("One match", "one.original"),
            ("Five match", "five.original"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (name, tvg))| IncomingChannel {
            order: index as u32,
            imported_name: name.into(),
            stream_url: format!("https://example.invalid/c4-picker/{index}.m3u8"),
            tvg_id: Some(tvg.into()),
            tvg_name: None,
            group_title: None,
            logo_url: None,
            channel_number: None,
            region: None,
            languages: BTreeSet::new(),
            countries: BTreeSet::new(),
            user_agent: None,
            referer: None,
        })
        .collect();
        store
            .create_source_with_initial_snapshot(
                &playlist_id,
                &SourceDraft {
                    name: "C4 picker playlist".into(),
                    origin: SourceOrigin {
                        kind: OriginKind::RemoteUrl,
                        locator: "https://example.invalid/c4-picker.m3u".into(),
                    },
                    enabled: true,
                    order: 0,
                    defaults: SourceDefaults::default(),
                },
                &ValidatedSnapshot::new(playlist_id.clone(), entries).unwrap(),
                1_800_000_000,
            )
            .unwrap();
        let ids: Vec<_> = store
            .query_channels(&ChannelQuery::default())
            .unwrap()
            .items
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids.len(), 3);
        let guide = uuid::Uuid::new_v4().to_string();
        let other_guide = uuid::Uuid::new_v4().to_string();
        {
            let mut db = store.connect().unwrap();
            for (id, name) in [
                (&guide, "C4 selected guide"),
                (&other_guide, "C4 other guide"),
            ] {
                db.execute(
                    "INSERT INTO epg_sources
                     (id,name,origin_kind,origin_locator,enabled,created_at,updated_at,last_success_at)
                     VALUES (?1,?2,'local_file','/fabricated/c4-guide',1,1,10,1000)",
                    params![id, name],
                )
                .unwrap();
            }
            crate::epg::bindings::set_shared_guide(
                &mut db,
                crate::epg::bindings::ConfirmedGuideSelection::after_user_confirmation(),
                Some(&guide),
            )
            .unwrap();
            for (id, name) in std::iter::once(("one.001".to_string(), "One match".to_string()))
                .chain((1..=5).map(|i| (format!("five.{i:03}"), "Five match".into())))
                .chain((0..36).map(|i| (format!("searchme.{i:03}"), format!("SearchMe {i:03}"))))
            {
                db.execute(
                    "INSERT INTO epg_channels(source_id,channel_id,display_name)
                     VALUES (?1,?2,?3)",
                    params![guide, id, name],
                )
                .unwrap();
            }
            db.execute(
                "INSERT INTO epg_channels(source_id,channel_id,display_name)
                 VALUES (?1,'wrong.guide','WrongGuide Only')",
                [&other_guide],
            )
            .unwrap();
        }
        let service = glib::MainContext::default()
            .block_on(StreamsService::with_storage(store.clone()))
            .unwrap();
        adw::init().expect("GTK display");
        gio::resources_register(
            &gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap(),
        );
        // Native Rust-test application bootstrap omits main.rs's real
        // stylesheet registration. Painted focus assertions must install the
        // shipped CSS exactly as production startup does, not a test shim.
        if painted {
            let css = gtk::CssProvider::new();
            css.load_from_resource("/io/github/Certain1989/Broadcaster/broadcaster.css");
            gtk::style_context_add_provider_for_display(
                &gtk::gdk::Display::default().expect("real X11 display"),
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        gio::Settings::new(crate::config::BASE_APP_ID)
            .set_boolean("osk-enabled", true)
            .unwrap();
        let app = TvApplication::new();
        app.register(None::<&gio::Cancellable>).unwrap();
        let win = crate::window::TvWindow::new(&app);
        win.set_default_size(width, height);
        win.present();
        spin();
        let view = find_view(win.upcast_ref()).expect("My Channels");
        // Switch the real window to the real view. The isolated service is
        // injected only because production startup is disabled in Rust tests.
        fn stack(widget: &gtk::Widget) -> Option<adw::ViewStack> {
            if let Ok(stack) = widget.clone().downcast::<adw::ViewStack>() {
                return Some(stack);
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                if let Some(stack) = stack(&next) {
                    return Some(stack);
                }
                child = next.next_sibling();
            }
            None
        }
        stack(win.upcast_ref())
            .expect("navigation stack")
            .set_visible_child_name("streams");
        view.install_remote_action_test_service(service, false);

        let wait = |predicate: &dyn Fn() -> bool, reason: &str| {
            for _ in 0..35 {
                spin();
                if predicate() {
                    return;
                }
            }
            panic!("C4 mapping timeout: {reason}");
        };
        wait(
            &|| {
                view.remote_action_test_visible_count() == 3
                    && view.remote_action_test_row_star(&ids[0]).is_some()
            },
            "mapped SQLite rows",
        );

        // B01: only the initial Search field is focus-seeded. Traverse the
        // real toolbar, filter row, and virtualised SQLite list with physical
        // XTest Down before opening the *actual* picker; do not call
        // remote_action_test_select() for any of these three card entries.
        let search = view.toolbar_focus_targets()[0].clone();
        assert!(search.grab_focus(), "B01 mapped Search entry");
        spin();
        for _ in 0..16 {
            let owner =
                gtk::prelude::GtkWindowExt::focus(&win).expect("B01 Search-to-results focus owner");
            if view.contains_results_focus(&owner) {
                break;
            }
            inject("Down");
        }
        let owner = gtk::prelude::GtkWindowExt::focus(&win)
            .expect("B01 focus after actual toolbar traversal");
        assert!(
            owner.is_mapped() && view.contains_results_focus(&owner),
            "B01 physical Search→filters→SQLite result route"
        );
        assert_eq!(
            view.remote_action_test_selected_id().as_deref(),
            Some(ids[0].as_str()),
            "B01 first real row after Search traversal"
        );
        for (index, expected_proposals) in [(0_u32, 0), (1, 1), (2, 5)] {
            let id = &ids[index as usize];
            if index != 0 {
                // The previous picker closed to its expanded Mapping action;
                // return/collapse/traverse the next row without helper seeding.
                inject("Left");
                view.assert_remote_action_focus_owner(
                    &gtk::prelude::GtkWindowExt::focus(&win).unwrap(),
                    &ids[index as usize - 1],
                );
                inject("XF86Back");
                inject("Down");
            }
            assert_eq!(
                view.remote_action_test_selected_id().as_deref(),
                Some(id.as_str()),
                "B01 row traversal must preserve source-qualified ID"
            );
            inject("Right");
            let play = c4_focused_button(&win).expect("Play entry");
            assert_eq!(play.label().as_deref(), Some("Play"));
            view.assert_remote_action_focus_owner(play.upcast_ref(), id);
            if e08_actions {
                // E08: activate the actual first/middle/last source-qualified
                // SQLite cards, not merely inspect the labels. The app's
                // test-only player absorbs Play without mpv or network access.
                let before = view.remote_action_test_counts();
                inject("Return");
                assert_eq!(
                    view.remote_action_test_counts(),
                    (before.0 + 1, before.1, before.2),
                    "E08 explicit Play only invokes fake player, never editor/picker"
                );
                view.assert_remote_action_focus_owner(
                    &gtk::prelude::GtkWindowExt::focus(&win).expect("E08 Play stays focused"),
                    id,
                );
                inject("Down");
                assert_eq!(
                    c4_focused_button(&win).and_then(|b| b.label()).as_deref(),
                    Some("Edit Metadata"),
                    "E08 physical Play→Edit"
                );
                inject("Return");
                assert_eq!(
                    view.remote_action_test_counts().1,
                    before.1 + 1,
                    "E08 real Edit callback invoked once"
                );
                fn edit_dialog(root: &gtk::Widget) -> Option<adw::Dialog> {
                    if let Ok(dialog) = root.clone().downcast::<adw::Dialog>() {
                        if dialog.is_mapped() && dialog.title().as_str() == "Edit Channel Metadata"
                        {
                            return Some(dialog);
                        }
                    }
                    let mut child = root.first_child();
                    while let Some(next) = child {
                        if let Some(dialog) = edit_dialog(&next) {
                            return Some(dialog);
                        }
                        child = next.next_sibling();
                    }
                    None
                }
                wait(
                    &|| edit_dialog(win.upcast_ref()).is_some(),
                    "E08 real SQLite row editor dialog",
                );
                let dialog = edit_dialog(win.upcast_ref()).unwrap();
                assert!(dialog.is_mapped(), "E08 editor must really be on screen");
                if index == 0 {
                    // R1: use the X11 remote to scroll to the real footer,
                    // then traverse its three actions without Tab, pointer
                    // or test-side focus injection. A mapped button whose
                    // rectangle is outside any scroller is not accepted.
                    let footer = ["Cancel", "Reset to Automatic", "Save"];
                    let mut first_footer = None;
                    for _ in 0..70 {
                        if let Some(button) = c4_focused_button(&win) {
                            if let Some(pos) = footer
                                .iter()
                                .position(|s| button.label().as_deref() == Some(*s))
                            {
                                first_footer = Some(pos);
                                break;
                            }
                        }
                        inject("Down");
                    }
                    let pos =
                        first_footer.expect("R1 physical Down must reach compact metadata footer");
                    let button = c4_picker_button(dialog.upcast_ref(), footer[pos]);
                    assert!(c4_native_visible_in_viewport(&win, button.upcast_ref()),
                        "R1 metadata footer control must be entirely inside real viewport after Down: {}", footer[pos]);
                    // The footer is a horizontal GTK action row; real Left
                    // and Right expose all three choices after scrolling.
                    for _ in 0..pos {
                        inject("Left");
                    }
                    for (idx, name) in footer.iter().enumerate() {
                        let button = c4_picker_button(dialog.upcast_ref(), name);
                        assert!(
                            button.has_focus(),
                            "R1 physical metadata footer action: {name}"
                        );
                        assert!(
                            c4_native_visible_in_viewport(&win, button.upcast_ref()),
                            "R1 clipped compact metadata button: {name}"
                        );
                        if idx + 1 < footer.len() {
                            inject("Right");
                        }
                    }
                    if wrong_race {
                        assert!(!c4_native_visible_in_viewport(&win,
                            c4_picker_button(dialog.upcast_ref(), "Save").upcast_ref()),
                            "R07_R1_EXPECTED_NEGATIVE_VISIBILITY: visible Save must not appear clipped");
                    }
                }
                // On the compact first-row route, complete the physical
                // scroll/geometry proof with an actual footer activation.
                // Traverse back from Save through Reset to Cancel, then
                // activate it using Return (never click, Tab or grab_focus).
                if index == 0 && !wrong_race {
                    inject("Left");
                    inject("Left");
                    let cancel = c4_picker_button(dialog.upcast_ref(), "Cancel");
                    assert!(cancel.has_focus(), "R1 physical Save→Reset→Cancel inverse");
                    assert!(
                        c4_native_visible_in_viewport(&win, cancel.upcast_ref()),
                        "R1 Cancel must be visible before D-pad activation"
                    );
                    inject("Return");
                } else {
                    inject("Escape");
                }
                wait(
                    &|| !dialog.is_mapped(),
                    "E08 physical editor Cancel/ Escape dismisses modal",
                );
                assert_eq!(
                    view.remote_action_test_selected_id().as_deref(),
                    Some(id.as_str())
                );
                let mutations: i64 = store
                    .connect()
                    .unwrap()
                    .query_row(
                        "SELECT COUNT(*) FROM iptv_channel_overrides WHERE channel_id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(mutations, 0, "E08 cancelled editor cannot mutate this row");
                if negative && index == 1 {
                    assert_eq!(view.remote_action_test_selected_id().as_deref(),
                        Some(ids[0].as_str()),
                        "R07_E08_EXPECTED_NEGATIVE_ORACLE: middle card must not return to first UUID");
                }
                // Detect actual focus restoration; never mask a broken
                // modal return by programmatically focusing the editor.
                let edit = view
                    .remote_action_test_buttons()
                    .expect("E08 original card still bound")[1]
                    .clone();
                assert!(edit.is_mapped() && edit.is_sensitive());
                assert!(
                    edit.has_focus(),
                    "E08 real Edit cancellation must restore focus without test-only grab_focus"
                );
                view.assert_remote_action_focus_owner(edit.upcast_ref(), id);
                inject("Down");
            } else {
                inject("Down");
                inject("Down");
            }
            assert_eq!(
                c4_focused_button(&win).and_then(|b| b.label()).as_deref(),
                Some("Programme mapping")
            );
            let previous = view.remote_action_test_counts().2;
            inject("Return");
            wait(
                &|| editor::c4_test_picker_dialog().is_some_and(|d| d.is_mapped()),
                "real mapping dialog",
            );
            assert_eq!(view.remote_action_test_counts().2, previous + 1);
            let dialog = editor::c4_test_picker_dialog().unwrap();
            let root = dialog.upcast_ref::<gtk::Widget>();
            let proposals = c4_picker_buttons(root)
                .into_iter()
                .filter(|b| {
                    b.label().is_some_and(|label| {
                        label.contains("review variant") || label.contains("exact identity")
                    })
                })
                .count();
            assert_eq!(proposals, expected_proposals, "selected-guide proposals");
            assert!(
                !c4_picker_buttons(root)
                    .into_iter()
                    .any(|b| b.label().is_some_and(|label| label.contains("WrongGuide"))),
                "wrong guide leaked into picker"
            );
            if index == 0 {
                // B02: zero suggestions must neither invent an exact mapping
                // nor silently save one before the user opens advanced search.
                assert!(c4_picker_buttons(root).iter().all(|b| {
                    !b.label().is_some_and(|l| {
                        l.contains("exact identity") || l.contains("review variant")
                    })
                }));
                let count: i64 = store
                    .connect()
                    .unwrap()
                    .query_row(
                        "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 0, "B02 no automatic mapping for zero candidates");
            }
            if painted {
                // G06: real picker, then the real nested K2 OSK, after
                // Search→filters→bound card→Mapping on physical XTest input.
                // Compare positive, suppressed and restored X11 pixels, not
                // a fabricated CSS class or GtkWidget::has_focus alone.
                let search = c4_picker_button(root, "Search all guide channels / choose exact ID…");
                assert!(search.grab_focus(), "G06 mapped picker search control");
                let picker_pixels = crate::window::r07_native_focus_tests::r07_painted_probe(
                    &win,
                    search.upcast_ref(),
                    "G06 real mapping picker search",
                );
                inject("Return");
                wait(
                    &|| c4_picker_entry(root).is_some(),
                    "G06 real exact-guide search field",
                );
                let keyboard = c4_picker_buttons(root)
                    .into_iter()
                    .find(|b| {
                        b.icon_name().as_deref() == Some("input-keyboard-symbolic") && b.is_mapped()
                    })
                    .expect("G06 real picker OSK launcher");
                assert!(keyboard.grab_focus());
                inject("Return");
                wait(
                    &|| c4_picker_modal(win.upcast_ref(), "osk-production").is_some(),
                    "G06 real nested OSK",
                );
                let osk = c4_picker_modal(win.upcast_ref(), "osk-production").unwrap();
                let left = c4_picker_button(osk.upcast_ref(), "←");
                assert!(left.grab_focus(), "G06 physical OSK caret cell focusable");
                let osk_pixels = crate::window::r07_native_focus_tests::r07_painted_probe(
                    &win,
                    left.upcast_ref(),
                    "G06 nested OSK caret",
                );
                if negative {
                    assert_eq!(
                        picker_pixels + osk_pixels,
                        0,
                        "R07_G06_EXPECTED_NEGATIVE_PAINT: native picker and OSK must paint focus"
                    );
                }
                assert!(picker_pixels >= 15 && osk_pixels >= 15,
                    "R07_G06_MISSING_PAINTED_PICKER_OSK: both actual controls must paint reversible focus pixels");
                let cancel = c4_picker_button(osk.upcast_ref(), "Cancel");
                assert!(cancel.grab_focus());
                inject("Return");
                wait(
                    &|| !osk.is_mapped(),
                    "G06 OSK cancel closes only nested dialog",
                );
                let close = c4_picker_button(root, "Cancel");
                assert!(close.grab_focus());
                inject("Return");
                wait(
                    &|| editor::c4_test_picker_dialog().is_none(),
                    "G06 picker cancel",
                );
                win.destroy();
                return;
            }
            if index == 1 {
                // B03: physical Return on the only scoped suggestion must
                // reach the real SQLite CAS writer, close the live modal and
                // return to the exact source-qualified card, not a test stub.
                let exact = c4_picker_buttons(root)
                    .into_iter()
                    .find(|b| {
                        b.label().is_some_and(|label| {
                            label.contains("review variant") || label.contains("exact identity")
                        })
                    })
                    .expect("B03 exactly one mapped action");
                assert!(exact.grab_focus());
                inject("Return");
                wait(
                    &|| editor::c4_test_picker_dialog().is_none(),
                    "B03 confirmed mapping closes picker",
                );
                let (saved_guide, saved_target): (String, String) = store.connect().unwrap()
                    .query_row(
                        "SELECT guide_source_id,guide_channel_id FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                        [id], |r| Ok((r.get(0)?, r.get(1)?)),
                    ).expect("B03 exact row-level mapping persisted");
                assert_eq!(
                    (saved_guide, saved_target),
                    (guide.clone(), "one.001".to_string())
                );
                assert_eq!(
                    c4_focused_button(&win).and_then(|b| b.label()).as_deref(),
                    Some("Programme mapping"),
                    "B03 focus returned to mapped card action",
                );
                view.assert_remote_action_focus_owner(
                    &gtk::prelude::GtkWindowExt::focus(&win).unwrap(),
                    id,
                );
                continue;
            }

            if index == 2 {
                let search = c4_picker_button(root, "Search all guide channels / choose exact ID…");
                assert!(search.grab_focus());
                inject("Return");
                spin();
                let entry = c4_picker_entry(root).expect("real search field");
                assert!(entry.is_mapped());
                entry.set_text("SearchMe");
                wait(
                    &|| {
                        c4_picker_buttons(root)
                            .iter()
                            .filter(|b| {
                                b.label()
                                    .is_some_and(|l| l.contains("SearchMe") && l.contains("— ["))
                            })
                            .count()
                            == 30
                    },
                    "first 30 selected-guide exact results",
                );
                let next = c4_picker_button(root, "Next");
                assert!(next.is_sensitive() && next.grab_focus());
                inject("Return");
                wait(
                    &|| {
                        c4_picker_buttons(root)
                            .iter()
                            .filter(|b| {
                                b.label()
                                    .is_some_and(|l| l.contains("SearchMe") && l.contains("— ["))
                            })
                            .count()
                            == 6
                    },
                    "second page with six exact results",
                );
                // Real OSK launcher, physical arrows on its live preview and
                // physical Cancel. Never commit a fabricated search result.
                let keyboard = c4_picker_buttons(root)
                    .into_iter()
                    .find(|b| {
                        b.icon_name().as_deref() == Some("input-keyboard-symbolic") && b.is_mapped()
                    })
                    .expect("mapped picker keyboard launcher");
                assert!(keyboard.grab_focus());
                inject("Return");
                wait(
                    &|| c4_picker_modal(win.upcast_ref(), "osk-production").is_some(),
                    "mapped production OSK",
                );
                let osk = c4_picker_modal(win.upcast_ref(), "osk-production").unwrap();
                let preview = c4_picker_entry(osk.upcast_ref()).expect("OSK draft entry");
                preview.set_text("ab");
                preview.set_position(1);
                let left = c4_picker_button(osk.upcast_ref(), "←");
                assert!(left.grab_focus());
                inject("Return");
                assert_eq!(preview.position(), 0, "physical OSK caret left");
                let right = c4_picker_button(osk.upcast_ref(), "→");
                assert!(right.grab_focus());
                inject("Return");
                assert_eq!(preview.position(), 1, "physical OSK caret right");
                let osk_cancel = c4_picker_button(osk.upcast_ref(), "Cancel");
                assert!(osk_cancel.grab_focus());
                inject("Return");
                wait(&|| !osk.is_mapped(), "OSK Cancel dismissed nested dialog");
                assert_eq!(
                    entry.text().as_str(),
                    "SearchMe",
                    "cancel cannot edit origin"
                );

                // B11: the nested physical caret/Cancel journey above occurred
                // on the actual second (30+6) exact-guide search page, not
                // merely after returning to the first page. Now physically
                // reverse pagination before the stale-CAS negative route.
                let previous = c4_picker_button(root, "Previous");
                assert!(previous.is_sensitive() && previous.grab_focus());
                inject("Return");
                wait(
                    &|| {
                        c4_picker_buttons(root)
                            .iter()
                            .filter(|b| {
                                b.label()
                                    .is_some_and(|l| l.contains("SearchMe") && l.contains("— ["))
                            })
                            .count()
                            == 30
                    },
                    "back to first search page",
                );

                // B14: mutate the real guide catalogue transactionally while
                // preserving its successful-import second and target identity.
                // A changed timestamp by itself would not prove a same-second
                // reimport stale-picker conflict.
                {
                    let db = store.connect().unwrap();
                    let before: (i64, i64) = db
                        .query_row(
                            "SELECT updated_at,last_success_at FROM epg_sources WHERE id=?1",
                            [&guide],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .unwrap();
                    assert_eq!(before, (10, 1000));
                    db.execute_batch("BEGIN IMMEDIATE").unwrap();
                    db.execute(
                        "UPDATE epg_channels SET display_name='SearchMe 000 reimported'
                         WHERE source_id=?1 AND channel_id='searchme.000'",
                        [&guide],
                    )
                    .unwrap();
                    db.execute(
                        "INSERT INTO epg_programmes(source_id,channel_id,start_utc,stop_utc,title)
                         VALUES (?1,'searchme.000',10,20,'Reimported schedule')",
                        [&guide],
                    )
                    .unwrap();
                    db.execute("UPDATE epg_sources SET updated_at=11 WHERE id=?1", [&guide])
                        .unwrap();
                    db.execute_batch("COMMIT").unwrap();
                    let after: (i64, i64) = db
                        .query_row(
                            "SELECT updated_at,last_success_at FROM epg_sources WHERE id=?1",
                            [&guide],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .unwrap();
                    assert_eq!(
                        after,
                        (11, before.1),
                        "same import second, new catalogue revision"
                    );
                    let target_count: i64 = db
                        .query_row(
                            "SELECT COUNT(*) FROM epg_channels WHERE source_id=?1
                             AND channel_id='searchme.000'",
                            [&guide],
                            |row| row.get(0),
                        )
                        .unwrap();
                    assert_eq!(
                        target_count, 1,
                        "reimport retained selected guide target ID"
                    );
                }
                // The physical picker still offers its loaded old target,
                // but the transactional CAS must reject the mapping write.
                let target = c4_picker_buttons(root)
                    .into_iter()
                    .find(|b| {
                        b.label()
                            .is_some_and(|l| l.contains("SearchMe") && l.contains("— ["))
                    })
                    .expect("real exact-result action");
                assert!(target.grab_focus());
                inject("Return");
                wait(
                    &|| {
                        // This is the user-visible CAS denial from the real
                        // picker callback, not a backend-only error oracle.
                        let mut labels = Vec::new();
                        fn collect(widget: &gtk::Widget, out: &mut Vec<String>) {
                            if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
                                out.push(label.text().to_string());
                            }
                            let mut child = widget.first_child();
                            while let Some(next) = child {
                                collect(&next, out);
                                child = next.next_sibling();
                            }
                        }
                        collect(root, &mut labels);
                        labels
                            .iter()
                            .any(|l| l.contains("Channel, guide or binding changed"))
                    },
                    "same-second stale guide refuses UI mapping write",
                );
                let count: i64 = store
                    .connect()
                    .unwrap()
                    .query_row(
                        "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 0, "wrong programme cannot bind on stale guide");
            }
            let cancel = c4_picker_button(root, "Cancel");
            assert!(cancel.grab_focus());
            inject("Return");
            wait(
                &|| editor::c4_test_picker_dialog().is_none(),
                "real picker Cancel close",
            );
            assert_eq!(
                c4_focused_button(&win).and_then(|b| b.label()).as_deref(),
                Some("Programme mapping"),
                "mapping close must restore its exact source-qualified card"
            );
            view.assert_remote_action_focus_owner(
                &gtk::prelude::GtkWindowExt::focus(&win).unwrap(),
                id,
            );
        }

        // B04: reopen the *same* third card after the stale-CAS rejection.
        // This freshly loaded picker sees the updated guide revision. Walk
        // all five bounded suggestions through genuine Down/Up keys, then
        // choose the fourth via physical Return, verifying only that UUID's
        // guide mapping persists and the modal restores its card focus.
        assert_eq!(
            view.remote_action_test_selected_id().as_deref(),
            Some(ids[2].as_str()),
            "B04 still on five-suggestion card"
        );
        inject("Return");
        wait(
            &|| editor::c4_test_picker_dialog().is_some_and(|d| d.is_mapped()),
            "B04 fresh five-suggestion picker",
        );
        let five_dialog = editor::c4_test_picker_dialog().unwrap();
        let five_root = five_dialog.upcast_ref::<gtk::Widget>();
        let candidates: Vec<_> = c4_picker_buttons(five_root)
            .into_iter()
            .filter(|b| {
                b.label()
                    .is_some_and(|l| l.contains("review variant") || l.contains("exact identity"))
            })
            .collect();
        assert_eq!(candidates.len(), 5, "B04 bounded five exact candidates");
        assert!(candidates[0].grab_focus());
        for (i, expected) in candidates.iter().enumerate().skip(1) {
            inject("Down");
            assert!(
                expected.has_focus(),
                "B04 physical Down must reach suggestion {i} without escaping modal"
            );
        }
        // Up is the inverse of the four candidate-to-candidate Down hops.
        // A further Down from the last candidate belongs to the following
        // Search action, not to this bounded five-suggestion graph.
        for (i, expected) in candidates.iter().enumerate().rev().skip(1) {
            inject("Up");
            assert!(
                expected.has_focus(),
                "B04 physical Up must reverse to suggestion {i}"
            );
        }
        for _ in 0..3 {
            inject("Down");
        }
        assert!(
            candidates[3].has_focus(),
            "B04 fourth suggestion navigation"
        );
        inject("Return");
        wait(
            &|| editor::c4_test_picker_dialog().is_none(),
            "B04 physical fourth-suggestion mapping closes picker",
        );
        let saved: (String, String) = store.connect().unwrap().query_row(
            "SELECT guide_source_id,guide_channel_id FROM epg_channel_overrides WHERE iptv_channel_id=?1",
            [&ids[2]], |r| Ok((r.get(0)?, r.get(1)?))
        ).expect("B04 selected source's guide mapping persisted");
        assert_eq!(
            saved,
            (guide.clone(), "five.004".to_string()),
            "B04 fourth candidate binds no other source or channel"
        );
        let one_binding: String = store
            .connect()
            .unwrap()
            .query_row(
                "SELECT guide_channel_id FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                [&ids[1]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(one_binding, "one.001", "B04 cannot rewrite earlier card");
        assert_eq!(
            c4_focused_button(&win).and_then(|b| b.label()).as_deref(),
            Some("Programme mapping"),
            "B04 focus returns to same card"
        );
        view.assert_remote_action_focus_owner(
            &gtk::prelude::GtkWindowExt::focus(&win).unwrap(),
            &ids[2],
        );

        // B05: three real UI reopenings, with precedence determined by real
        // SQLite row > playlist > shared selections. Changing selection must
        // never silently rewrite any channel's manual guide override.
        use crate::epg::bindings::{
            clear_row_mapping, selected_guide_for_row, set_playlist_exception,
            ConfirmedGuideSelection, ConfirmedRowMapping,
        };
        {
            let mut db = store.connect().unwrap();
            set_playlist_exception(
                &mut db,
                ConfirmedGuideSelection::after_user_confirmation(),
                &playlist_id,
                Some(&other_guide),
                1234,
            )
            .unwrap();
            assert_eq!(
                selected_guide_for_row(&db, &ids[2]).unwrap().as_deref(),
                Some(guide.as_str()),
                "B05 per-row guide wins over playlist exception",
            );
        }
        for (tier, selected_name) in [
            ("row override", "C4 selected guide"),
            ("playlist exception", "C4 other guide"),
            ("shared default", "C4 selected guide"),
        ] {
            if tier == "playlist exception" {
                let mut db = store.connect().unwrap();
                clear_row_mapping(
                    &mut db,
                    ConfirmedRowMapping::after_user_confirmation_for_this_stream(),
                    &ids[2],
                )
                .unwrap();
                assert_eq!(
                    selected_guide_for_row(&db, &ids[2]).unwrap().as_deref(),
                    Some(other_guide.as_str()),
                    "B05 playlist exception wins over shared",
                );
            } else if tier == "shared default" {
                let mut db = store.connect().unwrap();
                set_playlist_exception(
                    &mut db,
                    ConfirmedGuideSelection::after_user_confirmation(),
                    &playlist_id,
                    None,
                    1235,
                )
                .unwrap();
                assert_eq!(
                    selected_guide_for_row(&db, &ids[2]).unwrap().as_deref(),
                    Some(guide.as_str()),
                    "B05 shared guide restored after clearing playlist exception",
                );
            }
            // Existing Mapping action is the naturally restored focus owner.
            assert_eq!(
                c4_focused_button(&win)
                    .and_then(|button| button.label())
                    .as_deref(),
                Some("Programme mapping"),
                "B05 same source card before opening real picker",
            );
            inject("Return");
            wait(
                &|| editor::c4_test_picker_dialog().is_some_and(|dialog| dialog.is_mapped()),
                "B05 real guide-precedence modal",
            );
            let picker = editor::c4_test_picker_dialog().unwrap();
            // The card action intentionally opens the compact D35 picker,
            // which displays its effective guide as a read-only label.
            // A DropDown exists in the separate full mapping editor only.
            assert!(
                c4_picker_has_label(
                    picker.upcast_ref(),
                    &format!("Selected programme guide: {selected_name}"),
                ),
                "B05 actual compact picker guide label for {tier}",
            );
            let cancel = c4_picker_button(picker.upcast_ref(), "Cancel");
            assert!(cancel.grab_focus());
            inject("Return");
            wait(&|| editor::c4_test_picker_dialog().is_none(), "B05 Cancel");
            view.assert_remote_action_focus_owner(
                &gtk::prelude::GtkWindowExt::focus(&win).unwrap(),
                &ids[2],
            );
            let db = store.connect().unwrap();
            let row_count: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                    [&ids[2]],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                row_count,
                i64::from(tier == "row override"),
                "B05 opening and cancelling picker cannot write an override",
            );
            let retained: String = db
                .query_row(
                    "SELECT guide_channel_id FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                    [&ids[1]],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(retained, "one.001", "B05 unrelated mapping retained");
        }

        // B06 diagnostic: a disabled guide selected as playlist exception
        // remains identifiable in the real picker, but must surface an
        // explicit disabled warning rather than presenting it as healthy.
        // This is evidence only: do not change the disabled-guide policy.
        {
            let mut db = store.connect().unwrap();
            set_playlist_exception(
                &mut db,
                ConfirmedGuideSelection::after_user_confirmation(),
                &playlist_id,
                Some(&other_guide),
                1236,
            )
            .unwrap();
            db.execute(
                "UPDATE epg_sources SET enabled=0 WHERE id=?1",
                [&other_guide],
            )
            .unwrap();
        }
        // The real compact picker must refuse disabled / non-imported
        // guides before presenting a modal; it reports a toast instead.
        // Preserve the original source's real focused Mapping action.
        inject("Return");
        let mut saw_warning = false;
        for _ in 0..16 {
            spin();
            assert!(
                editor::c4_test_picker_dialog().is_none(),
                "B06 disabled guide must not open stale picker",
            );
            saw_warning |= c4_picker_has_label(win.upcast_ref(), "Selected guide is disabled");
        }
        assert!(
            saw_warning,
            "B06 disabled-guide warning toast was never painted"
        );
        view.assert_remote_action_focus_owner(
            &gtk::prelude::GtkWindowExt::focus(&win).unwrap(),
            &ids[2],
        );
        // B06/AG08 cross-component journey: real My Channels toolbar
        // launches Sources, then its Programme Guides child; disabled XMLTV
        // source remains listed rather than being mistaken for healthy.
        // Closing the nested modal must restore the exact manager launcher.
        let manage = view.c1_test_manage_button();
        assert!(manage.grab_focus(), "B06 mapped source manager launcher");
        inject("Return");
        wait(
            &|| c4_picker_modal(win.upcast_ref(), "broadcaster-source-manager").is_some(),
            "B06 source manager opened from My Channels toolbar",
        );
        let manager = c4_picker_modal(win.upcast_ref(), "broadcaster-source-manager").unwrap();
        let guides = c4_picker_button(manager.upcast_ref(), "Programme Guides");
        assert!(guides.grab_focus(), "B06 real nested guide launcher");
        inject("Return");
        wait(
            &|| c4_picker_modal(win.upcast_ref(), "broadcaster-guide-manager").is_some(),
            "B06 manager-to-guide modal",
        );
        let guide_manager = c4_picker_modal(win.upcast_ref(), "broadcaster-guide-manager").unwrap();
        assert!(
            c4_picker_has_label(guide_manager.upcast_ref(), "C4 other guide"),
            "B06 disabled guide must remain identifiable in manager",
        );
        let guide_close = c4_picker_button(guide_manager.upcast_ref(), "Close");
        assert!(guide_close.grab_focus());
        inject("XF86Back");
        wait(
            &|| !guide_manager.is_mapped(),
            "B06 close only child guide manager",
        );
        assert!(manager.is_mapped());
        wait(
            &|| guides.has_focus(),
            "B06 nested focus returns to launcher",
        );
        let source_close = c4_picker_button(manager.upcast_ref(), "Close");
        assert!(source_close.grab_focus());
        inject("XF86Back");
        wait(&|| !manager.is_mapped(), "B06 close source manager");
        assert!(manage.is_mapped(), "B06 parent tab remains available");
        {
            let mut db = store.connect().unwrap();
            let count: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                    [&ids[2]],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                count, 0,
                "B06 disabled warning cannot silently write mapping"
            );
            db.execute(
                "UPDATE epg_sources SET enabled=1 WHERE id=?1",
                [&other_guide],
            )
            .unwrap();
            set_playlist_exception(
                &mut db,
                ConfirmedGuideSelection::after_user_confirmation(),
                &playlist_id,
                None,
                1237,
            )
            .unwrap();
        }

        if advanced_exception {
            // B06: Only the approved full mapping editor may change a
            // playlist-level guide. Enter it from the REAL compact card
            // picker and make a physical confirmed selection; no test-only
            // direct binding call is allowed along this journey.
            view.remote_action_test_select(2);
            spin();
            inject("Right");
            inject("Down");
            inject("Down");
            assert_eq!(
                c4_focused_button(&win).and_then(|b| b.label()).as_deref(),
                Some("Programme mapping"),
                "B06 mapped card action before advanced guide choice",
            );
            inject("Return");
            wait(
                &|| editor::c4_test_picker_dialog().is_some_and(|d| d.is_mapped()),
                "B06 actual compact picker",
            );
            let compact = editor::c4_test_picker_dialog().unwrap();
            let advanced =
                c4_picker_button(compact.upcast_ref(), "Change guide / playlist exception…");
            let search = c4_picker_button(
                compact.upcast_ref(),
                "Search all guide channels / choose exact ID…",
            );
            assert!(
                search.grab_focus(),
                "B06 preceding real search action focus"
            );
            inject("Down");
            assert!(
                advanced.has_focus(),
                "B06 physical Down must reach the new guide-selection action",
            );
            inject("Return");
            wait(
                &|| c4_picker_modal(win.upcast_ref(), "broadcaster-full-mapping-editor").is_some(),
                "B06 full editor opened through real compact picker",
            );
            assert!(!compact.is_mapped(), "B06 compact picker must be closed");
            let full =
                c4_picker_modal(win.upcast_ref(), "broadcaster-full-mapping-editor").unwrap();
            fn dropdown(widget: &gtk::Widget) -> Option<gtk::DropDown> {
                if let Ok(d) = widget.clone().downcast::<gtk::DropDown>() {
                    return Some(d);
                }
                let mut child = widget.first_child();
                while let Some(c) = child {
                    if let Some(d) = dropdown(&c) {
                        return Some(d);
                    }
                    child = c.next_sibling();
                }
                None
            }
            let picker = dropdown(full.upcast_ref()).expect("B06 full editor actual guide chooser");
            // GtkDropDown delegates the real focus owner to a descendant
            // GtkToggleButton. Accept that genuine mapped child, not only
            // parent GtkDropDown::has_focus (which is false by design).
            for _ in 0..35 {
                if c4_native_focus_within(&win, picker.upcast_ref()) {
                    break;
                }
                inject("Down");
            }
            assert!(
                c4_native_focus_within(&win, picker.upcast_ref()),
                "R1 remote must reach the real focusable dropdown child"
            );
            assert!(
                c4_native_visible_in_viewport(&win, picker.upcast_ref()),
                "R1 guide selector must fit in real rendered scroll viewport"
            );
            inject("Return");
            spin();
            inject("Home");
            spin();
            inject("Return");
            spin();
            let selected = picker
                .selected_item()
                .and_downcast::<gtk::StringObject>()
                .expect("B06 selected string object")
                .string();
            assert_eq!(
                selected.as_str(),
                "C4 other guide",
                "B06 physical guide selection"
            );
            let playlist = c4_picker_button(full.upcast_ref(), "Use as playlist guide exception");
            for _ in 0..48 {
                if playlist.has_focus() {
                    break;
                }
                inject("Down");
            }
            assert!(
                playlist.has_focus(),
                "R1 D-pad Down must reach playlist guide exception"
            );
            assert!(
                c4_native_visible_in_viewport(&win, playlist.upcast_ref()),
                "R1 playlist guide action must be fully visible after physical scroll"
            );
            inject("Return");
            wait(
                &|| {
                    c4_picker_modal(win.upcast_ref(), "broadcaster-full-mapping-editor").is_some()
                        && c4_picker_has_label(
                            win.upcast_ref(),
                            "Make this guide an explicit exception",
                        )
                },
                "B06 confirm playlist-specific guide warning",
            );
            let confirm = c4_picker_button(win.upcast_ref(), "Confirm Playlist Exception");
            for _ in 0..22 {
                if confirm.has_focus() {
                    break;
                }
                inject("Down");
            }
            assert!(
                confirm.has_focus(),
                "R1 D-pad must reach real guide confirmation"
            );
            assert!(
                c4_native_visible_in_viewport(&win, confirm.upcast_ref()),
                "R1 confirm guide exception must lie entirely in viewport"
            );
            inject("Return");
            wait(
                &|| {
                    let db = store.connect().unwrap();
                    db.query_row(
                        "SELECT guide_source_id FROM epg_playlist_links WHERE playlist_source_id=?1",
                        [&playlist_id],
                        |r| r.get::<_,String>(0),
                    ).ok().as_deref() == Some(other_guide.as_str())
                },
                "B06 SQLite committed selected playlist guide",
            );
            let db = store.connect().unwrap();
            let unrelated: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                    [&ids[1]],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(unrelated, 1, "B06 unrelated per-row binding survives");
            let selected = db
                .query_row(
                    "SELECT guide_source_id FROM epg_playlist_links WHERE playlist_source_id=?1",
                    [&playlist_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap();
            if false_exception {
                assert_eq!(
                    selected, guide,
                    "R07_B06_EXPECTED_NEGATIVE_ORACLE: incorrect playlist guide accepted"
                );
            }
            assert_eq!(selected, other_guide, "B06 exact chosen guide committed");
            full.close();
            win.destroy();
            return;
        }

        // Suspend a completed background snapshot just before presentation,
        // switch the selected card while suspended, and assert no stale modal
        // becomes visible after release.
        editor::c4_test_pause_picker(true);
        view.remote_action_test_select(2);
        spin();
        inject("Right");
        inject("Down");
        inject("Down");
        inject("Return");
        wait(
            &editor::c4_test_picker_loaded,
            "paused completed picker snapshot",
        );
        assert!(editor::c4_test_picker_dialog().is_none());
        view.remote_action_test_select(0);
        editor::c4_test_pause_picker(false);
        spin();
        assert!(
            editor::c4_test_picker_dialog().is_none(),
            "stale source/row completion must never present a picker"
        );
        assert!(view.card_mapping_context(&ids[2]).is_none());

        if guide_race {
            // B13: hold a *completed* picker snapshot while the user uses the
            // actual Source -> Programme Guides -> Remove -> Confirm dialogs.
            // The deleted guide must never be resurrected by this stale async
            // completion or become a writable cross-guide candidate.
            editor::c4_test_pause_picker(true);
            view.remote_action_test_select(2);
            spin();
            inject("Right");
            inject("Down");
            inject("Down");
            inject("Return");
            wait(
                &editor::c4_test_picker_loaded,
                "B13 completed picker snapshot pending real manager deletion",
            );
            assert!(editor::c4_test_picker_dialog().is_none());
            let manage = view.c1_test_manage_button();
            assert!(manage.grab_focus());
            inject("Return");
            wait(
                &|| c4_picker_modal(win.upcast_ref(), "broadcaster-source-manager").is_some(),
                "B13 real Source Manager",
            );
            let source_manager =
                c4_picker_modal(win.upcast_ref(), "broadcaster-source-manager").unwrap();
            let guides = c4_picker_button(source_manager.upcast_ref(), "Programme Guides");
            assert!(guides.grab_focus());
            inject("Return");
            wait(
                &|| c4_picker_modal(win.upcast_ref(), "broadcaster-guide-manager").is_some(),
                "B13 real nested Guide Manager",
            );
            let guide_manager =
                c4_picker_modal(win.upcast_ref(), "broadcaster-guide-manager").unwrap();
            fn matching_guide_row(root: &gtk::Widget, expected: &str) -> Option<gtk::Widget> {
                if let Ok(label) = root.clone().downcast::<gtk::Label>() {
                    if label.text().starts_with(&format!("{expected} —")) {
                        return label.parent();
                    }
                }
                let mut child = root.first_child();
                while let Some(next) = child {
                    if let Some(found) = matching_guide_row(&next, expected) {
                        return Some(found);
                    }
                    child = next.next_sibling();
                }
                None
            }
            let selected_row = matching_guide_row(guide_manager.upcast_ref(), "C4 selected guide")
                .expect("B13 exact selected guide row in real manager");
            let remove = c4_picker_button(&selected_row, "Remove");
            assert!(remove.grab_focus());
            inject("Return");
            wait(
                &|| {
                    c4_picker_buttons(win.upcast_ref()).iter().any(|b| {
                        b.label().as_deref() == Some("Remove Permanently") && b.is_mapped()
                    })
                },
                "B13 real removal confirmation",
            );
            let permanent = c4_picker_button(win.upcast_ref(), "Remove Permanently");
            assert!(permanent.grab_focus());
            inject("Return");
            wait(
                &|| {
                    let db = store.connect().unwrap();
                    db.query_row(
                        "SELECT COUNT(*) FROM epg_sources WHERE id=?1",
                        [&guide],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap()
                        == 0
                },
                "B13 selected guide removed transactionally through real manager",
            );
            let guide_close = c4_picker_button(guide_manager.upcast_ref(), "Close");
            assert!(guide_close.grab_focus());
            inject("XF86Back");
            wait(&|| !guide_manager.is_mapped(), "B13 close guide manager");
            let source_close = c4_picker_button(source_manager.upcast_ref(), "Close");
            assert!(source_close.grab_focus());
            inject("XF86Back");
            wait(&|| !source_manager.is_mapped(), "B13 close source manager");
            editor::c4_test_pause_picker(false);
            for _ in 0..18 {
                spin();
            }
            assert!(
                editor::c4_test_picker_dialog().is_none(),
                "B13 deleted guide cannot present stale mapped picker",
            );
            let count: i64 = store
                .connect()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                    [&ids[2]],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "B13 deleted guide cannot persist mapping");
            if wrong_race {
                assert!(
                    store
                        .connect()
                        .unwrap()
                        .query_row(
                            "SELECT COUNT(*) FROM epg_sources WHERE id=?1",
                            [&guide],
                            |row| row.get::<_, i64>(0),
                        )
                        .unwrap()
                        > 0,
                    "R07_B13_EXPECTED_NEGATIVE_ORACLE",
                );
            }
        }
        if source_race {
            // B12: preserve a completed real picker snapshot, then replace
            // the source catalogue through a *real physical* manager Refresh.
            // The local M3U keeps the first stream but makes the focused last
            // stream absent. No HTTP/network and no production playlist data.
            let local = std::path::Path::new(&data_home).join("r07-picker-refresh.m3u");
            std::fs::write(
                &local,
                "#EXTM3U\n#EXTINF:-1 tvg-id=\"zero.original\",Zero match\nhttps://example.invalid/c4-picker/0.m3u8\n",
            ).expect("B12 disposable local M3U");
            let uri = gio::File::for_path(&local).uri();
            store.connect().unwrap().execute(
                "UPDATE iptv_sources SET origin_kind='local_file',origin_locator=?1 WHERE id=?2",
                rusqlite::params![uri.as_str(), &playlist_id],
            ).expect("B12 local network-free source origin");
            editor::c4_test_pause_picker(true);
            view.remote_action_test_select(2);
            spin();
            inject("Right");
            inject("Down");
            inject("Down");
            inject("Return");
            wait(
                &editor::c4_test_picker_loaded,
                "B12 selected last row picker snapshot pending real source Refresh",
            );
            assert!(editor::c4_test_picker_dialog().is_none());
            let manage = view.c1_test_manage_button();
            assert!(manage.grab_focus());
            inject("Return");
            wait(
                &|| c4_picker_modal(win.upcast_ref(), "broadcaster-source-manager").is_some(),
                "B12 native Source Manager",
            );
            let manager = c4_picker_modal(win.upcast_ref(), "broadcaster-source-manager").unwrap();
            let refresh = c4_picker_button(manager.upcast_ref(), "Refresh");
            assert!(refresh.grab_focus());
            inject("Return");
            wait(
                &|| {
                    store
                        .query_channels(&ChannelQuery::default())
                        .is_ok_and(|rows| rows.total == 1)
                },
                "B12 real local Refresh removed two channels in disposable SQLite",
            );
            wait(
                &|| view.remote_action_test_visible_count() == 1,
                "B12 async view rebind after source refresh",
            );
            let close = c4_picker_button(manager.upcast_ref(), "Close");
            assert!(close.grab_focus());
            inject("XF86Back");
            wait(&|| !manager.is_mapped(), "B12 close real Source Manager");
            editor::c4_test_pause_picker(false);
            for _ in 0..18 {
                spin();
            }
            assert!(
                editor::c4_test_picker_dialog().is_none(),
                "B12 removed source row must not resurrect a stale mapping modal",
            );
            let old_present: i64 = store
                .connect()
                .unwrap()
                .query_row(
                    "SELECT is_present FROM iptv_channels WHERE id=?1",
                    [&ids[2]],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                old_present, 0,
                "B12 focused row absent in actual SQLite refresh"
            );
            let overrides: i64 = store
                .connect()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                    [&ids[2]],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(overrides, 0, "B12 cannot create override for absent stream");
            if wrong_race {
                assert_eq!(old_present, 1, "R07_B12_EXPECTED_NEGATIVE_ORACLE");
            }
        }
        if e08_actions {
            // R2 negative-input regression: the last source-qualified row
            // remains valid in SQLite, but it has *no* expanded/bound card
            // when another row is selected. Opening the async full editor
            // must fail closed before launching its detail fetch, and may
            // not expose a modal or write any guide/row mapping.
            view.remote_action_test_select(0);
            spin();
            assert!(
                view.card_mapping_context(&ids[2]).is_none(),
                "R2 initial unbound card context must be absent"
            );
            let link_before: i64 = store
                .connect()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM epg_playlist_links WHERE playlist_source_id=?1",
                    [&playlist_id],
                    |r| r.get(0),
                )
                .unwrap();
            let override_before: i64 = store
                .connect()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                    [&ids[2]],
                    |r| r.get(0),
                )
                .unwrap();
            editor::show_epg_mapping_editor(&view, ids[2].clone());
            for _ in 0..12 {
                spin();
            }
            assert!(
                c4_picker_modal(win.upcast_ref(), "broadcaster-full-mapping-editor").is_none(),
                "R2 initially absent card generation must never present full mapping editor"
            );
            let db = store.connect().unwrap();
            let link_after: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM epg_playlist_links WHERE playlist_source_id=?1",
                    [&playlist_id],
                    |r| r.get(0),
                )
                .unwrap();
            let override_after: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                    [&ids[2]],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                (link_after, override_after),
                (link_before, override_before),
                "R2 no manual playlist/row write after initially invalidated card"
            );
        }
        win.destroy();
    }

    #[test]
    #[ignore = "real selected-guide mapping modal, CAS and XTest"]
    fn c4_native_mapping_640_480() {
        c4_native_mapping_picker(
            640,
            480,
            false,
            false,
            false,
            false,
            (false, false, false, false),
        );
    }

    #[test]
    #[ignore = "real selected-guide mapping modal, CAS and XTest"]
    fn c4_native_mapping_1280_800() {
        c4_native_mapping_picker(
            1280,
            800,
            false,
            false,
            false,
            false,
            (false, false, false, false),
        );
    }

    #[test]
    #[ignore = "R07 G06 real mapped programme picker and nested OSK painted X11 compact"]
    fn r07_native_picker_osk_paint_640_480() {
        c4_native_mapping_picker(
            640,
            480,
            true,
            false,
            false,
            false,
            (false, false, false, false),
        );
    }
    #[test]
    #[ignore = "R07 G06 real mapped programme picker and nested OSK painted X11 TV"]
    fn r07_native_picker_osk_paint_1280_800() {
        c4_native_mapping_picker(
            1280,
            800,
            true,
            false,
            false,
            false,
            (false, false, false, false),
        );
    }
    #[test]
    #[ignore = "R07 G06 wrong missing-paint oracle must be rejected"]
    fn r07_native_picker_osk_paint_negative() {
        c4_native_mapping_picker(
            640,
            480,
            true,
            true,
            false,
            false,
            (false, false, false, false),
        );
    }

    #[test]
    #[ignore = "B13 manager-initiated real guide deletion while picker load is suspended"]
    fn r07_native_picker_guide_removed_640_480() {
        c4_native_mapping_picker(
            640,
            480,
            false,
            false,
            true,
            false,
            (false, false, false, false),
        );
    }
    #[test]
    #[ignore = "B13 manager-initiated real guide deletion while picker load is suspended"]
    fn r07_native_picker_guide_removed_1280_800() {
        c4_native_mapping_picker(
            1280,
            800,
            false,
            false,
            true,
            false,
            (false, false, false, false),
        );
    }
    #[test]
    #[ignore = "B13 deliberately false removed-guide persistence oracle"]
    fn r07_native_picker_guide_removed_negative() {
        c4_native_mapping_picker(
            640,
            480,
            false,
            false,
            true,
            true,
            (false, false, false, false),
        );
    }

    #[test]
    #[ignore = "B12 real manager Refresh removes focused stream during completed picker snapshot"]
    fn r07_native_picker_source_refreshed_640_480() {
        c4_native_mapping_picker(
            640,
            480,
            false,
            false,
            false,
            false,
            (true, false, false, false),
        );
    }
    #[test]
    #[ignore = "B12 real manager Refresh removes focused stream during completed picker snapshot"]
    fn r07_native_picker_source_refreshed_1280_800() {
        c4_native_mapping_picker(
            1280,
            800,
            false,
            false,
            false,
            false,
            (true, false, false, false),
        );
    }
    #[test]
    #[ignore = "B12 deliberately wrong missing-row identity after manager Refresh"]
    fn r07_native_picker_source_refreshed_negative() {
        c4_native_mapping_picker(
            640,
            480,
            false,
            false,
            false,
            true,
            (true, false, false, false),
        );
    }

    #[test]
    #[ignore = "R07 E08 real first/middle/last SQLite Play/Edit/Mapping physical modal returns compact"]
    fn r07_native_e08_action_graph_640_480() {
        c4_native_mapping_picker(
            640,
            480,
            false,
            false,
            false,
            false,
            (false, false, false, true),
        );
    }

    #[test]
    #[ignore = "R07 E08 real first/middle/last SQLite Play/Edit/Mapping physical modal returns TV"]
    fn r07_native_e08_action_graph_1280_800() {
        c4_native_mapping_picker(
            1280,
            800,
            false,
            false,
            false,
            false,
            (false, false, false, true),
        );
    }

    #[test]
    #[ignore = "R07 E08 wrong middle-card identity must fail on the precise return assertion"]
    fn r07_native_e08_action_graph_negative() {
        c4_native_mapping_picker(
            640,
            480,
            false,
            true,
            false,
            false,
            (false, false, false, true),
        );
    }

    #[test]
    #[ignore = "R1 deliberately wrong compact footer visibility oracle"]
    fn r07_native_compact_geometry_negative() {
        c4_native_mapping_picker(
            640,
            480,
            false,
            false,
            false,
            true,
            (false, false, false, true),
        );
    }

    #[test]
    #[ignore = "B06 live compact picker to confirmed playlist guide selection"]
    fn r07_native_playlist_guide_640_480() {
        c4_native_mapping_picker(
            640,
            480,
            false,
            false,
            false,
            false,
            (false, true, false, false),
        );
    }
    #[test]
    #[ignore = "B06 live compact picker to confirmed playlist guide selection"]
    fn r07_native_playlist_guide_1280_800() {
        c4_native_mapping_picker(
            1280,
            800,
            false,
            false,
            false,
            false,
            (false, true, false, false),
        );
    }
    #[test]
    #[ignore = "B06 wrong confirmed guide identity rejected at SQLite oracle"]
    fn r07_native_playlist_guide_negative() {
        c4_native_mapping_picker(
            640,
            480,
            false,
            false,
            false,
            false,
            (false, true, true, false),
        );
    }

    fn run_mapped_manager(width: i32, height: i32) {
        adw::init().expect("display");
        let resource =
            gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
        gio::resources_register(&resource);
        let app = TvApplication::new();
        app.register(None::<&gio::Cancellable>)
            .expect("application startup");
        let win = crate::window::TvWindow::new(&app);
        win.set_default_size(width, height);
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
        // A single source must not expose either movement edge. These
        // disabled controls make the ordinary nearest-neighbour rule lose
        // the upper action when Down is immediately reversed by Up.
        buttons[5].set_sensitive(false); // Move Up
        buttons[6].set_sensitive(false); // Move Down
        bind_manager_navigation(&view, &list, &row, &actions, "fixture-source", 0);
        dialog.present(Some(&win));
        spin();
        assert!(row.grab_focus(), "portal focus");
        inject("Return");
        assert!(
            enabled.has_focus(),
            "row OK portal focuses Switch without toggling"
        );
        assert_eq!(
            view.manager_focus(),
            Some(("fixture-source".to_owned(), 0, 0)),
            "semantic Switch slot is remembered for same-source async rebind"
        );
        assert!(
            !enabled.is_active(),
            "fixture Switch starts OFF and OK must not toggle it"
        );
        inject("Right");
        inject("Right");
        assert!(buttons[1].has_focus(), "Right reaches Defaults");
        inject("Down");
        assert!(
            buttons[4].has_focus(),
            "Defaults falls back to enabled Delete"
        );
        inject("Up");
        assert!(
            buttons[1].has_focus(),
            "Up reverses to the same Defaults action"
        );
        inject("Right");
        assert!(buttons[2].has_focus(), "Right reaches Rename");
        inject("Down");
        assert!(
            buttons[4].has_focus(),
            "Rename falls back to enabled Delete"
        );
        inject("Up");
        assert!(
            buttons[2].has_focus(),
            "Up reverses to the same Rename action"
        );
        // External/programmatic focus movements must discard the previous
        // vertical hop. Without this, Delete -> Up wrongly returns Defaults,
        // even though Refresh is the nearest enabled upper action.
        assert!(buttons[1].grab_focus(), "focus Defaults externally");
        spin();
        inject("Down");
        assert!(buttons[4].has_focus(), "Down reaches Delete");
        assert!(buttons[2].grab_focus(), "focus Rename externally");
        spin();
        assert_eq!(
            view.manager_focus().as_ref().map(|(_, slot, _)| *slot),
            Some(3),
            "external Rename focus must update the semantic action"
        );
        assert!(buttons[4].grab_focus(), "focus Delete externally");
        spin();
        assert_eq!(
            view.manager_focus().as_ref().map(|(_, slot, _)| *slot),
            Some(5),
            "external Delete focus must update the semantic action"
        );
        inject("Up");
        assert!(
            buttons[0].has_focus(),
            "Up after external focus shifts follows fresh Delete -> Refresh"
        );
        inject("XF86Back");
        assert!(
            row.has_focus(),
            "physical Back returns to the same source row"
        );
        assert!(dialog.is_visible(), "first Back keeps manager open");
        inject("XF86Back");
        // adw::Dialog leaves GTK's visible flag set after it is closed; the
        // meaningful dismissal oracle is whether it remains *mapped*.
        // The prior one-row test checked only focus loss (too weak), while a
        // visible-flag assertion incorrectly REDs a genuinely closed dialog.
        assert!(
            !dialog.is_mapped(),
            "second physical Back must unmap the source manager"
        );
        assert!(
            dialog.parent().is_none(),
            "dismissed source manager must be removed from the dialog host"
        );
        assert!(!row.has_focus(), "dismissal must release the source row");
    }
}

#[cfg(test)]
#[path = "management/c1_native_service_tests.rs"]
mod c1_native_service_tests;

#[cfg(test)]
mod r07_facets_tests;

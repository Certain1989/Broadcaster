// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Small remote-friendly, explicitly consented XMLTV configuration dialog.
//! It does not consult playlist EPG hints, open guides or fetch on repaint.
use adw::{gio, glib, gtk, prelude::*};
use gettextrs::gettext;
use std::sync::OnceLock;

use super::{
    network::TrustScope,
    service::GlobalGuideManager,
    sources::{ConfirmedGuideDeletion, GuideSource, SourceOrigin},
    xmltv::StreamedRows,
};

use crate::utils::spawn;

static SHARED_MANAGER: OnceLock<tokio::sync::OnceCell<GlobalGuideManager>> = OnceLock::new();

/// Friendly explanations for fixed, sanitized EPG result codes.
fn guide_problem(code: &str) -> String {
    match code {
        "invalid_epg_source" => gettext("Enter a valid guide name and XMLTV location."),
        "epg_source_already_exists" => gettext("This programme guide has already been added."),
        "epg_source_not_found" => gettext("The programme guide no longer exists."),
        "epg_source_changed" => gettext("Guide settings changed. Close this dialog and reopen it."),
        "epg_source_disabled" | "source_disabled" => {
            gettext("Enable this guide before refreshing it.")
        }
        "invalid_origin" => gettext("Enter a valid HTTP(S) URL or select a local XMLTV file."),
        "network_scope_denied" | "redirect_denied" => {
            gettext("This guide's network location is not allowed.")
        }
        "http_status" => gettext("The guide server returned an HTTP error."),
        "network" => gettext("Cannot reach the guide server. Check your connection."),
        "timeout" => gettext("The guide download timed out."),
        "oversize_description" => {
            gettext("A programme description is longer than the permitted limit.")
        }
        "oversize" | "oversize_wire" | "oversize_expanded" | "oversize_ratio" => {
            gettext("The guide exceeds a download or decompression safety limit.")
        }
        "oversize_database" => gettext("The guide exceeds the database storage limit."),
        "oversize_duration" => gettext("The guide import took too long."),
        "oversize_title" => gettext("A programme title in this guide exceeds the 512-byte limit."),
        "oversize_subtitle" => {
            gettext("A programme subtitle in this guide exceeds the 512-byte limit.")
        }
        "oversize_channel_name" => {
            gettext("A channel display name in this guide exceeds the 512-byte limit.")
        }
        "oversize_depth"
        | "oversize_attributes"
        | "oversize_xml_text"
        | "oversize_channels"
        | "oversize_programmes" => gettext("The guide exceeds the XMLTV safety limits."),
        "invalid_xml" => gettext(
            "The guide could not be imported because it contains unsupported or inconsistent XMLTV data.",
        ),
        "encoding" => gettext("The guide is not valid UTF-8 XMLTV data."),
        "disk_full" => gettext(
            "The guide could not be saved because there is not enough available disk space. Any previously saved guide is still available."
        ),
        "storage_failed" | "epg_storage_failed" => {
            gettext("The guide could not be saved because of a storage error.")
        }
        "storage_permission" => gettext(
            "Broadcaster cannot write to its guide storage. Check the application's storage permissions."
        ),
        "local_access" => gettext("The selected local XMLTV file cannot be opened."),
        "cancelled" => gettext("Guide refresh cancelled."),
        "refresh_in_progress" => gettext("This guide is already being refreshed."),
        _ => gettext("The guide operation failed. Please try again."),
    }
}

/// Form sentences only from validated, source-independent counters.
fn truncation_messages(counts: &StreamedRows) -> Vec<String> {
    let mut messages = Vec::new();
    if counts.truncated_titles == 1 {
        messages.push(gettext(
            "One programme title was shortened to fit the 512-byte limit.",
        ));
    } else if counts.truncated_titles > 1 {
        messages.push(format!(
            "{} {}",
            counts.truncated_titles,
            gettext("programme titles were shortened to fit the 512-byte limit."),
        ));
    }
    if counts.truncated_subtitles == 1 {
        messages.push(gettext(
            "One programme subtitle was shortened to fit the 512-byte limit.",
        ));
    } else if counts.truncated_subtitles > 1 {
        messages.push(format!(
            "{} {}",
            counts.truncated_subtitles,
            gettext("programme subtitles were shortened to fit the 512-byte limit."),
        ));
    }
    if counts.truncated_channel_names == 1 {
        messages.push(gettext(
            "One channel display name was shortened to fit the 512-byte limit.",
        ));
    } else if counts.truncated_channel_names > 1 {
        messages.push(format!(
            "{} {}",
            counts.truncated_channel_names,
            gettext("channel display names were shortened to fit the 512-byte limit."),
        ));
    }
    if counts.truncated_descriptions == 1 {
        messages.push(gettext(
            "One programme description was shortened to fit the 2,048-byte limit.",
        ));
    } else if counts.truncated_descriptions > 1 {
        messages.push(format!(
            "{} {}",
            counts.truncated_descriptions,
            gettext("programme descriptions were shortened to fit the 2,048-byte limit."),
        ));
    }
    if counts.conflicting_duplicate_rows == 1 {
        messages.push(gettext(
            "One repeated programme had conflicting optional metadata; inconsistent fields were omitted.",
        ));
    } else if counts.conflicting_duplicate_rows > 1 {
        messages.push(format!(
            "{} {}",
            counts.conflicting_duplicate_rows,
            gettext("repeated programme entries had conflicting optional metadata; inconsistent fields were omitted."),
        ));
    }
    if counts.omitted_c1_programmes == 1 {
        messages.push(gettext(
            "One programme was omitted because its description contains unsupported control characters.",
        ));
    } else if counts.omitted_c1_programmes > 1 {
        messages.push(format!(
            "{} {}",
            counts.omitted_c1_programmes,
            gettext("programmes were omitted because their descriptions contain unsupported control characters."),
        ));
    }
    messages
}

fn box_column() -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 10);
    box_.set_margin_start(16);
    box_.set_margin_end(16);
    box_.set_margin_top(16);
    box_.set_margin_bottom(16);
    box_
}
fn status_label() -> gtk::Label {
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    status
}
fn command(label: &str, parent: &gtk::Box) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    parent.append(&button);
    button
}
// The viewport must never become a D-pad stop when there are no guide rows.
// Its actual row buttons remain focusable when sources have been added.
fn guide_list_scroller() -> gtk::ScrolledWindow {
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_focusable(false);
    scroller.set_vexpand(true);
    scroller
}

fn title(dialog: &adw::Dialog, label: &str) {
    dialog.set_title(label);
    dialog.set_content_width(560);
    dialog.set_content_height(550);
}
fn edit_field(label: &str, initial: &str, body: &gtk::Box) -> gtk::Entry {
    let caption = gtk::Label::new(Some(label));
    caption.set_xalign(0.0);
    body.append(&caption);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let entry = gtk::Entry::new();
    entry.set_hexpand(true);
    entry.set_text(initial);
    line.append(&entry);
    let keyboard = gtk::Button::from_icon_name("input-keyboard-symbolic");
    crate::keyboard::bind_launcher(
        &keyboard,
        &entry,
        crate::keyboard::KeyboardOptions::default(),
    );
    line.append(&keyboard);
    body.append(&line);
    vertical_link(&entry, &keyboard);
    entry
}

fn vertical_link<A: IsA<gtk::Widget>, B: IsA<gtk::Widget>>(above: &A, below: &B) {
    let down = gtk::EventControllerKey::new();
    down.set_propagation_phase(gtk::PropagationPhase::Capture);
    let next = below.clone().upcast::<gtk::Widget>().downgrade();
    down.connect_key_pressed(move |_, key, _, modifiers| {
        if crate::keyboard::unmodified_remote(modifiers)
            && matches!(key, gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down)
            && next
                .upgrade()
                .is_some_and(|w| w.is_mapped() && w.is_sensitive() && w.grab_focus())
        {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    above.add_controller(down);
    let up = gtk::EventControllerKey::new();
    up.set_propagation_phase(gtk::PropagationPhase::Capture);
    let previous = above.clone().upcast::<gtk::Widget>().downgrade();
    up.connect_key_pressed(move |_, key, _, modifiers| {
        if crate::keyboard::unmodified_remote(modifiers)
            && matches!(key, gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up)
            && previous
                .upgrade()
                .is_some_and(|w| w.is_mapped() && w.is_sensitive() && w.grab_focus())
        {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    below.add_controller(up);
}

// Fixed action footer remains reachable on 640x480; editable controls scroll.
fn form_with_footer(dialog: &adw::Dialog, body: &gtk::Box, actions: &[gtk::Button]) {
    let shell = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let scroll = guide_list_scroller();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(body));
    shell.append(&scroll);
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    footer.set_margin_start(16);
    footer.set_margin_end(16);
    footer.set_margin_bottom(12);
    for action in actions {
        body.remove(action);
        footer.append(action);
    }
    shell.append(&footer);
    dialog.set_child(Some(&shell));
}

fn focus_when_present<T: IsA<gtk::Widget>>(dialog: &adw::Dialog, target: &T) {
    let weak = target.clone().upcast::<gtk::Widget>().downgrade();
    dialog.connect_map(move |_| {
        let weak = weak.clone();
        glib::idle_add_local_once(move || {
            if let Some(widget) = weak.upgrade() {
                if widget.is_mapped() && widget.is_sensitive() {
                    widget.grab_focus();
                }
            }
        });
    });
}

async fn save_default_guide(source_id: Option<String>) -> Result<(), &'static str> {
    crate::utils::tokio(async move {
        tokio::task::spawn_blocking(move || {
            let mut conn = crate::streams::storage::Storage::new()
                .connect()
                .map_err(|_| "Cannot open programme data")?;
            super::bindings::set_shared_guide(
                &mut conn,
                super::bindings::ConfirmedGuideSelection::after_user_confirmation(),
                source_id.as_deref(),
            )
            .map_err(|_| "Programme guide changed or is unavailable")
        })
        .await
        .map_err(|_| "Programme guide worker unavailable")?
    })
    .await
}

fn confirm_shared_guide(
    parent: &gtk::Window,
    source: Option<GuideSource>,
    manager: GlobalGuideManager,
    list: gtk::Box,
    status: gtk::Label,
) {
    let dialog = adw::Dialog::new();
    title(&dialog, &gettext("Confirm Shared Programme Guide"));
    let body = box_column();
    let caption = gtk::Label::new(Some(&match &source {
        Some(source) => format!(
            "{}\n{}",
            source.name,
            gettext("Use this guide as the application-wide default? Per-stream overrides still take priority.")
        ),
        None => gettext("Clear the shared default guide? Explicit per-stream overrides remain unchanged."),
    }));
    caption.set_wrap(true);
    body.append(&caption);
    let failure = status_label();
    body.append(&failure);
    let cancel = command(&gettext("Cancel"), &body);
    let confirm = command(
        &if source.is_some() {
            gettext("Confirm Shared Default")
        } else {
            gettext("Clear Shared Default")
        },
        &body,
    );
    confirm.add_css_class("suggested-action");
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    focus_when_present(&dialog, &cancel);
    let callback_parent = parent.clone();
    confirm.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        failure,
        #[weak]
        confirm,
        move |_| {
            confirm.set_sensitive(false);
            let id = source.as_ref().map(|source| source.id.clone());
            let selected = id.clone();
            let manager = manager.clone();
            let list = list.clone();
            let status = status.clone();
            let parent = callback_parent.clone();
            spawn(glib::clone!(
                #[weak]
                dialog,
                #[weak]
                failure,
                #[weak]
                confirm,
                async move {
                    match save_default_guide(id).await {
                        Ok(()) => {
                            dialog.close();
                            status.set_text(&gettext("Shared guide selection updated"));
                            if let Some(id) = selected {
                                repaint_focused(manager, list, status, parent, Some((id, 0)));
                            } else {
                                repaint(manager, list, status, parent);
                            }
                        }
                        Err(reason) => {
                            failure.set_text(&gettext(reason));
                            confirm.set_sensitive(true);
                        }
                    }
                }
            ));
        }
    ));
    dialog.set_child(Some(&body));
    dialog.present(Some(parent));
}

// Every repaint destroys its previous row widgets. Restore by saved guide UUID.
fn repaint(service: GlobalGuideManager, list: gtk::Box, status: gtk::Label, parent: gtk::Window) {
    repaint_focused(service, list, status, parent, None);
}
fn repaint_focused(
    service: GlobalGuideManager,
    list: gtk::Box,
    status: gtk::Label,
    parent: gtk::Window,
    focus: Option<(String, usize)>,
) {
    spawn(async move {
        match service.list().await {
            Ok(sources) => {
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                if sources.is_empty() {
                    list.append(&gtk::Label::new(Some(&gettext(
                        "No programme guides configured",
                    ))));
                }
                let mut focus_rows: Vec<(String, [gtk::Button; 4])> = Vec::new();
                for source in sources {
                    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
                    row.set_margin_top(8);
                    row.set_margin_bottom(8);
                    let state = if !source.enabled {
                        gettext("Disabled")
                    } else if source.last_success_at.is_some() {
                        gettext("Cached guide available")
                    } else {
                        gettext("Not refreshed yet")
                    };
                    let caption = gtk::Label::new(Some(&format!("{} — {}", source.name, state)));
                    caption.set_xalign(0.0);
                    caption.set_wrap(true);
                    row.append(&caption);
                    if let Some(code) = source.last_error_code {
                        let failure = gtk::Label::new(Some(&format!(
                            "{}: {}",
                            gettext("Last refresh failed"),
                            guide_problem(code)
                        )));
                        failure.set_xalign(0.0);
                        failure.set_wrap(true);
                        failure.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                        failure.set_max_width_chars(48);
                        failure.set_hexpand(true);
                        row.append(&failure);
                    }
                    if source.last_success_at.is_some() {
                        if let Some(counts) = service.truncation_counts(&source.id).await {
                            for message in truncation_messages(&counts) {
                                let warning = gtk::Label::new(Some(&message));
                                warning.set_wrap(true);
                                warning.set_xalign(0.0);
                                warning.set_max_width_chars(48);
                                row.append(&warning);
                            }
                        }
                    }
                    // Two action lines fit narrow TV viewports without clipping.
                    let first = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    let set_default = command(&gettext("Use as Shared Default"), &first);
                    let refresh = command(&gettext("Refresh"), &first);
                    refresh.set_sensitive(source.enabled);
                    let second = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    let configure = command(&gettext("Edit"), &second);
                    let remove = command(&gettext("Remove"), &second);
                    row.append(&first);
                    row.append(&second);
                    list.append(&row);
                    focus_rows.push((
                        source.id.clone(),
                        [
                            set_default.clone(),
                            refresh.clone(),
                            configure.clone(),
                            remove.clone(),
                        ],
                    ));
                    let default_service = service.clone();
                    let default_list = list.clone();
                    let default_status = status.clone();
                    let default_parent = parent.clone();
                    let default_source = source.clone();
                    set_default.connect_clicked(move |_| {
                        confirm_shared_guide(
                            &default_parent,
                            Some(default_source.clone()),
                            default_service.clone(),
                            default_list.clone(),
                            default_status.clone(),
                        );
                    });
                    let id = source.id.clone();
                    let fetch_service = service.clone();
                    let fetch_list = list.clone();
                    let fetch_status = status.clone();
                    let fetch_parent = parent.clone();
                    refresh.connect_clicked(move |button| {
                        button.set_sensitive(false);
                        fetch_status.set_text(&gettext("Refreshing guide…"));
                        let manager = fetch_service.clone();
                        let id = id.clone();
                        let listing = fetch_list.clone();
                        let state = fetch_status.clone();
                        let window = fetch_parent.clone();
                        spawn(async move {
                            let result = manager.refresh(id.clone()).await;
                            let message = match result {
                                Ok(()) => {
                                    let details = manager
                                        .truncation_counts(&id)
                                        .await
                                        .map(|counts| truncation_messages(&counts))
                                        .unwrap_or_default();
                                    if details.is_empty() {
                                        gettext("Guide refreshed")
                                    } else {
                                        format!(
                                            "{} {}",
                                            gettext("Guide refreshed."),
                                            details.join(" ")
                                        )
                                    }
                                }
                                Err(error) => format!(
                                    "{}: {}",
                                    gettext("Refresh failed"),
                                    guide_problem(&error.to_string()),
                                ),
                            };
                            state.set_text(&message);
                            repaint_focused(manager, listing, state, window, Some((id, 1)));
                        });
                    });
                    let edit_service = service.clone();
                    let edit_list = list.clone();
                    let edit_status = status.clone();
                    let edit_parent = parent.clone();
                    let settings_source = source.clone();
                    configure.connect_clicked(move |_| {
                        show_source_settings(
                            &edit_parent,
                            edit_service.clone(),
                            settings_source.clone(),
                            edit_list.clone(),
                            edit_status.clone(),
                        );
                    });
                    let delete_service = service.clone();
                    let delete_list = list.clone();
                    let delete_status = status.clone();
                    let delete_parent = parent.clone();
                    let deleted_source = source.clone();
                    remove.connect_clicked(move |_| {
                        show_delete_confirmation(
                            &delete_parent,
                            delete_service.clone(),
                            deleted_source.clone(),
                            delete_list.clone(),
                            delete_status.clone(),
                        );
                    });
                }
                wire_guide_rows(&focus_rows);
                if let Some((id, slot)) = focus {
                    let target = focus_rows
                        .iter()
                        .find(|row| row.0 == id)
                        .or_else(|| focus_rows.first())
                        .and_then(|row| {
                            row.1
                                .get(slot)
                                .filter(|button| button.is_sensitive())
                                .or_else(|| row.1.iter().find(|button| button.is_sensitive()))
                        })
                        .cloned();
                    let weak_list = list.downgrade();
                    glib::idle_add_local_once(move || {
                        if weak_list.upgrade().is_some_and(|list| list.is_mapped()) {
                            if let Some(button) = target {
                                if button.is_mapped() && button.is_sensitive() {
                                    button.grab_focus();
                                }
                            }
                        }
                    });
                }
            }
            Err(_) => status.set_text(&gettext("Unable to load programme guides")),
        }
    });
}

// D-pad links identify real controls rather than a synthetic Tab chain.
fn wire_guide_rows(rows: &[(String, [gtk::Button; 4])]) {
    for (index, (_, controls)) in rows.iter().enumerate() {
        for slot in 0..4 {
            // Keep *weak* inter-row references: strong cycles between GTK
            // controllers would keep every removed/repainted row alive.
            let left = match slot {
                1 => Some(controls[0].clone()),
                3 => Some(controls[2].clone()),
                _ => None,
            }.map(|button| button.downgrade());
            let right = match slot {
                0 => Some(controls[1].clone()),
                2 => Some(controls[3].clone()),
                _ => None,
            }.map(|button| button.downgrade());
            let up = if slot >= 2 {
                // If Refresh is disabled, Remove goes back to Shared Default.
                Some(controls[if slot == 3 && !controls[1].is_sensitive() {
                    0
                } else { slot - 2 }].clone())
            } else if index > 0 {
                Some(rows[index - 1].1[slot + 2].clone())
            } else {
                None
            }.map(|button| button.downgrade());
            let down = if slot < 2 {
                Some(controls[slot + 2].clone())
            } else {
                rows.get(index + 1).map(|row| row.1[slot - 2].clone())
            }.map(|button| button.downgrade());
            let key = gtk::EventControllerKey::new();
            key.set_propagation_phase(gtk::PropagationPhase::Capture);
            key.connect_key_pressed(move |_, pressed, _, modifiers| {
                if !crate::keyboard::unmodified_remote(modifiers) {
                    return glib::Propagation::Proceed;
                }
                let target = match pressed {
                    gtk::gdk::Key::Left | gtk::gdk::Key::KP_Left => left.as_ref(),
                    gtk::gdk::Key::Right | gtk::gdk::Key::KP_Right => right.as_ref(),
                    gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up => up.as_ref(),
                    gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down => down.as_ref(),
                    _ => None,
                };
                if let Some(candidate) = target {
                    if candidate.upgrade().is_some_and(|button| {
                        button.is_mapped() && button.is_sensitive() && button.grab_focus()
                    }) {
                        glib::Propagation::Stop
                    } else {
                        // A disabled Refresh is not a native focus destination.
                        glib::Propagation::Stop
                    }
                } else {
                    glib::Propagation::Proceed
                }
            });
            controls[slot].add_controller(key);
        }
    }
}

fn show_source_settings(
    parent: &gtk::Window,
    service: GlobalGuideManager,
    source: GuideSource,
    list: gtk::Box,
    status: gtk::Label,
) {
    let dialog = adw::Dialog::new();
    title(&dialog, &gettext("Programme Guide Settings"));
    let body = box_column();
    let name = edit_field(&gettext("Name"), &source.name, &body);
    let enabled = gtk::CheckButton::with_label(&gettext("Enabled"));
    enabled.set_active(source.enabled);
    body.append(&enabled);
    let automatic = gtk::CheckButton::with_label(&gettext("Automatically refresh this guide"));
    automatic.set_active(source.auto_refresh);
    body.append(&automatic);
    let hours = gtk::SpinButton::with_range(6.0, 168.0, 1.0);
    hours.set_value(source.refresh_interval_seconds as f64 / 3600.0);
    body.append(&gtk::Label::new(Some(&gettext(
        "Refresh when the guide is this old (hours, 6–168)",
    ))));
    body.append(&hours);
    let message = status_label();
    body.append(&message);
    let save = command(&gettext("Apply"), &body);
    let close = command(&gettext("Close"), &body);
    if let Some(keyboard) = name.next_sibling() {
        vertical_link(&keyboard, &enabled);
    }
    vertical_link(&enabled, &automatic);
    vertical_link(&automatic, &hours);
    vertical_link(&hours, &save);
    vertical_link(&save, &close);
    form_with_footer(&dialog, &body, &[save.clone(), close.clone()]);
    focus_when_present(&dialog, &name);
    close.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    let callback_parent = parent.clone();
    save.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        message,
        move |_| {
            let manager = service.clone();
            let id = source.id.clone();
            let id_for_focus = id.clone();
            let revision = source.updated_at;
            let new_name = name.text().to_string();
            let new_enabled = enabled.is_active();
            let auto = automatic.is_active();
            let interval = hours.value_as_int() as i64 * 3600;
            let list = list.clone();
            let status = status.clone();
            let parent = callback_parent.clone();
            spawn(glib::clone!(
                #[weak]
                dialog,
                #[weak]
                message,
                async move {
                    let result = manager
                        .update_settings(id, revision, new_name, new_enabled, auto, interval)
                        .await;
                    match result {
                        Ok(()) => {
                            dialog.close();
                            repaint_focused(manager, list, status, parent, Some((id_for_focus, 2)));
                        }
                        Err(error) => message.set_text(&guide_problem(&error.to_string())),
                    }
                }
            ));
        }
    ));
    dialog.present(Some(parent));
}

fn show_delete_confirmation(
    parent: &gtk::Window,
    service: GlobalGuideManager,
    source: GuideSource,
    list: gtk::Box,
    status: gtk::Label,
) {
    // Capture exactly the identity and revision displayed before confirmation.
    let approval = ConfirmedGuideDeletion::after_user_confirmation(&source);
    let dialog = adw::Dialog::new();
    title(&dialog, &gettext("Remove Programme Guide?"));
    let body = box_column();
    let message = gtk::Label::new(Some(&format!(
        "{}\n{}",
        source.name,
        gettext("Removing this guide also deletes its saved schedule, manual guide mappings and playlist guide associations. IPTV playlists and playback settings remain unchanged.")
    )));
    message.set_wrap(true);
    body.append(&message);
    let error = status_label();
    body.append(&error);
    let cancel = command(&gettext("Keep Guide"), &body);
    let remove = command(&gettext("Remove Permanently"), &body);
    remove.add_css_class("destructive-action");
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    focus_when_present(&dialog, &cancel);
    let callback_parent = parent.clone();
    remove.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        error,
        move |_| {
            let manager = service.clone();
            let approval = approval.clone();
            let removed_id = source.id.clone();
            let list = list.clone();
            let status = status.clone();
            let parent = callback_parent.clone();
            spawn(glib::clone!(
                #[weak]
                dialog,
                #[weak]
                error,
                async move {
                    let result = manager.delete_confirmed(approval).await;
                    match result {
                        Ok(()) => {
                            dialog.close();
                            repaint_focused(manager, list, status, parent, Some((removed_id, 0)));
                        }
                        Err(reason) => error.set_text(&guide_problem(&reason.to_string())),
                    }
                }
            ));
        }
    ));
    dialog.set_child(Some(&body));
    dialog.present(Some(parent));
}

fn show_add(
    parent: &gtk::Window,
    service: GlobalGuideManager,
    list: gtk::Box,
    status: gtk::Label,
    local: Option<gio::File>,
) {
    let dialog = adw::Dialog::new();
    title(&dialog, &gettext("Add Programme Guide"));
    let body = box_column();
    let name = edit_field(&gettext("Guide name"), "", &body);
    let url = if local.is_none() {
        Some(edit_field(&gettext("XMLTV HTTP(S) URL"), "", &body))
    } else {
        None
    };
    let lan = gtk::CheckButton::with_label(&gettext(
        "I explicitly trust this local-network guide origin",
    ));
    if local.is_none() {
        body.append(&lan);
    }
    let automatic = gtk::CheckButton::with_label(&gettext("Keep this guide updated automatically"));
    automatic.set_active(true);
    body.append(&automatic);
    let note = gtk::Label::new(Some(&gettext(
        "When enabled, Broadcaster checks this saved guide when it is older than 24 hours while the app is running. You can disable updates in Edit."
    )));
    note.set_wrap(true);
    body.append(&note);
    let error = status_label();
    body.append(&error);
    let save = command(&gettext("Save Guide"), &body);
    let cancel = command(&gettext("Cancel"), &body);
    if let Some(keyboard) = name.next_sibling() {
        if let Some(url) = url.as_ref() {
            vertical_link(&keyboard, url);
            if let Some(url_keyboard) = url.next_sibling() {
                vertical_link(&url_keyboard, &lan);
            }
            vertical_link(&lan, &automatic);
        } else {
            vertical_link(&keyboard, &automatic);
        }
    }
    vertical_link(&automatic, &save);
    vertical_link(&save, &cancel);
    form_with_footer(&dialog, &body, &[save.clone(), cancel.clone()]);
    focus_when_present(&dialog, &name);
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    let callback_parent = parent.clone();
    save.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        error,
        move |_| {
            let origin = if let Some(ref file) = local {
                SourceOrigin::selected_local_file(file.uri().to_string())
            } else {
                let chosen = if lan.is_active() {
                    TrustScope::ExplicitLan
                } else {
                    TrustScope::Public
                };
                SourceOrigin::entered_remote_url(
                    url.as_ref()
                        .map(|field| field.text().to_string())
                        .unwrap_or_default(),
                    chosen,
                )
            };
            let Ok(origin) = origin else {
                error.set_text(&gettext(
                    "Enter a valid HTTP(S) URL or select a local XMLTV file.",
                ));
                return;
            };
            let manager = service.clone();
            let list = list.clone();
            let status = status.clone();
            let parent = callback_parent.clone();
            let name = name.text().to_string();
            let update_automatically = automatic.is_active();
            spawn(glib::clone!(
                #[weak]
                dialog,
                #[weak]
                error,
                async move {
                    match manager
                        .add_with_schedule(name, origin, update_automatically)
                        .await
                    {
                        Ok(added) => {
                            dialog.close();
                            repaint_focused(manager, list, status, parent, Some((added.id, 2)));
                        }
                        Err(reason) => error.set_text(&guide_problem(&reason.to_string())),
                    }
                }
            ));
        }
    ));
    dialog.present(Some(parent));
}

pub async fn ensure_started() -> Result<GlobalGuideManager, super::service::GuideServiceError> {
    let cell = SHARED_MANAGER.get_or_init(tokio::sync::OnceCell::const_new);
    cell.get_or_try_init(GlobalGuideManager::open)
        .await
        .cloned()
}
pub fn show_guide_manager(parent: &gtk::Window) {
    show_guide_manager_with_close(parent, || ());
}

pub fn show_guide_manager_with_close(parent: &gtk::Window, on_close: impl Fn() + 'static) {
    let parent = parent.clone();
    spawn(async move {
        let service = match ensure_started().await {
            Ok(service) => service,
            Err(_) => {
                crate::utils::show_error("Unable to open programme guide configuration");
                return;
            }
        };
        let dialog = adw::Dialog::new();
        title(&dialog, &gettext("Programme Guides"));
        let body = box_column();
        let intro = gtk::Label::new(Some(&gettext(
            "One shared programme-guide registry for all IPTV playlists. Sources are never imported from M3U hints automatically."
        )));
        intro.set_wrap(true);
        body.append(&intro);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let add_remote = command(&gettext("Add URL"), &actions);
        let add_local = command(&gettext("Choose Local XMLTV"), &actions);
        body.append(&actions);
        let more_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let clear_default = command(&gettext("Clear Shared Default"), &more_actions);
        let close = command(&gettext("Close"), &more_actions);
        body.append(&more_actions);
        let scroller = guide_list_scroller();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        scroller.set_child(Some(&list));
        body.append(&scroller);
        let status = status_label();
        body.append(&status);
        dialog.add_css_class("broadcaster-guide-manager");
        focus_when_present(&dialog, &add_remote);
        let clear_win = parent.clone();
        let clear_manager = service.clone();
        let clear_list = list.clone();
        let clear_status = status.clone();
        clear_default.connect_clicked(move |_| {
            confirm_shared_guide(
                &clear_win,
                None,
                clear_manager.clone(),
                clear_list.clone(),
                clear_status.clone(),
            );
        });
        close.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        let svc = service.clone();
        let ls = list.clone();
        let st = status.clone();
        let win = parent.clone();
        add_remote
            .connect_clicked(move |_| show_add(&win, svc.clone(), ls.clone(), st.clone(), None));
        let svc = service.clone();
        let ls = list.clone();
        let st = status.clone();
        let win = parent.clone();
        add_local.connect_clicked(move |_| {
            let svc = svc.clone();
            let ls = ls.clone();
            let st = st.clone();
            let win = win.clone();
            spawn(async move {
                if let Ok(file) = gtk::FileDialog::new().open_future(Some(&win)).await {
                    show_add(&win, svc, ls, st, Some(file));
                }
            });
        });
        dialog.set_child(Some(&body));
        dialog.connect_closed(move |_| on_close());
        dialog.present(Some(&parent));
        repaint(service, list, status, parent);
    });
}

#[cfg(test)]
mod focus_tests {
    use super::*;

    // Opt-in: xvfb-run -a env GDK_BACKEND=x11 cargo test
    // --locked gtk_empty_guide_manager_directional_focus -- --ignored --test-threads=1
    #[test]
    #[ignore = "requires a mapped GTK display (Xvfb)"]
    fn gtk_empty_guide_manager_directional_focus() {
        gtk::init().expect("GTK display");
        let window = gtk::Window::new();
        let body = box_column();
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let add = command("Add URL", &actions);
        let close = command("Close", &actions);
        body.append(&actions);
        let scroller = guide_list_scroller();
        scroller.set_child(Some(&gtk::Label::new(Some("No guides configured"))));
        body.append(&scroller);
        window.set_child(Some(&body));
        window.present();
        let context = glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
        assert!(window.is_mapped());
        assert!(!scroller.is_focusable());
        assert!(close.grab_focus());
        let _ = body.child_focus(gtk::DirectionType::Down);
        assert!(!scroller.has_focus());
        assert!(
            gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|focused| focused.is_visible())
        );
        assert!(add.grab_focus());
        window.close();
    }
}

#[cfg(test)]
#[path = "desktop/c2_native_tests.rs"]
mod c2_native_tests;

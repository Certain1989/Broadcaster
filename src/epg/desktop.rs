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
};

use crate::utils::spawn;

static SHARED_MANAGER: OnceLock<tokio::sync::OnceCell<GlobalGuideManager>> = OnceLock::new();

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
    entry
}
fn repaint(service: GlobalGuideManager, list: gtk::Box, status: gtk::Label, parent: gtk::Window) {
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
                        row.append(&gtk::Label::new(Some(&format!(
                            "{}: {code}",
                            gettext("Last error")
                        ))));
                    }
                    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    let refresh = command(&gettext("Refresh"), &buttons);
                    refresh.set_sensitive(source.enabled);
                    let configure = command(&gettext("Edit"), &buttons);
                    let remove = command(&gettext("Remove"), &buttons);
                    row.append(&buttons);
                    list.append(&row);
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
                            let result = manager.refresh(id).await;
                            state.set_text(&match result {
                                Ok(_) => gettext("Guide refreshed"),
                                Err(error) => format!("{}: {error}", gettext("Refresh failed")),
                            });
                            repaint(manager, listing, state, window);
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
            }
            Err(error) => {
                status.set_text(&format!("{}: {error}", gettext("Unable to load guides")))
            }
        }
    });
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
    let automatic = gtk::CheckButton::with_label(&gettext("Automatic refresh (opt-in)"));
    automatic.set_active(source.auto_refresh);
    body.append(&automatic);
    let hours = gtk::SpinButton::with_range(6.0, 168.0, 1.0);
    hours.set_value(source.refresh_interval_seconds as f64 / 3600.0);
    body.append(&gtk::Label::new(Some(&gettext(
        "Refresh interval in hours (6–168)",
    ))));
    body.append(&hours);
    let message = status_label();
    body.append(&message);
    let save = command(&gettext("Apply"), &body);
    let close = command(&gettext("Close"), &body);
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
                            repaint(manager, list, status, parent);
                        }
                        Err(error) => message.set_text(&error.to_string()),
                    }
                }
            ));
        }
    ));
    dialog.set_child(Some(&body));
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
    let callback_parent = parent.clone();
    remove.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        error,
        move |_| {
            let manager = service.clone();
            let approval = approval.clone();
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
                            repaint(manager, list, status, parent);
                        }
                        Err(reason) => error.set_text(&reason.to_string()),
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
    let note = gtk::Label::new(Some(&gettext(
        "Adding a source saves the selected locator privately. No download occurs until you choose Refresh or explicitly enable a schedule."
    )));
    note.set_wrap(true);
    body.append(&note);
    let error = status_label();
    body.append(&error);
    let save = command(&gettext("Save Guide"), &body);
    let cancel = command(&gettext("Cancel"), &body);
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
                error.set_text(&gettext("Invalid guide location"));
                return;
            };
            let manager = service.clone();
            let list = list.clone();
            let status = status.clone();
            let parent = callback_parent.clone();
            let name = name.text().to_string();
            spawn(glib::clone!(
                #[weak]
                dialog,
                #[weak]
                error,
                async move {
                    match manager.add(name, origin).await {
                        Ok(_) => {
                            dialog.close();
                            repaint(manager, list, status, parent);
                        }
                        Err(reason) => error.set_text(&reason.to_string()),
                    }
                }
            ));
        }
    ));
    dialog.set_child(Some(&body));
    dialog.present(Some(parent));
}

pub async fn ensure_started() -> Result<GlobalGuideManager, super::service::GuideServiceError> {
    let cell = SHARED_MANAGER.get_or_init(tokio::sync::OnceCell::const_new);
    cell.get_or_try_init(GlobalGuideManager::open)
        .await
        .cloned()
}
pub fn show_guide_manager(parent: &gtk::Window) {
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
        let close = command(&gettext("Close"), &actions);
        body.append(&actions);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_vexpand(true);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        scroller.set_child(Some(&list));
        body.append(&scroller);
        let status = status_label();
        body.append(&status);
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
        dialog.present(Some(&parent));
        repaint(service, list, status, parent);
    });
}

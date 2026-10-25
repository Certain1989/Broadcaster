// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Editor of *effective* channel facets with imported/default/manual provenance.
//! The service persists one validated ChannelOverrides transaction.
use std::collections::BTreeSet;

use adw::{glib, gtk, prelude::*};
use gettextrs::gettext;

use crate::{
    streams::{
        facet_picker::FacetPicker,
        model::{
            ChannelDetail, ChannelOverrides, FacetDimension, FacetMode, FieldOrigin, GroupOverride,
            Region, RegionOverride, SetOverride,
        },
        view::TvStreamsView,
    },
    utils::spawn,
};

fn mode_picker(current: FacetMode) -> gtk::DropDown {
    let picker = gtk::DropDown::from_strings(&[
        &gettext("Automatic"),
        &gettext("My values"),
        &gettext("Unknown"),
    ]);
    picker.set_selected(match current {
        FacetMode::Auto => 0,
        FacetMode::Replace => 1,
        FacetMode::Unknown => 2,
    });
    picker
}

fn mode(picker: &gtk::DropDown) -> FacetMode {
    match picker.selected() {
        1 => FacetMode::Replace,
        2 => FacetMode::Unknown,
        _ => FacetMode::Auto,
    }
}

fn text(value: &str) -> gtk::Entry {
    let entry = gtk::Entry::new();
    entry.set_text(value);
    entry.set_hexpand(true);
    entry
}

fn editable_control(field: &gtk::Entry, body: &gtk::Box) {
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.append(field);
    let keyboard = gtk::Button::new();
    keyboard.set_icon_name("input-keyboard-symbolic");
    keyboard.set_size_request(48, 44);
    keyboard.set_valign(gtk::Align::Center);
    crate::keyboard::bind_launcher(
        &keyboard,
        field,
        crate::keyboard::KeyboardOptions::default(),
    );
    controls.append(&keyboard);
    body.append(&controls);

    // Entry Left/Right remain GTK caret keys. Down reveals its launcher;
    // Up from the launcher returns focus to that exact live entry.
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
}

fn label(value: &str, target: &gtk::Box) {
    let label = gtk::Label::new(Some(value));
    label.set_wrap(true);
    label.set_xalign(0.0);
    target.append(&label);
}

fn origin_text(origin: FieldOrigin) -> String {
    match origin {
        FieldOrigin::Manual => gettext("My values"),
        FieldOrigin::Imported => gettext("Imported"),
        FieldOrigin::SourceFallback => gettext("Source default"),
        FieldOrigin::Unknown => gettext("Unknown"),
    }
}

fn value_text(value: Option<&str>) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| gettext("Unknown"))
}

fn set_text(set: &BTreeSet<String>) -> String {
    if set.is_empty() {
        gettext("Unknown")
    } else {
        set.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn region_text(region: Option<&Region>) -> String {
    region.map_or_else(
        || gettext("Unknown"),
        |region| {
            let code = region.code.as_deref().unwrap_or("");
            let label = region.label.as_deref().unwrap_or("");
            if code.is_empty() {
                label.to_owned()
            } else if label.is_empty() {
                code.to_owned()
            } else {
                format!("{code} ({label})")
            }
        },
    )
}

fn values_detail(body: &gtk::Box, imported: String, default: Option<String>, effective: String) {
    label(&format!("{}: {}", gettext("Imported"), imported), body);
    if let Some(default) = default {
        label(&format!("{}: {}", gettext("Source default"), default), body);
    }
    label(&format!("{}: {}", gettext("Effective"), effective), body);
}

fn editor_row(
    box_: &gtk::Box,
    title: &str,
    origin: &str,
    picker: &gtk::DropDown,
    field: &gtk::Entry,
) {
    label(&format!("{title} · {origin}"), box_);
    box_.append(picker);
    editable_control(field, box_);
}

fn filled_region(code: &str, label: &str) -> Result<Region, &'static str> {
    let code = code.trim();
    let label = label.trim();
    let value = Region {
        code: (!code.is_empty()).then(|| code.to_owned()),
        label: (!label.is_empty()).then(|| label.to_owned()),
    };
    value
        .validate()
        .map_err(|_| "Invalid region code or label")?;
    Ok(value)
}

struct Fields {
    use_name: gtk::CheckButton,
    name: gtk::Entry,
    name_error: gtk::Label,
    group_error: gtk::Label,
    region_error: gtk::Label,
    group_mode: gtk::DropDown,
    group: gtk::Entry,
    language_mode: gtk::DropDown,
    languages: FacetPicker,
    country_mode: gtk::DropDown,
    countries: FacetPicker,
    region_mode: gtk::DropDown,
    region_code: gtk::Entry,
    region_label: gtk::Entry,
}

impl Fields {
    fn new(detail: &ChannelDetail, body: &gtk::Box) -> Self {
        label(
            &format!(
                "{}: {}",
                gettext("Imported name"),
                detail.imported.imported_name
            ),
            body,
        );
        label(
            &format!(
                "{}: {}",
                gettext("Name origin"),
                origin_text(detail.provenance.name)
            ),
            body,
        );
        let use_name = gtk::CheckButton::with_label(&gettext("Use my display name"));
        use_name.set_active(detail.overrides.display_name.is_some());
        body.append(&use_name);
        let name = text(
            detail
                .overrides
                .display_name
                .as_deref()
                .unwrap_or(&detail.effective.visible_name),
        );
        editable_control(&name, body);
        let name_error = gtk::Label::new(None);
        name_error.set_xalign(0.0);
        name_error.set_wrap(true);
        name_error.add_css_class("error");
        body.append(&name_error);
        let group_mode = mode_picker(detail.overrides.group.mode);
        let group = text(
            detail
                .overrides
                .group
                .value
                .as_deref()
                .or(detail.effective.group.as_deref())
                .unwrap_or(""),
        );
        editor_row(
            body,
            &gettext("Group"),
            &origin_text(detail.provenance.group),
            &group_mode,
            &group,
        );
        let group_error = gtk::Label::new(None);
        group_error.set_xalign(0.0);
        group_error.set_wrap(true);
        group_error.add_css_class("error");
        body.append(&group_error);
        values_detail(
            body,
            value_text(detail.imported.group_title.as_deref()),
            None,
            value_text(detail.effective.group.as_deref()),
        );
        let language_mode = mode_picker(detail.overrides.languages.mode);
        let initial_languages = if detail.overrides.languages.mode == FacetMode::Replace {
            &detail.overrides.languages.values
        } else {
            &detail.effective.languages
        };
        let languages = FacetPicker::new(FacetDimension::Language, initial_languages);
        label(
            &format!(
                "{} · {}",
                gettext("Languages"),
                origin_text(detail.provenance.languages)
            ),
            body,
        );
        body.append(&language_mode);
        body.append(&languages.root);
        languages
            .root
            .set_sensitive(mode(&language_mode) == FacetMode::Replace);
        let language_root = languages.root.clone();
        language_mode.connect_selected_notify(move |picker| {
            language_root.set_sensitive(mode(picker) == FacetMode::Replace);
        });
        values_detail(
            body,
            set_text(&detail.imported.languages),
            Some(set_text(&detail.source_defaults.languages)),
            set_text(&detail.effective.languages),
        );

        let country_mode = mode_picker(detail.overrides.countries.mode);
        let initial_countries = if detail.overrides.countries.mode == FacetMode::Replace {
            &detail.overrides.countries.values
        } else {
            &detail.effective.countries
        };
        let countries = FacetPicker::new(FacetDimension::Country, initial_countries);
        label(
            &format!(
                "{} · {}",
                gettext("Countries"),
                origin_text(detail.provenance.countries)
            ),
            body,
        );
        body.append(&country_mode);
        body.append(&countries.root);
        countries
            .root
            .set_sensitive(mode(&country_mode) == FacetMode::Replace);
        let country_root = countries.root.clone();
        country_mode.connect_selected_notify(move |picker| {
            country_root.set_sensitive(mode(picker) == FacetMode::Replace);
        });
        values_detail(
            body,
            set_text(&detail.imported.countries),
            Some(set_text(&detail.source_defaults.countries)),
            set_text(&detail.effective.countries),
        );
        let region_mode = mode_picker(detail.overrides.region.mode);
        let region = detail
            .overrides
            .region
            .value
            .as_ref()
            .or(detail.effective.region.as_ref());
        let region_code = text(region.and_then(|r| r.code.as_deref()).unwrap_or(""));
        editor_row(
            body,
            &gettext("Region code"),
            &origin_text(detail.provenance.region),
            &region_mode,
            &region_code,
        );
        label(&gettext("Region label"), body);
        let region_label = text(region.and_then(|r| r.label.as_deref()).unwrap_or(""));
        editable_control(&region_label, body);
        let region_error = gtk::Label::new(None);
        region_error.set_xalign(0.0);
        region_error.set_wrap(true);
        region_error.add_css_class("error");
        body.append(&region_error);
        values_detail(
            body,
            region_text(detail.imported.region.as_ref()),
            Some(region_text(detail.source_defaults.region.as_ref())),
            region_text(detail.effective.region.as_ref()),
        );
        Self {
            use_name,
            name,
            name_error,
            group_error,
            region_error,
            group_mode,
            group,
            language_mode,
            languages,
            country_mode,
            countries,
            region_mode,
            region_code,
            region_label,
        }
    }

    fn edits(&self) -> Result<ChannelOverrides, String> {
        self.name_error.set_text("");
        self.group_error.set_text("");
        self.region_error.set_text("");
        let display_name = if self.use_name.is_active() {
            let name = self.name.text().trim().to_owned();
            if name.is_empty() || name.len() > 512 {
                let reason = gettext("Enter a valid display name (1–512 bytes).");
                self.name_error.set_text(&reason);
                self.name.grab_focus();
                return Err(reason);
            }
            Some(name)
        } else {
            None
        };
        let group_mode = mode(&self.group_mode);
        let group = if group_mode == FacetMode::Replace {
            let value = self.group.text().trim().to_owned();
            if value.is_empty() || value.len() > 256 {
                let reason = gettext("Enter a valid group name (1–256 bytes).");
                self.group_error.set_text(&reason);
                self.group.grab_focus();
                return Err(reason);
            }
            Some(value)
        } else {
            None
        };
        let language_mode = mode(&self.language_mode);
        let languages = if language_mode == FacetMode::Replace {
            self.languages.values(true)?
        } else {
            BTreeSet::new()
        };
        let country_mode = mode(&self.country_mode);
        let countries = if country_mode == FacetMode::Replace {
            self.countries.values(true)?
        } else {
            BTreeSet::new()
        };
        let region_mode = mode(&self.region_mode);
        let region = if region_mode == FacetMode::Replace {
            match filled_region(&self.region_code.text(), &self.region_label.text()) {
                Ok(value) => Some(value),
                Err(_) => {
                    let reason =
                        gettext("Enter a valid region name or code, e.g. Bavaria / DE-BY.");
                    self.region_error.set_text(&reason);
                    if self.region_label.text().len() > 128 {
                        self.region_label.grab_focus();
                    } else {
                        self.region_code.grab_focus();
                    }
                    return Err(reason);
                }
            }
        } else {
            None
        };
        let value = ChannelOverrides {
            display_name,
            group: GroupOverride {
                mode: group_mode,
                value: group,
            },
            region: RegionOverride {
                mode: region_mode,
                value: region,
            },
            languages: SetOverride {
                mode: language_mode,
                values: languages,
            },
            countries: SetOverride {
                mode: country_mode,
                values: countries,
            },
        };
        value
            .validate()
            .map_err(|_| gettext("Invalid channel override"))?;
        Ok(value)
    }
}

pub(crate) fn show_metadata_editor(view: &TvStreamsView, id: String) {
    let (Some(parent), Some(service)) = (view.root().and_downcast::<gtk::Window>(), view.service())
    else {
        return;
    };
    spawn(glib::clone!(
        #[weak]
        view,
        async move {
            let detail = match service.channel_detail(&id).await {
                Ok(detail) => detail,
                Err(_) => {
                    return;
                }
            };
            let dialog = adw::Dialog::new();
            dialog.set_title(&gettext("Edit Channel Metadata"));
            dialog.set_content_width(560);
            dialog.set_content_height(670);
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
            body.set_margin_top(16);
            body.set_margin_bottom(16);
            body.set_margin_start(16);
            body.set_margin_end(16);
            let fields = Fields::new(&detail, &body);
            let errors = gtk::Label::new(None);
            errors.set_wrap(true);
            errors.set_xalign(0.0);
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            actions.set_halign(gtk::Align::End);
            let cancel = gtk::Button::with_label(&gettext("Cancel"));
            let reset = gtk::Button::with_label(&gettext("Reset to Automatic"));
            let save = gtk::Button::with_label(&gettext("Save"));
            save.add_css_class("suggested-action");
            actions.append(&cancel);
            actions.append(&reset);
            actions.append(&save);
            body.append(&errors);
            body.append(&actions);
            scroll.set_child(Some(&body));
            dialog.set_child(Some(&scroll));
            cancel.connect_clicked(glib::clone!(
                #[weak]
                dialog,
                move |_| {
                    dialog.close();
                }
            ));
            let reset_service = service.clone();
            let reset_id = id.clone();
            let save_service = service.clone();
            let save_id = id.clone();
            reset.connect_clicked(glib::clone!(
                #[weak]
                view,
                #[weak]
                dialog,
                #[weak]
                errors,
                #[weak]
                reset,
                move |_| {
                    reset.set_sensitive(false);
                    let service = reset_service.clone();
                    let id = reset_id.clone();
                    spawn(glib::clone!(
                        #[weak]
                        view,
                        #[weak]
                        dialog,
                        #[weak]
                        errors,
                        #[weak]
                        reset,
                        async move {
                            match service.clear_overrides(&id).await {
                                Ok(()) => {
                                    dialog.close();
                                    view.reload();
                                }
                                Err(error) => {
                                    errors.set_text(&error.to_string());
                                    reset.set_sensitive(true);
                                }
                            }
                        }
                    ));
                }
            ));
            save.connect_clicked(glib::clone!(
                #[weak]
                view,
                #[weak]
                dialog,
                #[weak]
                errors,
                #[weak]
                save,
                move |_| {
                    let value = match fields.edits() {
                        Ok(value) => value,
                        Err(error) => {
                            errors.set_text(&error);
                            return;
                        }
                    };
                    save.set_sensitive(false);
                    let service = save_service.clone();
                    let id = save_id.clone();
                    spawn(glib::clone!(
                        #[weak]
                        view,
                        #[weak]
                        dialog,
                        #[weak]
                        errors,
                        #[weak]
                        save,
                        async move {
                            match service.set_overrides(&id, value).await {
                                Ok(()) => {
                                    dialog.close();
                                    view.reload();
                                }
                                Err(error) => {
                                    errors.set_text(&error.to_string());
                                    save.set_sensitive(true);
                                }
                            }
                        }
                    ));
                }
            ));
            dialog.present(Some(&parent));
        }
    ));
}

#[derive(Clone, Copy)]
enum MappingAction {
    Row,
    ExactAlias,
    PlaylistException,
    ClearPlaylistException,
    ClearRow,
}

async fn save_epg_mapping(
    action: MappingAction,
    row: String,
    playlist: String,
    source: String,
    target: String,
) -> Result<(), &'static str> {
    crate::utils::tokio(async move {
        tokio::task::spawn_blocking(move || {
            use crate::epg::bindings::{
                clear_row_mapping, set_playlist_exception, set_row_mapping,
                ConfirmedGuideSelection, ConfirmedRowMapping,
            };
            let mut conn = super::storage::Storage::new()
                .connect()
                .map_err(|_| "Cannot open programme data")?;
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |t| t.as_secs() as i64);
            let result = match action {
                MappingAction::Row => set_row_mapping(
                    &mut conn,
                    ConfirmedRowMapping::after_user_confirmation_for_this_stream(),
                    &row,
                    &source,
                    &target,
                    at,
                ),
                MappingAction::ExactAlias => {
                    // The accepted E3 API rereads the imported ID within its
                    // transaction but cannot compare it to the confirmed ID.
                    // Do not allow a playlist refresh to expand consent to B.
                    return Err("All-stream mapping needs an atomic imported-ID check");
                }
                MappingAction::PlaylistException => set_playlist_exception(
                    &mut conn,
                    ConfirmedGuideSelection::after_user_confirmation(),
                    &playlist,
                    Some(&source),
                    at,
                ),
                MappingAction::ClearPlaylistException => set_playlist_exception(
                    &mut conn,
                    ConfirmedGuideSelection::after_user_confirmation(),
                    &playlist,
                    None,
                    at,
                ),
                MappingAction::ClearRow => clear_row_mapping(
                    &mut conn,
                    ConfirmedRowMapping::after_user_confirmation_for_this_stream(),
                    &row,
                ),
            };
            result.map_err(|_| "Guide target missing or selection changed")
        })
        .await
        .map_err(|_| "Programme mapping worker unavailable")?
    })
    .await
}

fn confirm_epg_mapping(
    view: &TvStreamsView,
    parent: &gtk::Window,
    action: MappingAction,
    row: String,
    playlist: String,
    source: String,
    target: String,
    imported_tvg: Option<String>,
    source_name: String,
) {
    let dialog = adw::Dialog::new();
    dialog.set_title(&gettext("Confirm Programme Mapping"));
    dialog.set_content_width(460);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);
    label(&match action {
        MappingAction::Row => gettext("Confirm mapping for THIS stream only?"),
        MappingAction::ExactAlias => gettext("Confirm mapping for ALL streams with this exact case-sensitive tvg-id across playlists selecting this guide?"),
        MappingAction::PlaylistException => gettext("Make this guide an explicit exception for all channels of this playlist?"),
        MappingAction::ClearPlaylistException => gettext("Remove this playlist exception and return to the shared guide default?"),
        MappingAction::ClearRow => gettext("Clear only this stream's saved guide mapping?"),
    }, &body);
    if matches!(action, MappingAction::ExactAlias) {
        // Literal strings, never markup. This is the exact scope a future
        // atomic E3 API must compare under the same write transaction.
        label(
            &format!(
                "{}: {}\n{}: {}\n{}: {}",
                gettext("Imported exact tvg-id (case-sensitive)"),
                imported_tvg.as_deref().unwrap_or(""),
                gettext("Selected programme guide"),
                source_name,
                gettext("XMLTV channel ID"),
                target
            ),
            &body,
        );
    }
    let error = gtk::Label::new(None);
    if matches!(action, MappingAction::ExactAlias) {
        error.set_text(&gettext(
            "All-stream confirmation is temporarily unavailable until the imported ID can be checked atomically at save time."
        ));
    }
    body.append(&error);
    let cancel = gtk::Button::with_label(&gettext("Cancel"));
    let approve = gtk::Button::with_label(&match action {
        MappingAction::Row => gettext("Confirm This Stream Only"),
        MappingAction::ExactAlias => gettext("Confirm All Matching Streams"),
        MappingAction::PlaylistException => gettext("Confirm Playlist Exception"),
        MappingAction::ClearPlaylistException | MappingAction::ClearRow => {
            gettext("Confirm Removal")
        }
    });
    approve.add_css_class("suggested-action");
    // E4 cannot safely authorize accepted E3's non-comparing alias API.
    approve.set_sensitive(!matches!(action, MappingAction::ExactAlias));
    body.append(&cancel);
    body.append(&approve);
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    approve.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        error,
        #[weak]
        approve,
        #[weak]
        view,
        move |_| {
            approve.set_sensitive(false);
            let row = row.clone();
            let playlist = playlist.clone();
            let source = source.clone();
            let target = target.clone();
            spawn(glib::clone!(
                #[weak]
                dialog,
                #[weak]
                error,
                #[weak]
                approve,
                #[weak]
                view,
                async move {
                    match save_epg_mapping(action, row, playlist, source, target).await {
                        Ok(()) => {
                            dialog.close();
                            view.epg_changed();
                        }
                        Err(reason) => {
                            error.set_text(&gettext(reason));
                            approve.set_sensitive(true);
                        }
                    }
                }
            ));
        }
    ));
    dialog.set_child(Some(&body));
    dialog.present(Some(parent));
}

pub(crate) fn show_epg_mapping_editor(view: &TvStreamsView, id: String) {
    let (Some(window), Some(streams)) = (view.root().and_downcast::<gtk::Window>(), view.service())
    else {
        return;
    };
    spawn(glib::clone!(
        #[weak]
        view,
        async move {
            let Ok(detail) = streams.channel_detail(&id).await else {
                return;
            };
            let Ok(manager) = crate::epg::desktop::ensure_started().await else {
                return;
            };
            let Ok(guides) = manager.list().await else {
                return;
            };
            let dialog = adw::Dialog::new();
            dialog.set_title(&gettext("Programme Mapping"));
            dialog.set_content_width(530);
            let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
            body.set_margin_top(16);
            body.set_margin_bottom(16);
            body.set_margin_start(16);
            body.set_margin_end(16);
            label(&gettext("Select an existing guide and enter the exact XMLTV channel ID. Name suggestions never create a mapping."), &body);
            let names: Vec<String> = guides
                .iter()
                .map(|g| {
                    if g.enabled {
                        g.name.clone()
                    } else {
                        format!("{} ({})", g.name, gettext("disabled"))
                    }
                })
                .collect();
            let choices: Vec<&str> = names.iter().map(String::as_str).collect();
            let picker = gtk::DropDown::from_strings(&choices);
            picker.set_sensitive(!guides.is_empty());
            body.append(&picker);
            label(&gettext("Exact XMLTV channel ID (case-sensitive)"), &body);
            let channel = text("");
            editable_control(&channel, &body);
            let error = gtk::Label::new(None);
            error.set_xalign(0.0);
            error.set_wrap(true);
            body.append(&error);
            let single = gtk::Button::with_label(&gettext("Use for this stream"));
            let shared = gtk::Button::with_label(&gettext(
                "Review all-stream exact-ID mapping (unavailable)",
            ));
            single.set_sensitive(!guides.is_empty());
            shared.set_sensitive(
                !guides.is_empty()
                    && detail
                        .imported
                        .tvg_id
                        .as_deref()
                        .is_some_and(|s| !s.is_empty()),
            );
            label(&gettext("All-stream mapping remains disabled pending an atomic imported-ID safety check."), &body);
            let playlist = gtk::Button::with_label(&gettext("Use as playlist guide exception"));
            let clear_playlist =
                gtk::Button::with_label(&gettext("Remove playlist guide exception"));
            let clear_row =
                gtk::Button::with_label(&gettext("Remove this stream's manual mapping"));
            let close = gtk::Button::with_label(&gettext("Close"));
            playlist.set_sensitive(!guides.is_empty());
            for control in [
                &single,
                &shared,
                &playlist,
                &clear_playlist,
                &clear_row,
                &close,
            ] {
                control.set_halign(gtk::Align::Start);
                body.append(control);
            }
            close.connect_clicked(glib::clone!(
                #[weak]
                dialog,
                move |_| {
                    dialog.close();
                }
            ));
            for (button, action) in [
                (single, MappingAction::Row),
                (shared, MappingAction::ExactAlias),
                (playlist, MappingAction::PlaylistException),
                (clear_playlist, MappingAction::ClearPlaylistException),
                (clear_row, MappingAction::ClearRow),
            ] {
                let guides = guides.clone();
                let picker = picker.clone();
                let channel = channel.clone();
                let error = error.clone();
                let window = window.clone();
                let stable_id = detail.effective.id.clone();
                let playlist_id = detail.effective.source_id.clone();
                let imported_tvg = detail.imported.tvg_id.clone();
                button.connect_clicked(glib::clone!(
                    #[weak]
                    view,
                    move |_| {
                        let selected_guide = guides.get(picker.selected() as usize);
                        if selected_guide.is_none()
                            && !matches!(
                                action,
                                MappingAction::ClearPlaylistException | MappingAction::ClearRow
                            )
                        {
                            return;
                        }
                        let id = channel.text().to_string();
                        if matches!(action, MappingAction::Row | MappingAction::ExactAlias)
                            && (id.is_empty()
                                || id.len() > 512
                                || id.trim() != id
                                || id.chars().any(char::is_control))
                        {
                            error.set_text(&gettext("Enter a valid exact XMLTV channel ID"));
                            return;
                        }
                        confirm_epg_mapping(
                            &view,
                            &window,
                            action,
                            stable_id.clone(),
                            playlist_id.clone(),
                            selected_guide.map_or_else(String::new, |guide| guide.id.clone()),
                            id,
                            imported_tvg.clone(),
                            selected_guide.map_or_else(String::new, |guide| guide.name.clone()),
                        );
                    }
                ));
            }
            dialog.set_child(Some(&body));
            dialog.present(Some(&window));
        }
    ));
}

#[cfg(test)]
pub(crate) fn assert_synthetic_editor_form() {
    use self::model_for_test::*;

    let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let detail = sample_detail();
    let fields = Fields::new(&detail, &body);
    assert!(!fields.edits().expect("automatic defaults").has_values());

    fields.use_name.set_active(true);
    fields.name.set_text("Custom channel");
    fields.group_mode.set_selected(1);
    fields.group.set_text("News");
    fields.language_mode.set_selected(1);
    fields.languages.choose("de");
    fields.languages.choose("en");
    fields
        .languages
        .enter_manual("de-DE")
        .expect("advanced language tag");
    assert!(fields.languages.enter_manual("German").is_err());
    fields.country_mode.set_selected(2);
    fields.region_mode.set_selected(2);
    let edited = fields.edits().expect("valid explicit overrides");
    assert_eq!(edited.display_name.as_deref(), Some("Custom channel"));
    assert_eq!(edited.group.value.as_deref(), Some("News"));
    assert_eq!(edited.languages.values.len(), 3);
    assert!(edited.languages.values.contains("de-DE"));
    assert_eq!(edited.countries.mode, FacetMode::Unknown);
    assert!(edited.countries.values.is_empty());
    assert_eq!(edited.region.mode, FacetMode::Unknown);

    fields.country_mode.set_selected(1);
    assert!(
        fields.edits().is_err(),
        "empty My values countries must be rejected"
    );
    fields.countries.choose("DE");
    fields.countries.choose("AT");
    let selected = fields.edits().expect("two country overrides");
    assert_eq!(selected.countries.values.len(), 2);
    assert!(selected.countries.values.contains("AT"));
}

#[cfg(test)]
mod model_for_test {
    use super::*;
    use crate::streams::model::{
        ChannelEffective, EditProvenance, FieldOrigin, IncomingChannel, SourceDefaults,
    };

    pub(super) fn sample_detail() -> ChannelDetail {
        let imported = IncomingChannel {
            order: 0,
            imported_name: "Synthetic Channel".into(),
            stream_url: "https://example.invalid/synthetic.m3u8".into(),
            tvg_id: None,
            tvg_name: None,
            group_title: Some("Sport".into()),
            logo_url: None,
            channel_number: None,
            region: None,
            languages: BTreeSet::new(),
            countries: BTreeSet::new(),
            user_agent: None,
            referer: None,
        };
        let effective = ChannelEffective {
            id: "fabricated-channel".into(),
            source_id: "fabricated-source".into(),
            source_name: "Fabricated".into(),
            visible_name: imported.imported_name.clone(),
            stream_url: imported.stream_url.clone(),
            group: imported.group_title.clone(),
            languages: BTreeSet::new(),
            countries: BTreeSet::new(),
            region: None,
            logo_url: None,
            channel_number: None,
            has_user_overrides: false,
        };
        ChannelDetail {
            effective,
            imported,
            source_defaults: SourceDefaults::default(),
            overrides: ChannelOverrides::default(),
            provenance: EditProvenance {
                name: FieldOrigin::Imported,
                group: FieldOrigin::Imported,
                languages: FieldOrigin::Unknown,
                countries: FieldOrigin::Unknown,
                region: FieldOrigin::Unknown,
            },
        }
    }
}

#[cfg(test)]
mod editor_tests {
    use super::*;

    #[test]
    fn codes_and_regions_normalize_before_saving() {
        assert_eq!(
            crate::streams::model::normalize_facet("de-CH", FacetDimension::Language).unwrap(),
            "de-CH"
        );
        assert_eq!(
            crate::streams::model::normalize_facet("at", FacetDimension::Country).unwrap(),
            "AT"
        );
        assert!(crate::streams::model::normalize_facet("de-DE", FacetDimension::Country).is_err());
        assert_eq!(
            filled_region("  DE-BY ", " Bavaria ")
                .unwrap()
                .code
                .as_deref(),
            Some("DE-BY")
        );
        assert!(filled_region(" ", " ").is_err());
        assert!(filled_region(&"x".repeat(65), "Label").is_err());
    }
}

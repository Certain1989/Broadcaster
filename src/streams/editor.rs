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
            // The 670px desktop sheet used to exceed the actual 640x480
            // AdwBreakpointBin height and clip remote-only actions. Keep
            // the same editable fields and confirmation semantics, with a
            // bounded compact sheet and a scrollable X/Y content viewport.
            let compact = parent.width() <= 700;
            dialog.set_content_width(if compact { 480 } else { 560 });
            dialog.set_content_height(if compact { 350 } else { 670 });
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_policy(
                if compact {
                    gtk::PolicyType::Automatic
                } else {
                    gtk::PolicyType::Never
                },
                gtk::PolicyType::Automatic,
            );
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
    SuggestedRow,
    ExactAlias,
    PlaylistException,
    ClearPlaylistException,
    ClearRow,
}

/// Immutable, selected-guide review snapshot provenance. Never persisted as
/// a global or automatic rule; a changed row/guide/target blocks confirmation.
#[derive(Clone)]
struct HintPrefill {
    row_id: String,
    playlist_id: String,
    original_tvg_id: Option<String>,
    effective_name: String,
    guide_id: String,
    full_xmltv_id: String,
    guide_last_success_at: i64,
    guide_revision: i64,
}

async fn effective_row_guide(row_id: String) -> Result<Option<String>, &'static str> {
    crate::utils::tokio(async move {
        tokio::task::spawn_blocking(move || {
            use crate::epg::bindings::selected_guide_for_row;
            let conn = super::storage::Storage::new()
                .connect()
                .map_err(|_| "Cannot open programme data")?;
            selected_guide_for_row(&conn, &row_id)
                .map_err(|_| "Cannot check selected programme guide")
        })
        .await
        .map_err(|_| "Programme mapping worker unavailable")?
    })
    .await
}

fn guide_health(guide: &crate::epg::sources::GuideSource) -> String {
    if !guide.enabled {
        return gettext("Guide disabled — enable and refresh it before reviewing matches");
    }
    let Some(imported) = guide.last_success_at else {
        return gettext("No successful import — refresh the guide before reviewing matches");
    };
    let time = crate::utils::format_timestamp_full(imported)
        .map_or_else(|| gettext("unknown time"), |v| v.to_string());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let age = if imported > now {
        gettext("import time in future; verify clock")
    } else if now - imported > 259_200 {
        gettext("older than three days; may be stale")
    } else {
        gettext("imported, but current programme coverage is not guaranteed")
    };
    format!("{}: {} — {}", gettext("Last successful import"), time, age)
}

async fn save_epg_mapping(
    action: MappingAction,
    row: String,
    playlist: String,
    source: String,
    target: String,
    expected_tvg_id: Option<String>,
    hint: Option<HintPrefill>,
) -> Result<(), &'static str> {
    crate::utils::tokio(async move {
        tokio::task::spawn_blocking(move || {
            use crate::epg::bindings::{
                clear_row_mapping, set_global_alias_if_imported_id_matches,
                set_playlist_exception, set_row_mapping, set_row_mapping_from_hint_if_unchanged,
                ConfirmedGlobalMapping,
                ConfirmedGuideSelection, ConfirmedRowMapping, HintMappingSnapshot, BindingError,
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
                MappingAction::SuggestedRow => hint
                    .as_ref()
                    .filter(|h| h.row_id == row && h.playlist_id == playlist
                        && h.guide_id == source && h.full_xmltv_id == target)
                    .ok_or(BindingError::InvalidInput)
                    .and_then(|h| {
                        set_row_mapping_from_hint_if_unchanged(
                            &mut conn,
                            ConfirmedRowMapping::after_user_confirmation_for_this_stream(),
                            HintMappingSnapshot {
                                row_id: &row,
                                playlist_id: &h.playlist_id,
                                tvg_id: h.original_tvg_id.as_deref(),
                                effective_name: &h.effective_name,
                                guide_source_id: &source,
                                channel_id: &target,
                                last_success_at: h.guide_last_success_at,
                                guide_revision: h.guide_revision,
                            },
                            at,
                        )
                    }),
                MappingAction::ExactAlias => expected_tvg_id
                    .as_deref()
                    .ok_or(BindingError::InvalidInput)
                    .and_then(|expected| {
                        set_global_alias_if_imported_id_matches(
                            &mut conn,
                            ConfirmedGlobalMapping::after_user_confirmation_for_all_matching_streams(),
                            &row,
                            expected,
                            &source,
                            &target,
                            at,
                        )
                    }),
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
            result.map_err(|_| {
                if matches!(action, MappingAction::ExactAlias | MappingAction::SuggestedRow) {
                    "Imported tvg-id or guide changed. Reopen mapping and reconfirm."
                } else {
                    "Guide target missing or selection changed"
                }
            })
        })
        .await
        .map_err(|_| "Programme mapping worker unavailable")?
    })
    .await
}

struct MappingChoice {
    row: String,
    playlist: String,
    source: String,
    target: String,
    imported_tvg: Option<String>,
    source_name: String,
    hint: Option<HintPrefill>,
}

fn confirm_epg_mapping(
    view: &TvStreamsView,
    parent: &gtk::Window,
    action: MappingAction,
    choice: MappingChoice,
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
        MappingAction::SuggestedRow => gettext("Name resemblance is NOT schedule evidence. Confirm this exact XMLTV ID for THIS stream only?"),
        MappingAction::ExactAlias => gettext("Confirm mapping for ALL streams with this exact case-sensitive tvg-id across playlists selecting this guide?"),
        MappingAction::PlaylistException => gettext("Make this guide an explicit exception for all channels of this playlist?"),
        MappingAction::ClearPlaylistException => gettext("Remove this playlist exception and return to the shared guide default?"),
        MappingAction::ClearRow => gettext("Clear only this stream's saved guide mapping?"),
    }, &body);
    if matches!(action, MappingAction::SuggestedRow) {
        label(
            &format!(
                "{}: {}\n{}: {}\n{}: {}\n{}: {}",
                gettext("Playlist channel"),
                choice
                    .hint
                    .as_ref()
                    .map_or("", |h| h.effective_name.as_str()),
                gettext("Original imported tvg-id"),
                choice
                    .imported_tvg
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .unwrap_or("(none)"),
                gettext("Guide"),
                choice.source_name,
                gettext("Full XMLTV ID (case-sensitive)"),
                choice.target,
            ),
            &body,
        );
    }
    if matches!(action, MappingAction::ExactAlias) {
        // Literal strings, never markup. This is the exact scope a future
        // atomic E3 API must compare under the same write transaction.
        label(
            &format!(
                "{}: {}\n{}: {}\n{}: {}",
                gettext("Imported exact tvg-id (case-sensitive)"),
                choice.imported_tvg.as_deref().unwrap_or(""),
                gettext("Selected programme guide"),
                choice.source_name,
                gettext("XMLTV channel ID"),
                choice.target
            ),
            &body,
        );
    }
    let error = gtk::Label::new(None);
    body.append(&error);
    let cancel = gtk::Button::with_label(&gettext("Cancel"));
    let approve = gtk::Button::with_label(&match action {
        MappingAction::Row | MappingAction::SuggestedRow => gettext("Confirm This Stream Only"),
        MappingAction::ExactAlias => gettext("Confirm All Matching Streams"),
        MappingAction::PlaylistException => gettext("Confirm Playlist Exception"),
        MappingAction::ClearPlaylistException | MappingAction::ClearRow => {
            gettext("Confirm Removal")
        }
    });
    approve.add_css_class("suggested-action");
    // The confirmed literal imported ID is rechecked within the SQLite
    // write transaction. Changed IDs fail closed and require new consent.
    approve.set_sensitive(
        !matches!(action, MappingAction::ExactAlias)
            || choice
                .imported_tvg
                .as_deref()
                .is_some_and(|id| !id.is_empty()),
    );
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
            let row = choice.row.clone();
            let playlist = choice.playlist.clone();
            let source = choice.source.clone();
            let target = choice.target.clone();
            let expected_tvg = choice.imported_tvg.clone();
            let hint = choice.hint.clone();
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
                    match save_epg_mapping(
                        action,
                        row,
                        playlist,
                        source,
                        target,
                        expected_tvg,
                        hint,
                    )
                    .await
                    {
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

// The native GTK regression drives the *actual* picker and database. These
// test-only controls expose its modal and hold one completed async load so a
// source swap can be injected before presentation; no production shortcut.
#[cfg(test)]
thread_local! {
    static C4_PICKER_DIALOG: std::cell::RefCell<Option<glib::WeakRef<adw::Dialog>>> =
        const { std::cell::RefCell::new(None) };
    static C4_PICKER_PAUSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static C4_PICKER_LOADED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(test)]
pub(crate) fn c4_test_picker_dialog() -> Option<adw::Dialog> {
    C4_PICKER_DIALOG.with(|slot| slot.borrow().as_ref().and_then(glib::WeakRef::upgrade))
}
#[cfg(test)]
pub(crate) fn c4_test_pause_picker(paused: bool) {
    C4_PICKER_PAUSED.with(|state| state.set(paused));
    if paused {
        C4_PICKER_LOADED.with(|state| state.set(false));
    }
}
#[cfg(test)]
pub(crate) fn c4_test_picker_loaded() -> bool {
    C4_PICKER_LOADED.with(std::cell::Cell::get)
}

// D35 compact on-card mapper. The fuller manager is retained for the other
// explicit guide management workflows; the card action uses only this picker.
#[derive(Clone)]
struct CardMappingData {
    snapshot: crate::epg::bindings::PickerMappingSnapshot,
    guide_name: String,
    existing: String,
    suggestions: Vec<(String, String)>,
}

async fn load_card_mapping_data(row_id: String) -> Result<CardMappingData, &'static str> {
    crate::utils::tokio(async move {
        tokio::task::spawn_blocking(move || {
            use crate::epg::{bindings::selected_guide_for_row, matching};
            use rusqlite::{params, OptionalExtension};
            let mut conn = super::storage::Storage::new().connect().map_err(|_| "Cannot open programme data")?;
            // Hold a single consistent SQLite read snapshot across row,
            // effective guide, source revision, overrides and catalogue.
            let tx = conn.transaction().map_err(|_| "Cannot read programme data")?;
            let row: Option<(String, Option<String>, String, i64)> = tx.query_row(
                "SELECT c.source_id,c.tvg_id,COALESCE(o.display_name_override,c.imported_name),s.revision
                 FROM iptv_channels c JOIN iptv_sources s ON s.id=c.source_id
                 LEFT JOIN iptv_channel_overrides o ON o.channel_id=c.id
                 WHERE c.id=?1 AND c.is_present=1 AND s.enabled=1",
                [&row_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            ).optional().map_err(|_| "Channel no longer exists")?;
            let Some((playlist_id, tvg_id, effective_name, playlist_revision)) = row else {
                return Err("Channel no longer exists");
            };
            let guide_id = selected_guide_for_row(&tx, &row_id)
                .map_err(|_| "Cannot read selected guide")?
                .ok_or("No programme guide selected; configure a guide in Programme Guides")?;
            let source: Option<(String, i64, Option<i64>, i64)> = tx.query_row(
                "SELECT name,enabled,last_success_at,updated_at FROM epg_sources WHERE id=?1",
                [&guide_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            ).optional().map_err(|_| "Cannot read selected guide")?;
            let Some((guide_name, enabled, imported, guide_revision)) = source else {
                return Err("Selected guide is missing");
            };
            if enabled != 1 || imported.is_none() {
                return Err("Selected guide is disabled or not yet imported");
            }
            let imported = imported.expect("checked imported");
            let prior_row_target: Option<String> = tx.query_row(
                "SELECT guide_channel_id FROM epg_channel_overrides WHERE iptv_channel_id=?1",
                [&row_id], |r| r.get(0),
            ).optional().map_err(|_| "Cannot read existing mapping")?;
            let prior_alias_target: Option<String> = tx.query_row(
                "SELECT guide_channel_id FROM epg_global_id_aliases
                 WHERE guide_source_id=?1 AND iptv_tvg_id=?2",
                params![guide_id, tvg_id], |r| r.get(0),
            ).optional().map_err(|_| "Cannot read existing mapping")?;

            // Entire selected-guide channel METADATA only; never programme
            // bodies, provider endpoints, streaming URLs or credentials.
            const MAX_ROWS: usize = 100_000;
            const MAX_META_BYTES: usize = 48 * 1024 * 1024;
            let mut stmt = tx.prepare(
                "SELECT channel_id,display_name FROM epg_channels
                 WHERE source_id=?1 ORDER BY channel_id LIMIT 100001",
            ).map_err(|_| "Programme catalogue unavailable")?;
            let mut rows = stmt.query([&guide_id]).map_err(|_| "Programme catalogue unavailable")?;
            let mut channels = Vec::new();
            let mut bytes = 0usize;
            while let Some(row) = rows.next().map_err(|_| "Programme catalogue unavailable")? {
                if channels.len() >= MAX_ROWS { return Err("Programme catalogue exceeds mapping limit"); }
                let id: String = row.get(0).map_err(|_| "Programme catalogue unavailable")?;
                let name: String = row.get(1).map_err(|_| "Programme catalogue unavailable")?;
                bytes = bytes.saturating_add(id.len()).saturating_add(name.len());
                if bytes > MAX_META_BYTES { return Err("Programme catalogue metadata exceeds mapping limit"); }
                channels.push(crate::epg::model::GuideChannel {
                    source_id: guide_id.clone(), channel_id: id, display_name: name,
                });
            }
            drop(rows);
            drop(stmt);
            tx.commit().map_err(|_| "Cannot finish programme snapshot")?;
            let existing = if let Some(target) = prior_row_target.as_ref() {
                format!("Manual target for this stream: {target}")
            } else if let Some(target) = prior_alias_target.as_ref() {
                format!("Confirmed shared ID alias: {target}")
            } else if let Some(target) = tvg_id.as_ref().filter(|id| channels.iter().any(|ch| &ch.channel_id == *id)) {
                format!("Literal guide ID: {target}")
            } else if let Some(target) = tvg_id.as_deref()
                .and_then(|id| matching::automatic_identity_target(id, &guide_id, &channels)) {
                format!("Automatically proven identity: {target}")
            } else { "No confirmed match".to_owned() };

            // Same-broadcaster gate BEFORE relevance. Names alone are not
            // station identity and are never allowed to suggest another brand.
            let original = tvg_id.as_deref().unwrap_or("");
            let plausible = matching::automatic_identity_candidates(original);
            let mut suggestions: Vec<(String, String)> = Vec::new();
            for (candidate, kind) in plausible {
                if let Some(ch) = channels.iter().find(|c| c.channel_id == candidate
                    && matching::corroborated_auto_identity(original, kind, c)) {
                    suggestions.push((candidate, format!("{} — exact identity ({kind:?})", ch.display_name)));
                }
            }
            // Additional human-only exact-name suggestions MUST have a literal
            // broadcaster-brand gate in the ID and exact canonical name; no
            // Levenshtein or cross-region generic closeness is accepted.
            if let Some(brand) = original.split_once('.').map(|(brand, _)| brand)
                .filter(|brand| (3..=24).contains(&brand.len())
                    && brand.bytes().all(|b| b.is_ascii_alphanumeric())) {
                let name_key = matching::normalized_name(&effective_name);
                for ch in &channels {
                    if suggestions.len() >= 5 { break; }
                    let guidelabel = ch.display_name
                        .split_once(" - ").map_or(ch.display_name.as_str(), |(_, label)| label);
                    let guide_key = matching::normalized_name(guidelabel);
                    if ch.channel_id.split_once('.').is_some_and(|(targetbrand, _)|
                        targetbrand.eq_ignore_ascii_case(brand))
                        && guide_key == name_key
                        && !suggestions.iter().any(|s| s.0 == ch.channel_id) {
                        suggestions.push((ch.channel_id.clone(),
                            format!("{} — same broadcaster and exact displayed name (review variant)",ch.display_name)));
                    }
                }
            }
            suggestions.truncate(5);
            Ok(CardMappingData {
                snapshot: crate::epg::bindings::PickerMappingSnapshot {
                    row_id, playlist_id, tvg_id, effective_name, playlist_revision,
                    guide_source_id: guide_id, guide_last_success_at: imported, guide_revision,
                    prior_row_target, prior_alias_target,
                },
                guide_name, existing, suggestions,
            })
        }).await.map_err(|_| "Programme picker worker unavailable")?
    }).await
}

async fn search_card_guide(
    guide: String,
    typed: String,
    page: u32,
) -> Result<Vec<(String, String)>, &'static str> {
    crate::utils::tokio(async move {
        tokio::task::spawn_blocking(move || {
            use rusqlite::params;
            if typed.len() > 128 || typed.chars().any(char::is_control) || page > 5000 {
                return Err("Search input too long");
            }
            let conn = super::storage::Storage::new()
                .connect()
                .map_err(|_| "Cannot open programme data")?;
            let mut stmt = conn
                .prepare(
                    "SELECT channel_id,display_name FROM epg_channels
                 WHERE source_id=?1 AND (instr(lower(channel_id),lower(?2))>0
                     OR instr(lower(display_name),lower(?2))>0)
                 ORDER BY channel_id LIMIT 31 OFFSET ?3",
                )
                .map_err(|_| "Cannot search guide channels")?;
            let results = stmt
                .query_map(params![guide, typed, page.saturating_mul(30)], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|_| "Cannot search guide channels")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|_| "Cannot search guide channels")?;
            Ok(results)
        })
        .await
        .map_err(|_| "Programme search worker unavailable")?
    })
    .await
}

fn card_picker_apply(
    view: &TvStreamsView,
    dialog: &adw::Dialog,
    status: &gtk::Label,
    snap: &crate::epg::bindings::PickerMappingSnapshot,
    exact_target: String,
) {
    let expected = snap.clone();
    status.set_text(&gettext("Saving exact stream mapping…"));
    spawn(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        #[weak]
        status,
        async move {
            let result: Result<(), &'static str> = crate::utils::tokio(async move {
                tokio::task::spawn_blocking(move || {
                    let mut conn = super::storage::Storage::new().connect()
                        .map_err(|_| "Cannot open programme data")?;
                    let at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs() as i64);
                    crate::epg::bindings::set_row_mapping_from_picker_if_unchanged(
                        &mut conn,
                        crate::epg::bindings::ConfirmedRowMapping::after_user_confirmation_for_this_stream(),
                        &expected, &exact_target, at,
                    ).map_err(|_| "Channel, guide or binding changed; reopen mapping")
                }).await.map_err(|_| "Programme save worker unavailable")?
            }).await;
            match result {
                Ok(()) => {
                    dialog.close();
                    view.epg_changed();
                }
                Err(err) => status.set_text(&gettext(err)),
            }
        }
    ));
}

struct CardSearchUi {
    results: gtk::Box,
    previous: gtk::Button,
    next: gtk::Button,
    page: std::rc::Rc<std::cell::Cell<u32>>,
    serial: std::rc::Rc<std::cell::Cell<u64>>,
}
impl CardSearchUi {
    fn new(
        results: &gtk::Box,
        previous: &gtk::Button,
        next: &gtk::Button,
        page: &std::rc::Rc<std::cell::Cell<u32>>,
        serial: &std::rc::Rc<std::cell::Cell<u64>>,
    ) -> Self {
        Self {
            results: results.clone(),
            previous: previous.clone(),
            next: next.clone(),
            page: page.clone(),
            serial: serial.clone(),
        }
    }
}

fn refresh_card_guide_search(
    view: &TvStreamsView,
    dialog: &adw::Dialog,
    status: &gtk::Label,
    snapshot: &crate::epg::bindings::PickerMappingSnapshot,
    entry: &gtk::Entry,
    ui: &CardSearchUi,
) {
    let results = &ui.results;
    let previous = &ui.previous;
    let next = &ui.next;
    let page = &ui.page;
    let serial = &ui.serial;
    let generation = serial.get().wrapping_add(1);
    serial.set(generation);
    let current_page = page.get();
    let guide = snapshot.guide_source_id.clone();
    let query = entry.text().to_string();
    let snapshot = snapshot.clone();
    status.set_text(&gettext("Searching selected guide…"));
    spawn(glib::clone!(
        #[weak]
        view,
        #[weak]
        dialog,
        #[weak]
        status,
        #[weak]
        results,
        #[weak]
        previous,
        #[weak]
        next,
        #[strong]
        serial,
        async move {
            let found = search_card_guide(guide, query, current_page).await;
            if serial.get() != generation {
                return;
            }
            while let Some(first) = results.first_child() {
                results.remove(&first);
            }
            match found {
                Ok(mut channels) => {
                    let more = channels.len() > 30;
                    channels.truncate(30);
                    previous.set_sensitive(current_page > 0);
                    next.set_sensitive(more);
                    for (id, name) in channels {
                        let button = gtk::Button::with_label(&format!("{name} — [{id}]"));
                        button.set_halign(gtk::Align::Fill);
                        let snap = snapshot.clone();
                        button.connect_clicked(glib::clone!(
                            #[weak]
                            view,
                            #[weak]
                            dialog,
                            #[weak]
                            status,
                            move |_| card_picker_apply(&view, &dialog, &status, &snap, id.clone())
                        ));
                        results.append(&button);
                    }
                    status.set_text(&format!(
                        "{} / {}",
                        gettext("Guide search page"),
                        current_page + 1
                    ));
                }
                Err(err) => status.set_text(&gettext(err)),
            }
        }
    ));
}

// Check the actual catalogue and row again after an asynchronous picker
// snapshot was loaded, before presenting a modal. The view-generation guard
// below catches GTK rebinds; this cheap SQLite read catches independent Guide
// Manager deletion/selection and source refresh between load and presentation.
// Confirmation still uses the original immediate-transaction CAS; this is a
// visibility/return-focus guard, not permission to weaken the write gate.
async fn card_picker_snapshot_presentable(
    expected: crate::epg::bindings::PickerMappingSnapshot,
) -> bool {
    crate::utils::tokio(async move {
        tokio::task::spawn_blocking(move || {
            use rusqlite::OptionalExtension;
            let Ok(mut conn) = super::storage::Storage::new().connect() else {
                return false;
            };
            let Ok(tx) = conn.transaction() else {
                return false;
            };
            let row: Option<(String, i64, i64, i64)> = match tx
                .query_row(
                    "SELECT c.source_id,s.revision,c.is_present,s.enabled
                 FROM iptv_channels c JOIN iptv_sources s ON s.id=c.source_id
                 WHERE c.id=?1",
                    [&expected.row_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()
            {
                Ok(row) => row,
                Err(_) => return false,
            };
            let Some((source, revision, present, enabled)) = row else {
                return false;
            };
            if source != expected.playlist_id
                || revision != expected.playlist_revision
                || present != 1
                || enabled != 1
            {
                return false;
            }
            let Ok(selected) = crate::epg::bindings::selected_guide_for_row(&tx, &expected.row_id)
            else {
                return false;
            };
            if selected.as_deref() != Some(expected.guide_source_id.as_str()) {
                return false;
            }
            let guide: Option<(i64, i64, Option<i64>)> = match tx
                .query_row(
                    "SELECT enabled,updated_at,last_success_at FROM epg_sources WHERE id=?1",
                    [&expected.guide_source_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
            {
                Ok(guide) => guide,
                Err(_) => return false,
            };
            matches!(guide, Some((1, revision, Some(imported)))
                if revision == expected.guide_revision
                    && imported == expected.guide_last_success_at)
        })
        .await
        .unwrap_or(false)
    })
    .await
}

pub(crate) fn show_card_mapping_picker(view: &TvStreamsView, row_id: String) {
    let Some(parent) = view.root().and_downcast::<gtk::Window>() else {
        return;
    };
    // Never present data loaded for a card that has since been recycled,
    // collapsed, filtered out or replaced by another playlist's selection.
    let Some((generation, source_id)) = view.card_mapping_context(&row_id) else {
        return;
    };
    spawn(glib::clone!(
        #[weak]
        view,
        async move {
            let data = match load_card_mapping_data(row_id.clone()).await {
                Ok(data) => data,
                Err(reason) => {
                    // No selected/ready guide is not a mapping dialog. Opening
                    // a modal just to report it steals D-pad focus from the
                    // original card. Keep focus and point to guide management.
                    if let Some(overlay) = view
                        .ancestor(adw::ToastOverlay::static_type())
                        .and_downcast::<adw::ToastOverlay>()
                    {
                        overlay.add_toast(adw::Toast::new(&gettext(reason)));
                    }
                    return;
                }
            };
            #[cfg(test)]
            {
                C4_PICKER_LOADED.with(|state| state.set(true));
                while C4_PICKER_PAUSED.with(std::cell::Cell::get) {
                    glib::timeout_future(std::time::Duration::from_millis(10)).await;
                }
            }
            if data.snapshot.playlist_id != source_id
                || view.card_mapping_context(&row_id) != Some((generation, source_id.clone()))
                || !card_picker_snapshot_presentable(data.snapshot.clone()).await
                || view.card_mapping_context(&row_id) != Some((generation, source_id))
            {
                return;
            }
            let dialog = adw::Dialog::new();
            dialog.set_title(&gettext("Programme mapping"));
            dialog.set_content_width(520);
            let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
            body.set_margin_top(12);
            body.set_margin_bottom(12);
            body.set_margin_start(12);
            body.set_margin_end(12);
            label(
                &format!(
                    "{}: {}",
                    gettext("Selected programme guide"),
                    data.guide_name
                ),
                &body,
            );
            label(&data.existing, &body);
            label(&gettext("Choose an exact identity for THIS stream; regional, language and time-shift schedules may differ."), &body);
            let status = gtk::Label::new(None);
            status.set_xalign(0.0);
            status.set_wrap(true);
            for (exact, why) in &data.suggestions {
                let button = gtk::Button::with_label(&format!("{why} — [{exact}]"));
                button.set_halign(gtk::Align::Fill);
                let target = exact.clone();
                let snapshot = data.snapshot.clone();
                button.connect_clicked(glib::clone!(
                    #[weak]
                    view,
                    #[weak]
                    dialog,
                    #[weak]
                    status,
                    move |_| card_picker_apply(&view, &dialog, &status, &snapshot, target.clone())
                ));
                body.append(&button);
            }
            if data.suggestions.is_empty() {
                label(&gettext("No safe, same-broadcaster suggestions. Use exact search if you know the intended channel."), &body);
            }
            let search_toggle =
                gtk::Button::with_label(&gettext("Search all guide channels / choose exact ID…"));
            search_toggle.set_halign(gtk::Align::Start);
            // The transient guide picker is outside the My Channels toolbar
            // focus styles. Keep a visible cue on its real advanced action.
            search_toggle.add_css_class("tv-epg-picker-focus");
            body.append(&search_toggle);
            let advanced = gtk::Revealer::new();
            advanced.set_reveal_child(false);
            let search_area = gtk::Box::new(gtk::Orientation::Vertical, 6);
            let entry = text("");
            entry.set_placeholder_text(Some(&gettext("Exact XMLTV ID or station name")));
            editable_control(&entry, &search_area);
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_min_content_height(200);
            scroll.set_max_content_height(240);
            let results = gtk::Box::new(gtk::Orientation::Vertical, 4);
            scroll.set_child(Some(&results));
            search_area.append(&scroll);
            let pager = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let prev = gtk::Button::with_label(&gettext("Previous"));
            let next = gtk::Button::with_label(&gettext("Next"));
            prev.set_sensitive(false);
            next.set_sensitive(false);
            pager.append(&prev);
            pager.append(&next);
            search_area.append(&pager);
            advanced.set_child(Some(&search_area));
            body.append(&advanced);
            // The compact D35 row picker deliberately only confirms a single
            // exact channel identity. Route explicit playlist-guide overrides
            // to the pre-existing, separately confirmed full mapping editor;
            // never change a playlist binding merely by opening this control.
            let guide_settings =
                gtk::Button::with_label(&gettext("Change guide / playlist exception…"));
            guide_settings.set_halign(gtk::Align::Start);
            guide_settings.add_css_class("tv-epg-picker-focus");
            let editor_row = row_id.clone();
            let editor_context = (generation, data.snapshot.playlist_id.clone());
            guide_settings.connect_clicked(glib::clone!(
                #[weak]
                view,
                #[weak]
                dialog,
                move |_| {
                    // The picker is transient. A refresh or list rebind may
                    // already have invalidated its original source identity.
                    if view.card_mapping_context(&editor_row) == Some(editor_context.clone()) {
                        dialog.close();
                        show_epg_mapping_editor(&view, editor_row.clone());
                    } else {
                        dialog.close();
                    }
                }
            ));
            body.append(&guide_settings);
            body.append(&status);
            let close = gtk::Button::with_label(&gettext("Cancel"));
            close.connect_clicked(glib::clone!(
                #[weak]
                dialog,
                move |_| {
                    dialog.close();
                }
            ));
            body.append(&close);
            let snapshot = data.snapshot.clone();
            let current_page = std::rc::Rc::new(std::cell::Cell::new(0_u32));
            let generation = std::rc::Rc::new(std::cell::Cell::new(0_u64));
            search_toggle.connect_clicked(glib::clone!(
                #[weak]
                view,
                #[weak]
                dialog,
                #[weak]
                status,
                #[weak]
                entry,
                #[weak]
                results,
                #[weak]
                prev,
                #[weak]
                next,
                #[weak]
                advanced,
                #[strong]
                current_page,
                #[strong]
                generation,
                #[strong]
                snapshot,
                move |_| {
                    advanced.set_reveal_child(true);
                    entry.grab_focus();
                    refresh_card_guide_search(
                        &view,
                        &dialog,
                        &status,
                        &snapshot,
                        &entry,
                        &CardSearchUi::new(&results, &prev, &next, &current_page, &generation),
                    );
                }
            ));
            entry.connect_changed(glib::clone!(
                #[weak]
                view,
                #[weak]
                dialog,
                #[weak]
                status,
                #[weak]
                results,
                #[weak]
                prev,
                #[weak]
                next,
                #[weak]
                advanced,
                #[strong]
                current_page,
                #[strong]
                generation,
                #[strong]
                snapshot,
                move |entry| {
                    current_page.set(0);
                    if advanced.reveals_child() {
                        refresh_card_guide_search(
                            &view,
                            &dialog,
                            &status,
                            &snapshot,
                            entry,
                            &CardSearchUi::new(&results, &prev, &next, &current_page, &generation),
                        );
                    }
                }
            ));
            next.connect_clicked(glib::clone!(
                #[weak]
                view,
                #[weak]
                dialog,
                #[weak]
                status,
                #[weak]
                entry,
                #[weak]
                results,
                #[weak]
                prev,
                #[strong]
                current_page,
                #[strong]
                generation,
                #[strong]
                snapshot,
                move |next| {
                    current_page.set(current_page.get().saturating_add(1));
                    refresh_card_guide_search(
                        &view,
                        &dialog,
                        &status,
                        &snapshot,
                        &entry,
                        &CardSearchUi::new(&results, &prev, next, &current_page, &generation),
                    );
                }
            ));
            prev.connect_clicked(glib::clone!(
                #[weak]
                view,
                #[weak]
                dialog,
                #[weak]
                status,
                #[weak]
                entry,
                #[weak]
                results,
                #[weak]
                next,
                #[strong]
                current_page,
                #[strong]
                generation,
                #[strong]
                snapshot,
                move |prev| {
                    current_page.set(current_page.get().saturating_sub(1));
                    refresh_card_guide_search(
                        &view,
                        &dialog,
                        &status,
                        &snapshot,
                        &entry,
                        &CardSearchUi::new(&results, prev, &next, &current_page, &generation),
                    );
                }
            ));
            let restore_id = row_id.clone();
            dialog.connect_closed(glib::clone!(
                #[weak]
                view,
                move |_| {
                    #[cfg(test)]
                    C4_PICKER_DIALOG.with(|slot| slot.replace(None));
                    view.restore_card_mapping_focus(&restore_id);
                }
            ));
            // Bound the entire expanded search sheet on compact displays.
            // The inner paged result scroller alone does not limit its shell.
            if parent.width() <= 700 {
                let scroll = gtk::ScrolledWindow::new();
                scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
                scroll.set_min_content_height(180);
                scroll.set_max_content_height(340);
                scroll.set_min_content_width(240);
                scroll.set_max_content_width(430);
                scroll.set_child(Some(&body));
                dialog.set_content_width(480);
                dialog.set_content_height(350);
                dialog.set_child(Some(&scroll));
            } else {
                dialog.set_child(Some(&body));
            }
            dialog.present(Some(&parent));
            #[cfg(test)]
            C4_PICKER_DIALOG.with(|slot| slot.replace(Some(dialog.downgrade())));
        }
    ));
}

pub(crate) fn show_epg_mapping_editor(view: &TvStreamsView, id: String) {
    show_epg_mapping_editor_with_hint(view, id, None);
}

fn show_epg_mapping_editor_with_hint(view: &TvStreamsView, id: String, hint: Option<HintPrefill>) {
    let (Some(window), Some(streams)) = (view.root().and_downcast::<gtk::Window>(), view.service())
    else {
        return;
    };
    // The normal card route is generation/playlist-bound. Reject async
    // results for a recycled row before exposing guide-changing controls.
    // Bulk-review hints have their own separately checked identity snapshot.
    let card_context = hint
        .is_none()
        .then(|| view.card_mapping_context(&id))
        .flatten();
    // An already recycled/filtered card has no stable generation/source.
    // Do not let its async detail lookup reopen a guide-changing modal.
    // Bulk-review hints use their separate transaction-checked snapshot.
    if hint.is_none() && card_context.is_none() {
        return;
    }
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
            // Name-hint shortcuts must still refer to the very same stream.
            // The transactional confirmation independently repeats this check.
            if hint.as_ref().is_some_and(|h| {
                h.row_id != detail.effective.id
                    || h.playlist_id != detail.effective.source_id
                    || h.original_tvg_id != detail.imported.tvg_id
                    || h.effective_name != detail.effective.visible_name
            }) {
                return;
            }
            let effective_guide = effective_row_guide(detail.effective.id.clone()).await;
            if card_context
                .as_ref()
                .is_some_and(|expected| view.card_mapping_context(&id).as_ref() != Some(expected))
            {
                return;
            }
            let dialog = adw::Dialog::new();
            dialog.set_title(&gettext("Programme Mapping"));
            dialog.add_css_class("broadcaster-full-mapping-editor");
            // Keep the existing multi-action editor reachable at compact TV
            // resolutions. Its long form must scroll rather than requesting
            // a floating sheet taller or wider than the physical viewport.
            dialog.set_content_width(452);
            let restore_row = detail.effective.id.clone();
            dialog.connect_closed(glib::clone!(
                #[weak]
                view,
                move |_| view.restore_card_mapping_focus(&restore_row)
            ));
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
            // Default to the stream's actual row > playlist > shared guide,
            // not arbitrarily to the first guide in a list.
            let selected_id = hint
                .as_ref()
                .map(|h| h.guide_id.as_str())
                .or_else(|| effective_guide.as_ref().ok().and_then(Option::as_deref));
            if hint.is_some() && !guides.iter().any(|g| Some(g.id.as_str()) == selected_id) {
                return;
            }
            if let Some(idx) = selected_id.and_then(|id| guides.iter().position(|g| g.id == id)) {
                picker.set_selected(idx as u32);
            }
            let selection_explanation = match &effective_guide {
                Ok(Some(id)) => {
                    let name = guides.iter().find(|g| &g.id == id)
                        .map_or_else(|| gettext("missing guide"), |g| g.name.clone());
                    format!("{}: {}", gettext("Effective guide for this stream"), name)
                }
                Ok(None) => gettext("No effective guide selected. Choose one explicitly; otherwise the EPG cannot match this stream."),
                Err(_) => gettext("Effective guide could not be checked. Reopen mapping after checking programme data."),
            };
            label(&selection_explanation, &body);
            body.append(&picker);
            let health = gtk::Label::new(None);
            health.set_xalign(0.0);
            health.set_wrap(true);
            if let Some(guide) = guides.get(picker.selected() as usize) {
                health.set_text(&guide_health(guide));
            }
            body.append(&health);
            picker.connect_selected_notify({
                let guides = guides.clone();
                let health = health.clone();
                move |picker| {
                    if let Some(guide) = guides.get(picker.selected() as usize) {
                        health.set_text(&guide_health(guide));
                    }
                }
            });
            if hint.is_some() {
                picker.set_sensitive(false);
                label(&gettext("Suggested from a unique display name only. Check schedules, regional feeds and language before confirming. No global alias will be created."), &body);
            }
            label(&gettext("Exact XMLTV channel ID (case-sensitive)"), &body);
            let channel = text(hint.as_ref().map_or("", |h| h.full_xmltv_id.as_str()));
            editable_control(&channel, &body);
            let error = gtk::Label::new(None);
            error.set_xalign(0.0);
            error.set_wrap(true);
            body.append(&error);
            let single = gtk::Button::with_label(&gettext("Use for this stream"));
            let shared =
                gtk::Button::with_label(&gettext("Use for all streams with this exact tvg-id"));
            single.set_sensitive(!guides.is_empty());
            shared.set_sensitive(
                hint.is_none()
                    && !guides.is_empty()
                    && detail
                        .imported
                        .tvg_id
                        .as_deref()
                        .is_some_and(|s| !s.is_empty()),
            );
            let playlist = gtk::Button::with_label(&gettext("Use as playlist guide exception"));
            let clear_playlist =
                gtk::Button::with_label(&gettext("Remove playlist guide exception"));
            let clear_row =
                gtk::Button::with_label(&gettext("Remove this stream's manual mapping"));
            let close = gtk::Button::with_label(&gettext("Close"));
            let bulk = gtk::Button::with_label(&gettext("Review possible mappings in this guide"));
            bulk.set_sensitive(hint.is_none() && !guides.is_empty());
            bulk.set_halign(gtk::Align::Start);
            body.append(&bulk);
            bulk.connect_clicked(glib::clone!(
                #[weak]
                view,
                #[strong]
                guides,
                #[strong]
                picker,
                #[strong]
                window,
                move |_| {
                    if let Some(guide) = guides.get(picker.selected() as usize) {
                        show_epg_bulk_review(&view, &window, guide.id.clone());
                    }
                }
            ));
            playlist.set_sensitive(hint.is_none() && !guides.is_empty());
            if hint.is_some() {
                clear_playlist.set_sensitive(false);
                clear_row.set_sensitive(false);
            }
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
                (
                    single,
                    if hint.is_some() {
                        MappingAction::SuggestedRow
                    } else {
                        MappingAction::Row
                    },
                ),
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
                let hint = hint.clone();
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
                        if matches!(
                            action,
                            MappingAction::Row
                                | MappingAction::SuggestedRow
                                | MappingAction::ExactAlias
                        ) && (id.is_empty()
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
                            MappingChoice {
                                row: stable_id.clone(),
                                playlist: playlist_id.clone(),
                                source: selected_guide
                                    .map_or_else(String::new, |guide| guide.id.clone()),
                                target: id,
                                imported_tvg: imported_tvg.clone(),
                                source_name: selected_guide
                                    .map_or_else(String::new, |guide| guide.name.clone()),
                                hint: hint.clone(),
                            },
                        );
                    }
                ));
            }
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
            scroll.set_min_content_height(180);
            scroll.set_max_content_height(390);
            scroll.set_child(Some(&body));
            dialog.set_child(Some(&scroll));
            // The approved full mapping editor is now entered from a
            // D-pad-only compact picker. Begin inside its actual selected
            // guide rather than leaving focus on the closed picker behind
            // the newly presented sheet. Do not focus until it is mapped.
            dialog.connect_map(glib::clone!(
                #[weak]
                picker,
                move |_| {
                    picker.grab_focus();
                }
            ));
            dialog.present(Some(&window));
            // libadwaita may move initial focus once the sheet presentation
            // settles, after GtkWidget::map. Reassert only while this editor
            // and its guide chooser remain mapped; later user navigation is
            // never overridden by periodic or delayed retries.
            let initial_dialog = dialog.clone();
            let initial_picker = picker.clone();
            glib::idle_add_local_once(move || {
                if initial_dialog.is_mapped() && initial_picker.is_mapped() {
                    initial_picker.grab_focus();
                }
            });
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
            is_favourite: false,
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

// M1's candidate assistant never mutates an inferred mapping directly. It
// exposes at most one small page of controls, uses background SQLite snapshots,
// and requires a second explicit impact confirmation before the atomic write.
const GUIDE_REVIEW_PAGE_SIZE: usize = 16;
const GUIDE_REVIEW_MAX_SELECTIONS: usize = 4_096;

/// Do not queue an unbounded number of whole-guide snapshots. A rejected job
/// is preferable to starving GTK, SQLite imports or the playback pipeline.
fn guide_work_permit() -> Result<tokio::sync::OwnedSemaphorePermit, &'static str> {
    static GATE: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| "Two guide review tasks are already active")
}

fn review_reason(reason: crate::epg::candidates::CandidateReason) -> &'static str {
    use crate::epg::candidates::{CandidateReason, FeedClass};
    match reason {
        CandidateReason::Feed(FeedClass::Quality) => "Quality variant — review only",
        CandidateReason::Feed(FeedClass::Region) => "Regional schedule — verify individually",
        CandidateReason::Feed(FeedClass::Language) => "Language variant — verify individually",
        CandidateReason::Feed(FeedClass::TimeShift) => "Time-shifted feed — verify individually",
        CandidateReason::Feed(FeedClass::Provider) => "Distribution/provider — verify individually",
        CandidateReason::Feed(FeedClass::Unknown) => "Unknown feed — verify individually",
        CandidateReason::NameOnly => "Name similarity is not an alias identity",
    }
}

fn populate_guide_review_page(
    review: &crate::epg::candidates::GuideReview,
    page: usize,
    rows: &gtk::Box,
    navigation: &gtk::Label,
    selected: &std::rc::Rc<std::cell::RefCell<BTreeSet<String>>>,
) {
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    let count = review.proposals.len();
    let start = page.saturating_mul(GUIDE_REVIEW_PAGE_SIZE).min(count);
    let end = (start + GUIDE_REVIEW_PAGE_SIZE).min(count);
    navigation.set_text(&format!(
        "{} {}–{} / {} • {} {}",
        gettext("Review rows"),
        start.saturating_add(usize::from(count != 0)),
        end,
        count,
        selected.borrow().len(),
        gettext("selected")
    ));
    for item in &review.proposals[start..end] {
        let original = item.candidate.original_tvg_id.clone();
        let text = format!(
            "{} → {}\n{} • {}: {} • {}: {}",
            original,
            item.candidate.full_xmltv_id,
            gettext(review_reason(item.candidate.reason)),
            gettext("Current rows"),
            item.affected_rows,
            gettext("Manual exclusions"),
            item.already_bound_rows
        );
        let choice = gtk::CheckButton::with_label(&text);
        choice.set_tooltip_text(Some(&gettext(
            "Not automatically mapped. Confirmation creates a guide-scoped alias for the original case-sensitive ID, including future same-ID streams.",
        )));
        choice.set_active(selected.borrow().contains(&original));
        let selected = selected.clone();
        choice.connect_toggled(move |control| {
            if control.is_active() {
                if selected.borrow().len() >= GUIDE_REVIEW_MAX_SELECTIONS {
                    control.set_active(false);
                    return;
                }
                selected.borrow_mut().insert(original.clone());
            } else {
                selected.borrow_mut().remove(&original);
            }
        });
        choice.set_halign(gtk::Align::Fill);
        rows.append(&choice);
    }
}

/// Never creates check buttons or alias choices: a name resemblance is only
/// an explanation to inspect, not evidence of an identical broadcast schedule.
fn populate_read_only_name_hints_page(
    review: &crate::epg::candidates::GuideReview,
    guide_revision: i64,
    page: usize,
    rows: &gtk::Box,
    navigation: &gtk::Label,
    view: &TvStreamsView,
) {
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    let count = review.name_only_hints.len();
    let start = page.saturating_mul(GUIDE_REVIEW_PAGE_SIZE).min(count);
    let end = (start + GUIDE_REVIEW_PAGE_SIZE).min(count);
    navigation.set_text(&format!(
        "{} {}–{} / {}{} · {}",
        gettext("Read-only name hints"),
        start.saturating_add(usize::from(count != 0)),
        end,
        count,
        if review.name_only_truncated {
            gettext(" (additional hints not loaded; narrow the playlist)")
        } else {
            String::new()
        },
        gettext("Choose one hint to inspect, then confirm only that stream")
    ));
    for hint in &review.name_only_hints[start..end] {
        let button = gtk::Button::with_label(&format!(
            "{}: {} · {}: {}\n{}: {} · {}: {}\n{}: {} · {}",
            gettext("Original M3U tvg-id"),
            hint.original_tvg_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .unwrap_or("(none)"),
            gettext("Playlist channel name"),
            hint.imported_name,
            gettext("Complete XMLTV ID"),
            hint.full_xmltv_id,
            gettext("XMLTV channel name"),
            hint.guide_display_name,
            gettext("Selected guide"),
            review.guide_source_id,
            gettext("Inspect exact target for THIS stream only")
        ));
        button.set_halign(gtk::Align::Fill);
        button.set_can_focus(true);
        let prefill = HintPrefill {
            row_id: hint.row_id.clone(),
            playlist_id: hint.playlist_source_id.clone(),
            original_tvg_id: hint.original_tvg_id.clone(),
            effective_name: hint.imported_name.clone(),
            guide_id: review.guide_source_id.clone(),
            full_xmltv_id: hint.full_xmltv_id.clone(),
            guide_last_success_at: review.last_success_at,
            guide_revision,
        };
        button.connect_clicked(glib::clone!(
            #[weak]
            view,
            move |_| {
                show_epg_mapping_editor_with_hint(
                    &view,
                    prefill.row_id.clone(),
                    Some(prefill.clone()),
                );
            }
        ));
        rows.append(&button);
    }
}

async fn compute_guide_review(
    guide: String,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(crate::epg::candidates::GuideReview, i64), &'static str> {
    let permit = guide_work_permit()?;
    crate::utils::tokio(async move {
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut conn = super::storage::Storage::new()
                .connect()
                .map_err(|_| "Programme data unavailable")?;
            // Reuse the existing candidate review's coherent transaction;
            // capture selected-guide revision BEFORE ending the same read
            // snapshot. Never read a later revision for older suggestions.
            let tx = conn
                .transaction()
                .map_err(|_| "Unable to read programme data")?;
            let review = crate::epg::candidates::review_in_transaction(&tx, &guide, &cancel)
                .map_err(|error| {
                    use crate::epg::candidates::ReviewLoadError;
                    match error {
                        ReviewLoadError::Unavailable => "Guide disabled or not imported yet",
                        ReviewLoadError::Cancelled => "Review cancelled",
                        ReviewLoadError::TooLarge => "Review memory or row budget exceeded",
                        ReviewLoadError::Storage => "Unable to read programme data",
                    }
                })?;
            let revision: i64 = tx
                .query_row(
                    "SELECT updated_at FROM epg_sources WHERE id=?1 AND enabled=1",
                    [&guide],
                    |row| row.get(0),
                )
                .map_err(|_| "Unable to read guide revision")?;
            tx.commit().map_err(|_| "Unable to finish guide review")?;
            Ok((review, revision))
        })
        .await
        .map_err(|_| "Programme mapping worker unavailable")?
    })
    .await
}

async fn prepare_guide_bulk(
    guide: String,
    choices: Vec<crate::epg::bulk::AliasChoice>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<crate::epg::bulk::BulkPreview, &'static str> {
    let permit = guide_work_permit()?;
    crate::utils::tokio(async move {
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut conn = super::storage::Storage::new()
                .connect()
                .map_err(|_| "Programme data unavailable")?;
            // Match validation, impact and the fingerprint now share one
            // consistent SQLite read transaction (no TOCTOU gap).
            crate::epg::bulk::preview_discovered_bulk(&mut conn, &guide, &choices, &cancel)
                .map_err(|_| "Candidate or guide state changed; reopen the review")
        })
        .await
        .map_err(|_| "Programme mapping worker unavailable")?
    })
    .await
}

async fn commit_guide_bulk(
    preview: crate::epg::bulk::BulkPreview,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<crate::epg::bulk::SessionUndo, &'static str> {
    let permit = guide_work_permit()?;
    crate::utils::tokio(async move {
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut conn = super::storage::Storage::new()
                .connect()
                .map_err(|_| "Programme data unavailable")?;
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |value| value.as_secs() as i64);
            crate::epg::bulk::apply_bulk(
                &mut conn,
                crate::epg::bulk::ConfirmedBulkReview::after_user_confirmation(),
                preview,
                at,
                &cancel,
            )
            .map_err(|_| "Mapping state changed or write failed; no partial batch was saved")
        })
        .await
        .map_err(|_| "Programme mapping worker unavailable")?
    })
    .await
}

fn present_bulk_undo(
    parent: &gtk::Window,
    view: &TvStreamsView,
    undo: crate::epg::bulk::SessionUndo,
) {
    let dialog = adw::Dialog::new();
    dialog.set_title(&gettext("Mappings saved"));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    body.set_margin_start(20);
    body.set_margin_end(20);
    label(&gettext("Your confirmed aliases were saved. Same-session undo is available only while nothing relevant has changed."), &body);
    let error = gtk::Label::new(None);
    error.set_xalign(0.0);
    error.set_wrap(true);
    body.append(&error);
    let undo_button = gtk::Button::with_label(&gettext("Undo this batch"));
    body.append(&undo_button);
    let close = gtk::Button::with_label(&gettext("Close"));
    body.append(&close);
    close.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    let undo_slot = std::rc::Rc::new(std::cell::RefCell::new(Some(undo)));
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    dialog.connect_closed({
        let cancelled = cancelled.clone();
        move |_| cancelled.store(true, std::sync::atomic::Ordering::Release)
    });
    undo_button.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        error,
        #[weak]
        view,
        #[weak]
        undo_button,
        #[strong]
        cancelled,
        move |_| {
            let Some(token) = undo_slot.borrow_mut().take() else {
                return;
            };
            undo_button.set_sensitive(false);
            let cancelled = cancelled.clone();
            spawn(glib::clone!(
                #[weak]
                error,
                #[weak]
                dialog,
                #[weak]
                view,
                async move {
                    let result = crate::utils::tokio(async move {
                        tokio::task::spawn_blocking(move || {
                            let mut conn = super::storage::Storage::new()
                                .connect()
                                .map_err(|_| "Programme database unavailable")?;
                            crate::epg::bulk::undo_same_session(&mut conn, token, &cancelled)
                                .map_err(|_| "Undo refused: mappings or guide state changed")
                        })
                        .await
                        .map_err(|_| "Programme mapping worker unavailable")?
                    })
                    .await;
                    match result {
                        Ok(()) => {
                            view.epg_changed();
                            dialog.close();
                        }
                        Err(message) => error.set_text(&gettext(message)),
                    }
                }
            ));
        }
    ));
    dialog.set_child(Some(&body));
    dialog.present(Some(parent));
}

fn present_bulk_confirmation(
    parent: &gtk::Window,
    view: &TvStreamsView,
    preview: crate::epg::bulk::BulkPreview,
) {
    let dialog = adw::Dialog::new();
    dialog.set_title(&gettext("Confirm programme mappings"));
    dialog.set_content_width(660);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);
    let items = preview.impact();
    let (mut selected, mut manual, mut elsewhere) = (0_u64, 0_u64, 0_u64);
    for item in items {
        selected = selected.saturating_add(item.selected_guide_rows);
        manual = manual.saturating_add(item.manual_exclusions);
        elsewhere = elsewhere.saturating_add(item.other_guide_rows);
    }
    label(&format!(
        "{}: {}\n{}: {} • {}: {} • {}: {}\n{}",
        gettext("Distinct original IPTV ID alias keys"), items.len(),
        gettext("Currently selected-guide rows"), selected,
        gettext("Manual mapping exclusions"), manual,
        gettext("Rows selecting other guides"), elsewhere,
        gettext("Each guide-scoped alias can also affect FUTURE imported rows with exactly the same case-sensitive tvg-id. No inferred mapping is activated without your approval.")
    ), &body);
    // A small illustrative page; never instantiate 4,096 impact widgets.
    let details = gtk::Box::new(gtk::Orientation::Vertical, 6);
    for item in items.iter().take(12) {
        label(
            &format!(
                "{} → {} ({}: {}, {}: {})",
                item.choice.original_tvg_id,
                item.choice.full_xmltv_id,
                gettext("Current rows"),
                item.selected_guide_rows,
                gettext("Manual exclusions"),
                item.manual_exclusions
            ),
            &details,
        );
    }
    if items.len() > 12 {
        label(
            &format!(
                "{} {}",
                items.len() - 12,
                gettext("additional aliases included in the batch")
            ),
            &details,
        );
    }
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_min_content_height(120);
    scroller.set_max_content_height(230);
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_child(Some(&details));
    body.append(&scroller);
    let error = gtk::Label::new(None);
    error.set_xalign(0.0);
    error.set_wrap(true);
    body.append(&error);
    let approve = gtk::Button::with_label(&gettext("Confirm and apply all listed aliases"));
    let cancel_button = gtk::Button::with_label(&gettext("Back without changes"));
    body.append(&approve);
    body.append(&cancel_button);
    cancel_button.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    let preview_slot = std::rc::Rc::new(std::cell::RefCell::new(Some(preview)));
    let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    dialog.connect_closed({
        let cancellation = cancellation.clone();
        move |_| cancellation.store(true, std::sync::atomic::Ordering::Release)
    });
    let parent = parent.clone();
    let present_parent = parent.clone();
    approve.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        error,
        #[weak]
        approve,
        #[weak]
        view,
        #[strong]
        cancellation,
        move |_| {
            let Some(preview) = preview_slot.borrow_mut().take() else {
                return;
            };
            approve.set_sensitive(false);
            spawn(glib::clone!(
                #[weak]
                dialog,
                #[weak]
                error,
                #[weak]
                view,
                #[strong]
                parent,
                #[strong]
                cancellation,
                async move {
                    match commit_guide_bulk(preview, cancellation).await {
                        Ok(undo) => {
                            view.epg_changed();
                            dialog.close();
                            present_bulk_undo(&parent, &view, undo);
                        }
                        Err(message) => error.set_text(&gettext(message)),
                    }
                }
            ));
        }
    ));
    dialog.set_child(Some(&body));
    dialog.present(Some(&present_parent));
}

fn show_epg_bulk_review(view: &TvStreamsView, parent: &gtk::Window, guide: String) {
    let dialog = adw::Dialog::new();
    dialog.set_title(&gettext("Review suggested programme mappings"));
    dialog.set_content_width(760);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);
    label(&gettext("Candidates are NOT confirmed matches. Verify complete XMLTV IDs, feed/region warnings and cross-playlist effects before approving."), &body);
    let overview = gtk::Label::new(Some(&gettext("Building selected-guide index…")));
    overview.set_wrap(true);
    overview.set_xalign(0.0);
    body.append(&overview);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_min_content_height(290);
    scroll.set_max_content_height(410);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_child(Some(&list));
    body.append(&scroll);
    let position = gtk::Label::new(None);
    body.append(&position);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let previous = gtk::Button::with_label(&gettext("Previous page"));
    let next = gtk::Button::with_label(&gettext("Next page"));
    let preview = gtk::Button::with_label(&gettext("Preview selected aliases"));
    let inspect_hints = gtk::ToggleButton::with_label(&gettext("Inspect name-only hints"));
    inspect_hints.set_sensitive(false);
    controls.append(&inspect_hints);
    let close = gtk::Button::with_label(&gettext("Close"));
    for control in [&previous, &next, &preview, &close] {
        control.set_sensitive(control == &close);
        controls.append(control);
    }
    body.append(&controls);
    close.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    dialog.connect_closed({
        let cancelled = cancelled.clone();
        move |_| cancelled.store(true, std::sync::atomic::Ordering::Release)
    });
    dialog.set_child(Some(&body));
    dialog.present(Some(parent));
    let parent = parent.clone();
    spawn(glib::clone!(
        #[weak]
        overview,
        #[weak]
        previous,
        #[weak]
        next,
        #[weak]
        preview,
        #[weak]
        inspect_hints,
        #[weak]
        list,
        #[weak]
        position,
        #[weak]
        view,
        #[strong]
        cancelled,
        async move {
            match compute_guide_review(guide.clone(), cancelled.clone()).await {
                Ok((review, guide_revision))
                    if !cancelled.load(std::sync::atomic::Ordering::Acquire) =>
                {
                    let c = &review.counts;
                    let timestamp = crate::utils::format_timestamp_full(review.last_success_at)
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| gettext("Unknown local time"));
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_secs() as i64);
                    let freshness = if review.last_success_at > now {
                        gettext("Guide import time is in the future; freshness unknown")
                    } else {
                        let age = now - review.last_success_at;
                        if age > 259_200 {
                            gettext("Guide is stale (over 3 days); check refresh status before trusting schedules")
                        } else if age > 129_600 {
                            gettext("Guide is aging (over 36 hours); schedules may be incomplete")
                        } else {
                            gettext(
                                "Guide recently imported; schedule completeness is not guaranteed",
                            )
                        }
                    };
                    overview.set_text(&format!(
                        "{}: {} • {}: {}\n{}\n{}: {} • {}: {} • {}: {}\n{}: {} • {}: {} • {}: {} • {}: {}\n{}: {}",
                        gettext("Selected guide"), review.guide_source_id,
                        gettext("Last successful import (local)"), timestamp,
                        freshness,
                        gettext("Selected playlist rows"), c.playlist_rows,
                        gettext("Manual/confirmed"), c.manual_or_alias,
                        gettext("Exact IDs"), c.exact_ids,
                        gettext("Feed candidates for review"), c.feed_review,
                        gettext("Name-only hints (not aliasable)"), c.name_only,
                        gettext("Ambiguous"), c.ambiguous,
                        gettext("Unmatched"), c.unmatched,
                        gettext("Missing imported IDs"), c.no_imported_id
                    ));
                    let review = std::rc::Rc::new(review);
                    let selected = std::rc::Rc::new(std::cell::RefCell::new(BTreeSet::new()));
                    for item in &review.proposals {
                        if item.candidate.default_review_eligible
                            && selected.borrow().len() < GUIDE_REVIEW_MAX_SELECTIONS
                        {
                            selected
                                .borrow_mut()
                                .insert(item.candidate.original_tvg_id.clone());
                        }
                    }
                    let current = std::rc::Rc::new(std::cell::Cell::new(0usize));
                    let read_only_hints = std::rc::Rc::new(std::cell::Cell::new(false));
                    populate_guide_review_page(&review, 0, &list, &position, &selected);
                    previous.set_sensitive(false);
                    next.set_sensitive(review.proposals.len() > GUIDE_REVIEW_PAGE_SIZE);
                    preview.set_sensitive(!review.proposals.is_empty());
                    inspect_hints.set_sensitive(!review.name_only_hints.is_empty());
                    inspect_hints.connect_toggled({
                        let review = review.clone();
                        let selected = selected.clone();
                        let current = current.clone();
                        let view = view.clone();
                        let read_only_hints = read_only_hints.clone();
                        let list = list.clone();
                        let position = position.clone();
                        let previous = previous.clone();
                        let next = next.clone();
                        let preview = preview.clone();
                        move |button| {
                            let hints = button.is_active();
                            read_only_hints.set(hints);
                            current.set(0);
                            if hints {
                                populate_read_only_name_hints_page(
                                    &review,
                                    guide_revision,
                                    0,
                                    &list,
                                    &position,
                                    &view,
                                );
                            } else {
                                populate_guide_review_page(&review, 0, &list, &position, &selected);
                            }
                            let count = if hints {
                                review.name_only_hints.len()
                            } else {
                                review.proposals.len()
                            };
                            previous.set_sensitive(false);
                            next.set_sensitive(count > GUIDE_REVIEW_PAGE_SIZE);
                            preview.set_sensitive(!hints && !review.proposals.is_empty());
                        }
                    });
                    for (button, increment) in [(&previous, false), (&next, true)] {
                        let review = review.clone();
                        let selected = selected.clone();
                        let current = current.clone();
                        let view = view.clone();
                        let list = list.clone();
                        let position = position.clone();
                        let previous = previous.clone();
                        let next = next.clone();
                        let read_only_hints = read_only_hints.clone();
                        button.connect_clicked(move |_| {
                            let hints = read_only_hints.get();
                            let pages = (if hints {
                                review.name_only_hints.len()
                            } else {
                                review.proposals.len()
                            })
                            .div_ceil(GUIDE_REVIEW_PAGE_SIZE);
                            let new_page = if increment {
                                current.get().saturating_add(1)
                            } else {
                                current.get().saturating_sub(1)
                            };
                            if new_page >= pages {
                                return;
                            }
                            current.set(new_page);
                            if hints {
                                populate_read_only_name_hints_page(
                                    &review,
                                    guide_revision,
                                    new_page,
                                    &list,
                                    &position,
                                    &view,
                                );
                            } else {
                                populate_guide_review_page(
                                    &review, new_page, &list, &position, &selected,
                                );
                            }
                            previous.set_sensitive(new_page > 0);
                            next.set_sensitive(new_page + 1 < pages);
                        });
                    }
                    preview.connect_clicked(glib::clone!(
                        #[weak]
                        preview,
                        #[weak]
                        overview,
                        #[weak]
                        view,
                        #[strong]
                        parent,
                        #[strong]
                        review,
                        #[strong]
                        selected,
                        #[strong]
                        cancelled,
                        move |_| {
                            let choices = review
                                .proposals
                                .iter()
                                .filter(|item| {
                                    selected.borrow().contains(&item.candidate.original_tvg_id)
                                })
                                .map(|item| crate::epg::bulk::AliasChoice {
                                    original_tvg_id: item.candidate.original_tvg_id.clone(),
                                    full_xmltv_id: item.candidate.full_xmltv_id.clone(),
                                })
                                .collect::<Vec<_>>();
                            if choices.is_empty() {
                                overview.set_text(&gettext(
                                    "Select at least one proposed ID to preview.",
                                ));
                                return;
                            }
                            preview.set_sensitive(false);
                            spawn(glib::clone!(
                                #[weak]
                                preview,
                                #[weak]
                                overview,
                                #[weak]
                                view,
                                #[strong]
                                parent,
                                #[strong]
                                cancelled,
                                #[strong]
                                guide,
                                async move {
                                    let result =
                                        prepare_guide_bulk(guide, choices, cancelled.clone()).await;
                                    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                                        return;
                                    }
                                    preview.set_sensitive(true);
                                    match result {
                                        Ok(impact) => {
                                            present_bulk_confirmation(&parent, &view, impact)
                                        }
                                        Err(reason) => overview.set_text(&gettext(reason)),
                                    }
                                }
                            ));
                        }
                    ));
                }
                Ok(_) => (),
                Err(reason) => overview.set_text(&gettext(reason)),
            }
        }
    ));
}

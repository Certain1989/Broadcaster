// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Editor of *effective* channel facets with imported/default/manual provenance.
//! The service persists one validated ChannelOverrides transaction.
use std::collections::BTreeSet;

use adw::{glib, gtk, prelude::*};
use gettextrs::gettext;

use crate::{
    streams::{
        model::{
            normalize_facet, ChannelDetail, ChannelOverrides, FacetDimension, FacetMode, FieldOrigin,
            GroupOverride, Region, RegionOverride, SetOverride,
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

fn tokens(raw: &str, kind: FacetDimension) -> Result<BTreeSet<String>, &'static str> {
    let mut result = BTreeSet::new();
    for text in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        result.insert(normalize_facet(text, kind).map_err(|_| "Invalid facet code")?);
    }
    if result.len() > 64 {
        return Err("Too many facet codes");
    }
    Ok(result)
}

fn text(value: &str) -> gtk::Entry {
    let entry = gtk::Entry::new();
    entry.set_text(value);
    entry.set_hexpand(true);
    entry
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
    value.filter(|value| !value.trim().is_empty())
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
    region.map_or_else(|| gettext("Unknown"), |region| {
        let code = region.code.as_deref().unwrap_or("");
        let label = region.label.as_deref().unwrap_or("");
        if code.is_empty() { label.to_owned() }
        else if label.is_empty() { code.to_owned() }
        else { format!("{code} ({label})") }
    })
}

fn values_detail(
    body: &gtk::Box,
    imported: String,
    default: Option<String>,
    effective: String,
) {
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
    box_.append(field);
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
    group_mode: gtk::DropDown,
    group: gtk::Entry,
    language_mode: gtk::DropDown,
    languages: gtk::Entry,
    country_mode: gtk::DropDown,
    countries: gtk::Entry,
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
            &format!("{}: {}", gettext("Name origin"), origin_text(detail.provenance.name)),
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
        body.append(&name);
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
        values_detail(
            body,
            value_text(detail.imported.group_title.as_deref()),
            None,
            value_text(detail.effective.group.as_deref()),
        );
        let language_mode = mode_picker(detail.overrides.languages.mode);
        let languages = text(
            &detail
                .effective
                .languages
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        );
        editor_row(
            body,
            &gettext("Languages (comma separated)"),
            &origin_text(detail.provenance.languages),
            &language_mode,
            &languages,
        );
        values_detail(
            body,
            set_text(&detail.imported.languages),
            Some(set_text(&detail.source_defaults.languages)),
            set_text(&detail.effective.languages),
        );
        let country_mode = mode_picker(detail.overrides.countries.mode);
        let countries = text(
            &detail
                .effective
                .countries
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        );
        editor_row(
            body,
            &gettext("Countries (ISO codes, comma separated)"),
            &origin_text(detail.provenance.countries),
            &country_mode,
            &countries,
        );
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
        body.append(&region_label);
        values_detail(
            body,
            region_text(detail.imported.region.as_ref()),
            Some(region_text(detail.source_defaults.region.as_ref())),
            region_text(detail.effective.region.as_ref()),
        );
        Self {
            use_name,
            name,
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

    fn edits(&self) -> Result<ChannelOverrides, &'static str> {
        let display_name = if self.use_name.is_active() {
            let name = self.name.text().trim().to_owned();
            if name.is_empty() || name.len() > 512 {
                return Err("Invalid display name");
            }
            Some(name)
        } else {
            None
        };
        let group_mode = mode(&self.group_mode);
        let group = if group_mode == FacetMode::Replace {
            let value = self.group.text().trim().to_owned();
            if value.is_empty() || value.len() > 256 {
                return Err("Invalid group name");
            }
            Some(value)
        } else {
            None
        };
        let language_mode = mode(&self.language_mode);
        let languages = if language_mode == FacetMode::Replace {
            let values = tokens(&self.languages.text(), FacetDimension::Language)?;
            if values.is_empty() {
                return Err("Enter at least one language");
            }
            values
        } else {
            BTreeSet::new()
        };
        let country_mode = mode(&self.country_mode);
        let countries = if country_mode == FacetMode::Replace {
            let values = tokens(&self.countries.text(), FacetDimension::Country)?;
            if values.is_empty() {
                return Err("Enter at least one country");
            }
            values
        } else {
            BTreeSet::new()
        };
        let region_mode = mode(&self.region_mode);
        let region = if region_mode == FacetMode::Replace {
            Some(filled_region(
                &self.region_code.text(),
                &self.region_label.text(),
            )?)
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
        value.validate().map_err(|_| "Invalid channel override")?;
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
                            errors.set_text(error);
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


#[cfg(test)]
mod editor_tests {
    use super::*;

    #[test]
    fn codes_and_regions_normalize_before_saving() {
        assert_eq!(tokens("de-CH, en, de-CH", FacetDimension::Language).unwrap(),
                   ["de-CH".to_owned(), "en".to_owned()].into_iter().collect());
        assert_eq!(tokens("de, at", FacetDimension::Country).unwrap(),
                   ["DE".to_owned(), "AT".to_owned()].into_iter().collect());
        assert!(tokens("unknown", FacetDimension::Country).is_err());
        assert!(tokens("en, zz-?", FacetDimension::Language).is_err());
        assert_eq!(filled_region("  DE-BY ", " Bavaria ").unwrap().code.as_deref(),
                   Some("DE-BY"));
        assert!(filled_region(" ", " ").is_err());
        assert!(filled_region(&"x".repeat(65), "Label").is_err());
    }
}

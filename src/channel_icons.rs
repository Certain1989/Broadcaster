// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::Cell;

use adw::{gdk, gdk::gdk_pixbuf, glib, prelude::*};
use eyre::WrapErr;
use phf::phf_map;
use tracing::error;

/// Resolve a bundled channel logo or show a readable textual fallback.
pub fn load_channel_icon(
    channel_id: Option<&str>,
    image: &gtk::Image,
    fallback: &gtk::Label,
    size: i32,
) {
    let channel_id = channel_id.unwrap_or("");
    fallback.set_label(&fallback_label(channel_id));
    fallback.set_tooltip_text(Some(channel_id));

    let Some(icon_name) = find_icon(channel_id) else {
        show_fallback(image, fallback);
        return;
    };

    let style_manager = adw::StyleManager::default();
    set_icon(&style_manager, image, fallback, icon_name, size);

    let dark_handler = style_manager.connect_dark_notify(glib::clone!(
        #[weak]
        image,
        #[weak]
        fallback,
        move |manager| set_icon(manager, &image, &fallback, icon_name, size)
    ));
    let dark_handler = Cell::new(Some(dark_handler));
    image.connect_destroy(move |_| {
        if let Some(handler) = dark_handler.take() {
            adw::StyleManager::default().disconnect(handler);
        }
    });
    image.connect_scale_factor_notify(glib::clone!(
        #[weak]
        fallback,
        move |image| set_icon(
            &adw::StyleManager::default(),
            image,
            &fallback,
            icon_name,
            size
        )
    ));
}

fn show_fallback(image: &gtk::Image, fallback: &gtk::Label) {
    image.set_visible(false);
    fallback.set_visible(true);
}

fn set_icon(
    style_manager: &adw::StyleManager,
    image: &gtk::Image,
    fallback: &gtk::Label,
    icon_name: &str,
    size: i32,
) {
    match load_icon(
        icon_name,
        size * image.scale_factor(),
        ColorScheme::for_style_manager(style_manager),
    ) {
        Ok(texture) => {
            image.set_paintable(Some(&texture));
            image.set_visible(true);
            fallback.set_visible(false);
        }
        Err(e) => {
            error!("{e:?}");
            show_fallback(image, fallback);
        }
    }
}

fn load_icon(icon_name: &str, size: i32, color_scheme: ColorScheme) -> eyre::Result<gdk::Texture> {
    let resource = format!(
        "/io/github/Certain1989/Broadcaster/icons/scalable/channels/{color_scheme}/{icon_name}"
    );
    gdk_pixbuf::Pixbuf::from_resource_at_scale(&resource, size, size, true)
        .map(|pixbuf| gdk::Texture::for_pixbuf(&pixbuf))
        .wrap_err_with(|| format!("failed to load channel logo from {resource}"))
}

/// Return existing, licensed application artwork for a known imported
/// broadcaster. Only a small set of generic resolution labels is removed;
/// no network search, guessed station identity or remote SVG processing.
pub(crate) fn bundled_streams_logo(name: &str, size: i32) -> Option<gdk::Texture> {
    let mut base = name.trim();
    for suffix in [" (1080p)", " (720p)", " (576p)", " (480p)", " (360p)", " HD"] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped;
            break;
        }
    }
    let name = find_icon(base)?;
    load_icon(
        name,
        size,
        ColorScheme::for_style_manager(&adw::StyleManager::default()),
    )
    .ok()
}

/// Use exact identifiers or explicit aliases, not fuzzy prefix matching.
fn find_icon(channel_id: &str) -> Option<&'static str> {
    if let Some(icon) = ICON_NAMES.get(channel_id.trim()) {
        return Some(*icon);
    }
    let normalized = channel_id.trim().to_lowercase();
    match normalized.as_str() {
        "3sat" => Some("3sat.svg"),
        "ard" => Some("ard.svg"),
        "ard-alpha" | "ard alpha" => Some("ard-alpha.svg"),
        "arte" | "arte.de" | "arte.en" | "arte.es" | "arte.fr" | "arte.it" | "arte.pl" => {
            Some("arte.svg")
        }
        "br" => Some("br.svg"),
        "das erste" => Some("das-erste.svg"),
        "dw" => Some("deutsche-welle.svg"),
        "hr" => Some("hr.svg"),
        "kika" => Some("kika.svg"),
        "mdr" => Some("mdr.svg"),
        "ndr" => Some("ndr.svg"),
        "one" => Some("one.svg"),
        "orf" => Some("orf.svg"),
        "phoenix" => Some("phoenix.svg"),
        "radio bremen" | "radio bremen tv" | "radiobremen" | "rbtv" => Some("rb.svg"),
        "rbb" => Some("rbb.svg"),
        "sr" => Some("sr.svg"),
        "srf" => Some("srf.svg"),
        "swr" => Some("swr.svg"),
        "tagesschau24" => Some("tagesschau24.svg"),
        "wdr" => Some("wdr.svg"),
        "zdf" | "zdf-tivi" => Some("zdf.svg"),
        "zdfinfo" | "zdf info" => Some("zdf-info.svg"),
        "zdfneo" | "zdf neo" => Some("zdf-neo.svg"),
        _ => None,
    }
}

/// A compact label for the space normally occupied by a 64-pixel logo.
fn fallback_label(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if words.is_empty() {
        return "?".to_owned();
    }
    if words.len() > 1 {
        words
            .iter()
            .take(3)
            .filter_map(|word| word.chars().next())
            .flat_map(char::to_uppercase)
            .collect()
    } else {
        words[0]
            .chars()
            .take(5)
            .flat_map(char::to_uppercase)
            .collect()
    }
}

static ICON_NAMES: phf::Map<&'static str, &'static str> = phf_map! {
    // live
    "ard_alpha" => "ard-alpha.svg",
    "arte" => "arte.svg",
    "br_nord" => "br.svg",
    "br_sued" => "br.svg",
    "das_erste" => "das-erste.svg",
    "deutsche_welle" => "deutsche-welle.svg",
    "deutsche_welle_plus" => "deutsche-welle.svg",
    "dreisat" => "3sat.svg",
    "hr" => "hr.svg",
    "kika" => "kika.svg",
    "mdr_sachsen" => "mdr.svg",
    "mdr_sachsen_anhalt" => "mdr.svg",
    "mdr_thueringen" => "mdr.svg",
    "ndr_hh" => "ndr.svg",
    "ndr_mv" => "ndr.svg",
    "ndr_nds" => "ndr.svg",
    "ndr_sh" => "ndr.svg",
    "one" => "one.svg",
    "parlamentsfernsehen_1" => "parlamentsfernsehen.svg",
    "parlamentsfernsehen_2" => "parlamentsfernsehen.svg",
    "phoenix" => "phoenix.svg",
    "rb" => "rb.svg",
    "rbb_berlin" => "rbb.svg",
    "rbb_brandenburg" => "rbb.svg",
    "sr" => "sr.svg",
    "swr_bw" => "swr.svg",
    "swr_rp" => "swr.svg",
    "tagesschau24" => "tagesschau24.svg",
    "wdr" => "wdr.svg",
    "zdf" => "zdf.svg",
    "zdf_info" => "zdf-info.svg",
    "zdf_neo" => "zdf-neo.svg",
    // mediathek
    "3Sat" => "3sat.svg",
    "ARD" => "ard.svg",
    "ARTE.DE" => "arte.svg",
    "ARTE.EN" => "arte.svg",
    "ARTE.ES" => "arte.svg",
    "ARTE.FR" => "arte.svg",
    "ARTE.IT" => "arte.svg",
    "ARTE.PL" => "arte.svg",
    "BR" => "br.svg",
    "DW" => "deutsche-welle.svg",
    "HR" => "hr.svg",
    "KiKA" => "kika.svg",
    "MDR" => "mdr.svg",
    "NDR" => "ndr.svg",
    "ORF" => "orf.svg",
    "PHOENIX" => "phoenix.svg",
    "Radio Bremen TV" => "rb.svg",
    "RBB" => "rbb.svg",
    "rbtv" => "rb.svg",
    "SR" => "sr.svg",
    "SRF" => "srf.svg",
    "SWR" => "swr.svg",
    "WDR" => "wdr.svg",
    "ZDF" => "zdf.svg",
    "ZDF-tivi" => "zdf.svg",
};

#[derive(Clone, Copy, Debug)]
enum ColorScheme {
    Light,
    Dark,
}

impl ColorScheme {
    fn for_style_manager(style_manager: &adw::StyleManager) -> Self {
        if style_manager.is_dark() {
            Self::Dark
        } else {
            Self::Light
        }
    }
}

impl std::fmt::Display for ColorScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Light => "light",
            Self::Dark => "dark",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{fallback_label, find_icon};

    #[test]
    fn known_broadcasters_match_explicit_aliases() {
        assert_eq!(find_icon("ARTE.DE"), Some("arte.svg"));
        assert_eq!(find_icon(" arte.de "), Some("arte.svg"));
        assert_eq!(find_icon("3sat"), Some("3sat.svg"));
        assert_eq!(find_icon("ard-alpha"), Some("ard-alpha.svg"));
        assert_eq!(find_icon("Radio Bremen"), Some("rb.svg"));
        assert_eq!(find_icon("radiobremen"), Some("rb.svg"));
        assert_eq!(find_icon("zdfneo"), Some("zdf-neo.svg"));
    }

    #[test]
    fn unfamiliar_broadcasters_have_text_instead_of_wrong_artwork() {
        assert_eq!(find_icon("Unrelated Channel"), None);
        assert_eq!(find_icon("ARTE-fake"), None);
        assert_eq!(fallback_label("Unrelated Channel"), "UC");
        assert_eq!(fallback_label("unknown"), "UNKNO");
        assert_eq!(fallback_label(""), "?");
    }

    #[test]
    fn selected_logo_assets_are_bundled_for_both_color_schemes() {
        let resource =
            adw::gio::Resource::from_data(&adw::glib::Bytes::from_static(crate::GRESOURCE_BYTES))
                .expect("GResource must be readable");
        for logo in [
            "3sat.svg",
            "ard.svg",
            "ard-alpha.svg",
            "arte.svg",
            "rb.svg",
            "zdf-neo.svg",
            "orf.svg",
            "srf.svg",
        ] {
            for scheme in ["light", "dark"] {
                let path = format!(
                    "/io/github/Certain1989/Broadcaster/icons/scalable/channels/{scheme}/{logo}"
                );
                assert!(
                    resource
                        .lookup_data(&path, adw::gio::ResourceLookupFlags::NONE)
                        .is_ok(),
                    "missing bundled resource {path}"
                );
            }
        }
    }
}

// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::new_without_default)]

mod application;
mod channel_icons;
#[allow(dead_code)]
mod epg;
#[allow(dead_code)]
mod config {
    include!(concat!(env!("OUT_DIR"), "/config.rs"));
}
mod keyboard;
mod launcher;
mod live;
mod mediathek;
mod mpv;
mod preferences;
mod programme_markup;
mod settings;
mod streams;
mod utils;
mod window;
mod zapp;

use self::{
    application::TvApplication,
    config::{LOCALEDIR, PROJECT_NAME},
};

use adw::{gio, glib, gtk, prelude::*};
use gettextrs::{bind_textdomain_codeset, bindtextdomain, textdomain};

static GRESOURCE_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/broadcaster.gresource"));

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_file(true)
        .with_line_number(true)
        .with_target(false)
        .with_env_filter(tracing_subscriber::filter::EnvFilter::from_default_env())
        .init();

    bindtextdomain(PROJECT_NAME, LOCALEDIR).expect("Unable to bind the text domain");
    bind_textdomain_codeset(PROJECT_NAME, "UTF-8").expect("Unable to set the text domain encoding");
    textdomain(PROJECT_NAME).expect("Unable to switch to the text domain");
    gio::resources_register(
        &gio::Resource::from_data(&glib::Bytes::from_static(GRESOURCE_BYTES)).unwrap(),
    );

    let application = TvApplication::new();
    // GtkApplication initializes GDK during startup, not before run().
    // Installing a provider earlier can silently do nothing (no display).
    application.connect_startup(|_| {
        let css = gtk::CssProvider::new();
        css.load_from_resource("/io/github/Certain1989/Broadcaster/broadcaster.css");
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });
    application.run()
}

#[cfg(test)]
mod tests {
    use super::{gio, glib, gtk, GRESOURCE_BYTES};

    #[test]
    fn bundled_resources_include_generated_files() {
        let resource = gio::Resource::from_data(&glib::Bytes::from_static(GRESOURCE_BYTES))
            .expect("GResource must be readable");

        for name in [
            "/io/github/Certain1989/Broadcaster/shortcuts-dialog.ui",
            "/io/github/Certain1989/Broadcaster/io.github.Certain1989.Broadcaster.metainfo.xml",
            "/io/github/Certain1989/Broadcaster/icons/scalable/actions/loupe-symbolic.svg",
            "/io/github/Certain1989/Broadcaster/broadcaster.css",
            "/io/github/Certain1989/Broadcaster/icons/128x128/apps/io.github.Certain1989.Broadcaster.png",
            "/io/github/Certain1989/Broadcaster/icons/128x128/apps/io.github.Certain1989.Broadcaster-about.png",
        ] {
            assert!(
                resource
                    .lookup_data(name, gio::ResourceLookupFlags::NONE)
                    .is_ok(),
                "missing bundled resource {name}"
            );
        }

        // Existence alone did not catch the broken About image in the user's
        // Flatpak. GDK must actually be able to decode the embedded PNG.
        let icon = resource
            .lookup_data(
                "/io/github/Certain1989/Broadcaster/icons/128x128/apps/io.github.Certain1989.Broadcaster.png",
                gio::ResourceLookupFlags::NONE,
            )
            .expect("About icon must be bundled");
        gtk::gdk::Texture::from_bytes(&icon)
            .expect("bundled About icon must be a GDK-decodable PNG");
    }
}

// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::OnceCell,
    collections::HashSet,
    ops::Deref,
    sync::{Mutex, OnceLock},
};

use adw::{gio, glib, prelude::*};

use crate::{config::BASE_APP_ID, zapp::ChannelId};

// Typed access to the existing schema without a build-time XML proc macro.
// No schema IDs, keys, defaults or existing user data are changed.
#[derive(Clone, Debug)]
pub struct TvSettings(gio::Settings);

macro_rules! string_setting {
    ($getter:ident, $setter:ident, $key:literal) => {
        pub fn $getter(&self) -> String {
            self.0.string($key).to_string()
        }

        pub fn $setter(&self, value: &str) {
            self.0
                .set_string($key, value)
                .expect(concat!("Could not update ", $key));
        }
    };
}

macro_rules! connect_setting {
    ($method:ident, $key:literal) => {
        pub fn $method<F>(&self, callback: F) -> glib::SignalHandlerId
        where
            F: Fn(&gio::Settings) + 'static,
        {
            self.0
                .connect_changed(Some($key), move |settings, _| callback(settings))
        }
    };
}

macro_rules! bind_setting {
    ($method:ident, $key:literal) => {
        pub fn $method<'a, P: glib::prelude::IsA<glib::Object>>(
            &'a self,
            object: &'a P,
            property: &'a str,
        ) -> gio::BindingBuilder<'a> {
            self.0.bind($key, object, property)
        }
    };
}

impl TvSettings {
    pub fn new(id: &str) -> Self {
        Self(gio::Settings::new(id))
    }

    pub fn get() -> Self {
        thread_local! {
            static SETTINGS: OnceCell<TvSettings> = const { OnceCell::new() };
        }
        SETTINGS.with(|settings| {
            settings
                .get_or_init(|| TvSettings::new(BASE_APP_ID))
                .clone()
        })
    }

    pub fn use_external_player(&self) -> bool {
        self.0.boolean("use-external-player")
    }

    string_setting!(
        video_player_name,
        set_video_player_name,
        "video-player-name"
    );
    string_setting!(video_player_id, set_video_player_id, "video-player-id");
    string_setting!(
        video_downloader_name,
        set_video_downloader_name,
        "video-downloader-name"
    );
    string_setting!(
        video_downloader_id,
        set_video_downloader_id,
        "video-downloader-id"
    );

    pub fn mpv_arguments(&self) -> Vec<String> {
        self.0
            .strv("mpv-arguments")
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// Consent is keyed to the *source ID and exact locator*, not merely a
    /// host/URL shared by unrelated sources. A source replacement, restore
    /// with a new ID, or changed locator cannot inherit old permission.
    /// Only a SHA-256 digest of the sensitive locator is stored in GSettings.
    pub(crate) fn playlist_lan_allowed(&self, id: &str, locator: &str) -> bool {
        // An imported snapshot atomically replaces the SQLite inode, so a
        // restored source cannot inherit consent from the previous database.
        let Some(db_identity) = playlist_database_identity() else {
            return false;
        };
        let key = playlist_lan_consent_key(id, locator, &db_identity);
        lan_consent_effective(
            id,
            self.0
                .strv("playlist-lan-consents")
                .iter()
                .any(|consent| consent.as_str() == key),
        )
    }

    /// Retry revocation if an old persisted entry remains, even when an
    /// earlier failed write already fenced the source for this process.
    pub(crate) fn playlist_lan_consent_needs_update(
        &self,
        id: &str,
        locator: &str,
        requested: bool,
    ) -> bool {
        if requested {
            !self.playlist_lan_allowed(id, locator)
        } else {
            let prefix = format!("{id}:");
            self.0
                .strv("playlist-lan-consents")
                .iter()
                .any(|entry| entry.starts_with(&prefix))
        }
    }

    /// A failed preference write must not silently grant LAN access.
    pub(crate) fn set_playlist_lan_consent(&self, id: &str, locator: &str, allowed: bool) -> bool {
        update_lan_consent_with_write(id, allowed, || {
            let Some(db_identity) = playlist_database_identity() else {
                return false;
            };
            let prefix = format!("{id}:");
            let mut entries: Vec<String> = self
                .0
                .strv("playlist-lan-consents")
                .iter()
                .filter(|entry| !entry.starts_with(&prefix))
                .map(ToString::to_string)
                .collect();
            if allowed {
                // Limit stale entries without promoting any unapproved source;
                // dropped entries simply require fresh user consent.
                if entries.len() >= 1024 {
                    entries.drain(..entries.len() - 1023);
                }
                entries.push(playlist_lan_consent_key(id, locator, &db_identity));
            }
            let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
            if self
                .0
                .set_strv("playlist-lan-consents", refs.as_slice())
                .is_err()
            {
                return false;
            }
            // Do not claim success merely because a setter returned Ok: the
            // retained grant must really have disappeared for revocation.
            let stored = self.0.strv("playlist-lan-consents");
            if allowed {
                let key = playlist_lan_consent_key(id, locator, &db_identity);
                stored.iter().any(|entry| entry.as_str() == key)
            } else {
                !stored.iter().any(|entry| entry.starts_with(&prefix))
            }
        })
    }

    pub fn live_channels(&self) -> Vec<ChannelId> {
        self.0
            .strv("live-channels")
            .iter()
            .map(|id| ChannelId::from(id.to_string()))
            .collect()
    }

    pub fn set_live_channels(&self, channels: Vec<ChannelId>) {
        let values: Vec<&str> = channels.iter().map(AsRef::<str>::as_ref).collect();
        self.0
            .set_strv("live-channels", values.as_slice())
            .expect("Could not update live-channels");
    }

    pub fn default_playback_quality(&self) -> DefaultPlaybackQuality {
        match self.0.string("default-playback-quality").as_str() {
            "low" => DefaultPlaybackQuality::Low,
            "medium" => DefaultPlaybackQuality::Medium,
            _ => DefaultPlaybackQuality::High,
        }
    }

    connect_setting!(connect_live_channels_changed, "live-channels");
    connect_setting!(connect_mpv_arguments_changed, "mpv-arguments");
    connect_setting!(connect_video_player_name_changed, "video-player-name");
    connect_setting!(connect_video_player_id_changed, "video-player-id");
    connect_setting!(
        connect_video_downloader_name_changed,
        "video-downloader-name"
    );
    connect_setting!(connect_video_downloader_id_changed, "video-downloader-id");

    pub fn osk_enabled(&self) -> bool {
        self.0.boolean("osk-enabled")
    }

    pub fn osk_qwertz(&self) -> bool {
        self.0.boolean("osk-qwertz")
    }

    pub fn set_osk_qwertz(&self, enabled: bool) {
        self.0
            .set_boolean("osk-qwertz", enabled)
            .expect("Could not update osk-qwertz");
    }

    bind_setting!(bind_osk_enabled, "osk-enabled");
    bind_setting!(bind_use_external_player, "use-external-player");
    bind_setting!(bind_search_everywhere, "search-everywhere");
    bind_setting!(bind_include_future, "include-future");
    bind_setting!(bind_sort_by, "sort-by");
    bind_setting!(bind_sort_order, "sort-order");
    bind_setting!(bind_width, "width");
    bind_setting!(bind_height, "height");
    bind_setting!(bind_is_maximized, "is-maximized");
    bind_setting!(bind_is_fullscreen, "is-fullscreen");
}

// Serialise LAN permission writes across the GTK and service threads. A failed
// revocation fences access immediately for the current process; it remains a
// documented, explicit failure until the owner decides restart persistence.
// The operation mutex (separate from the deny set) avoids an older setter
// overwriting a later revocation and prevents stale read-modify-write updates.
static DENIED_LAN_SOURCES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static LAN_CONSENT_WRITES: OnceLock<Mutex<()>> = OnceLock::new();

fn denied_lan_sources() -> &'static Mutex<HashSet<String>> {
    DENIED_LAN_SOURCES.get_or_init(|| Mutex::new(HashSet::new()))
}

fn lan_consent_effective(id: &str, stored: bool) -> bool {
    stored
        && !denied_lan_sources()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(id)
}

fn update_lan_consent_with_write(id: &str, allowed: bool, persist: impl FnOnce() -> bool) -> bool {
    // Keep the deny-set mutex independent of the GSettings callback: readers
    // never wait on a backend write while testing whether a source is fenced.
    // Writes are rare, so one small global operation mutex is sufficient.
    let _operation = LAN_CONSENT_WRITES
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !allowed {
        denied_lan_sources()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_owned());
    }
    let persisted = persist();
    if persisted && allowed {
        denied_lan_sources()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
    }
    persisted
}

#[cfg(test)]
pub(crate) fn simulate_lan_consent_write(id: &str, allowed: bool, success: bool) -> bool {
    update_lan_consent_with_write(id, allowed, || success)
}

#[cfg(test)]
pub(crate) fn stale_lan_consent_effective(id: &str) -> bool {
    lan_consent_effective(id, true)
}

fn playlist_database_identity() -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let storage = crate::streams::storage::Storage::new();
    let metadata = std::fs::metadata(storage.database_path()).ok()?;
    metadata
        .is_file()
        .then(|| format!("{}:{}", metadata.dev(), metadata.ino()))
}

fn playlist_lan_consent_key(id: &str, locator: &str, db_identity: &str) -> String {
    use ring::digest::{digest, SHA256};
    use std::fmt::Write;
    let mut key = format!("{id}:{db_identity}:");
    for byte in digest(&SHA256, locator.as_bytes()).as_ref() {
        write!(&mut key, "{byte:02x}").expect("writing to String cannot fail");
    }
    key
}

#[cfg(test)]
mod lan_consent_tests {
    use super::{
        playlist_lan_consent_key, simulate_lan_consent_write, stale_lan_consent_effective,
    };

    #[test]
    fn consent_fingerprint_never_contains_locator_and_is_bound_to_source_id() {
        let url = "http://10.0.0.1/private?token=synthetic";
        let key = playlist_lan_consent_key("source-id", url, "1:100");
        assert!(!key.contains(url));
        assert!(!key.contains("token"));
        assert_ne!(key, playlist_lan_consent_key("other-id", url, "1:100"));
        assert_ne!(key, playlist_lan_consent_key("source-id", url, "1:101"));
        assert_ne!(
            key,
            playlist_lan_consent_key("source-id", "http://10.0.0.2/", "1:100")
        );
    }

    #[test]
    fn write_failed_revoke_stays_denied_until_a_verified_explicit_grant() {
        let id = format!("source-{}", uuid::Uuid::new_v4());
        assert!(stale_lan_consent_effective(&id));
        assert!(!simulate_lan_consent_write(&id, false, false));
        assert!(!stale_lan_consent_effective(&id));
        assert!(!simulate_lan_consent_write(&id, true, false));
        assert!(!stale_lan_consent_effective(&id));
        assert!(simulate_lan_consent_write(&id, true, true));
        assert!(stale_lan_consent_effective(&id));
        assert!(simulate_lan_consent_write(&id, false, true));
        assert!(!stale_lan_consent_effective(&id));
    }

    #[test]
    fn older_cross_thread_grant_cannot_clear_newer_failed_revocation() {
        use std::sync::{mpsc, Arc, Barrier};
        use std::time::Duration;

        // Hold an earlier writer inside its persistence callback while a
        // later revoke begins on another thread. The revoke must run after
        // that writer; a stale successful grant must not clear the deny.
        let id = format!("race-{}", uuid::Uuid::new_v4());
        let start = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let grant_id = id.clone();
        let grant_start = start.clone();
        let grant_release = release.clone();
        let grant = std::thread::spawn(move || {
            super::update_lan_consent_with_write(&grant_id, true, || {
                grant_start.wait();
                grant_release.wait();
                true
            })
        });
        start.wait();
        let (started_sender, started_receiver) = mpsc::channel();
        let revoke_id = id.clone();
        let revoke = std::thread::spawn(move || {
            started_sender.send(()).unwrap();
            simulate_lan_consent_write(&revoke_id, false, false)
        });
        started_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        release.wait();
        assert!(grant.join().unwrap());
        assert!(!revoke.join().unwrap());
        assert!(!stale_lan_consent_effective(&id));
    }
}

impl Deref for TvSettings {
    type Target = gio::Settings;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for TvSettings {
    fn default() -> Self {
        Self::get()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultPlaybackQuality {
    High,
    Medium,
    Low,
}

impl StaticVariantType for ChannelId {
    fn static_variant_type() -> std::borrow::Cow<'static, glib::VariantTy> {
        String::static_variant_type()
    }
}
impl FromVariant for ChannelId {
    fn from_variant(variant: &glib::Variant) -> Option<Self> {
        String::from_variant(variant).map(ChannelId::from)
    }
}
impl ToVariant for ChannelId {
    fn to_variant(&self) -> glib::Variant {
        AsRef::<str>::as_ref(self).to_variant()
    }
}
impl From<ChannelId> for glib::Variant {
    fn from(id: ChannelId) -> Self {
        id.to_variant()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VideoQuality {
    High,
    Medium,
    Low,
}

impl VideoQuality {
    pub fn default_playback() -> Self {
        match TvSettings::get().default_playback_quality() {
            DefaultPlaybackQuality::High => VideoQuality::High,
            DefaultPlaybackQuality::Medium => VideoQuality::Medium,
            DefaultPlaybackQuality::Low => VideoQuality::Low,
        }
    }
}

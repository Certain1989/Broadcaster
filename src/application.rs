// SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org>
// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
    sync::Arc,
};

use adw::{gio, glib, prelude::*, subclass::prelude::*};
use eyre::WrapErr;
use gettextrs::gettext;
use smart_default::SmartDefault;

use crate::{
    config::{APP_ID, PROFILE, VERSION},
    launcher::{ExternalProgram, ExternalProgramType, ProgramSelector},
    mpv,
    preferences::TvPreferencesDialog,
    settings::{TvSettings, VideoQuality},
    utils::{show_error, spawn, spawn_clone, tokio, AsyncResource},
    window::TvWindow,
    zapp::Zapp,
};

/// Main-thread request order shared by IPTV, Live and Mediathek.
#[derive(Debug, Default)]
pub(crate) struct PlaybackRequests {
    serial: Cell<u64>,
}

impl PlaybackRequests {
    fn begin(&self) -> u64 {
        let serial = self.serial.get().wrapping_add(1);
        self.serial.set(serial);
        serial
    }

    fn is_current(&self, serial: u64) -> bool {
        self.serial.get() == serial
    }
}

mod imp {

    use super::*;

    #[derive(Debug, SmartDefault)]
    pub struct TvApplication {
        #[default(Arc::new(Zapp::new().expect("failed to initialize Zapp client")))]
        pub(super) zapp: Arc<Zapp>,
        pub(super) live_channels: AsyncResource<LiveChannels>,
        pub(super) window: RefCell<Option<glib::WeakRef<TvWindow>>>,
        pub(super) playback_requests: PlaybackRequests,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TvApplication {
        const NAME: &'static str = "TvApplication";
        type Type = super::TvApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for TvApplication {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.setup_gactions();
        }
    }

    impl ApplicationImpl for TvApplication {
        fn activate(&self) {
            self.obj().window().present();
        }

        fn shutdown(&self) {
            mpv::terminate();
            self.parent_shutdown();
        }
    }

    impl GtkApplicationImpl for TvApplication {}
    impl AdwApplicationImpl for TvApplication {}
}

glib::wrapper! {
    pub struct TvApplication(ObjectSubclass<imp::TvApplication>)
        @extends gio::Application, gtk::Application, adw::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

thread_local! {
    static APPLICATION: OnceCell<TvApplication> = const { OnceCell::new() };
}

impl TvApplication {
    pub fn new() -> Self {
        let slf: Self = glib::Object::builder()
            .property("application-id", APP_ID)
            .property("resource-base-path", "/io/github/Certain1989/Broadcaster")
            .build();

        APPLICATION.with(|app| {
            app.set(slf.clone())
                .expect("TvApplication may only be created once")
        });

        let live_channels = slf.live_channels();
        live_channels.set_load_fn({
            let zapp = slf.zapp();
            move || {
                let zapp = zapp.clone();
                Box::pin(async move {
                    tokio(async move { zapp.channel_info_list().await })
                        .await
                        .map(Rc::new)
                        .map_err(|e| {
                            let e = e.wrap_err(gettext("Failed to load livestream channels"));
                            tracing::error!("Failed to load livestream channels (upstream diagnostics suppressed)");
                            Rc::new(e)
                        })
                })
            }
        });
        spawn(async move { live_channels.load() });

        slf
    }
    pub fn get() -> Self {
        APPLICATION.with(|app| {
            app.get()
                .expect("TvApplication::get() may only be called from the main thread")
                .clone()
        })
    }

    /// The connection to the session bus, established on first use.
    pub async fn dbus() -> eyre::Result<zbus::Connection> {
        static DBUS: tokio::sync::OnceCell<zbus::Connection> = tokio::sync::OnceCell::const_new();

        // concurrent first calls share one connection attempt
        tokio(async {
            DBUS.get_or_try_init(zbus::Connection::session)
                .await
                .cloned()
        })
        .await
        .wrap_err_with(|| gettext("Failed to connect to the session D-Bus"))
    }

    pub fn window(&self) -> TvWindow {
        let mut window = self.imp().window.borrow_mut();

        match window.as_ref().and_then(|p| p.upgrade()) {
            Some(window) => window,
            None => {
                let new_window = TvWindow::new(self);
                *window = Some(glib::clone::Downgrade::downgrade(&new_window));
                new_window
            }
        }
    }

    pub fn zapp(&self) -> Arc<Zapp> {
        self.imp().zapp.clone()
    }

    pub fn live_channels(&self) -> AsyncResource<LiveChannels> {
        self.imp().live_channels.clone()
    }

    /// Reserve before an asynchronous IPTV detail lookup so later requests win.
    pub fn begin_playback_request(&self) -> u64 {
        self.imp().playback_requests.begin()
    }

    pub fn playback_request_is_current(&self, serial: u64) -> bool {
        self.imp().playback_requests.is_current(serial)
    }

    pub async fn play(&self, video: VideoInfo) {
        let serial = self.begin_playback_request();
        self.play_requested(video, serial).await;
    }

    /// IPTV resolves metadata asynchronously; do not allocate a second request
    /// when its already-reserved request reaches this common playback boundary.
    pub async fn play_requested(&self, video: VideoInfo, serial: u64) {
        if !self.playback_request_is_current(serial) {
            return;
        }
        let Some(uri) = video.uri() else {
            show_error(&gettext("No playable video URL is available"));
            return;
        };

        let settings = TvSettings::get();

        if settings.use_external_player() {
            let player_name = settings.video_player_name();
            let player_id = settings.video_player_id();

            let player = if player_id.is_empty() {
                None
            } else {
                match ExternalProgram::find(player_name, player_id.clone()).await {
                    Ok(player) => player,
                    Err(_) => {
                        show_error(&gettext("Failed to find the configured video player"));
                        None
                    }
                }
            };

            let player = match player {
                Some(player) => player,
                None => {
                    match ProgramSelector::select_program(ExternalProgramType::Player, player_id)
                        .await
                    {
                        Some(player) => {
                            settings.set_video_player_name(&player.name);
                            settings.set_video_player_id(&player.id);

                            player
                        }
                        None => return,
                    }
                }
            };

            if !self.playback_request_is_current(serial) {
                return;
            }
            match player.open(uri.to_owned()).await {
                Ok(()) => (),
                Err(_) => show_error(&gettext("Failed to play video stream")),
            }
        } else {
            if !self.playback_request_is_current(serial) {
                return;
            }
            // The managed player is a separate process. Capture the source view
            // before launching, then restore it only when its process exits.
            let window = self.window();
            let token = window.begin_playback();
            let weak_window = glib::clone::Downgrade::downgrade(&window);
            if let Err(e) = mpv::play(
                video.title(),
                uri,
                video.subtitle_url(),
                video.stream_headers(),
                serial,
                move || {
                    if let Some(window) = weak_window.upgrade() {
                        window.finish_playback(token);
                    }
                },
            )
            .await
            {
                // A spawn failure has no watcher. Discard this context without
                // allowing an older playback request to override newer state.
                window.finish_playback(token);
                if e != mpv::StartError::Superseded && self.playback_request_is_current(serial) {
                    show_error(&e.message());
                }
            }
        }
    }

    pub async fn download(&self, uri: String) {
        let settings = TvSettings::get();
        let downloader_name = settings.video_downloader_name();
        let downloader_id = settings.video_downloader_id();

        let downloader = if downloader_id.is_empty() {
            None
        } else {
            match ExternalProgram::find(downloader_name, downloader_id.clone()).await {
                Ok(downloader) => downloader,
                Err(_) => {
                    show_error(&gettext("Failed to find the configured video downloader"));
                    None
                }
            }
        };

        let downloader = match downloader {
            Some(downloader) => downloader,
            None => {
                match ProgramSelector::select_program(
                    ExternalProgramType::Downloader,
                    downloader_id,
                )
                .await
                {
                    Some(downloader) => {
                        settings.set_video_downloader_name(&downloader.name);
                        settings.set_video_downloader_id(&downloader.id);

                        downloader
                    }
                    None => return,
                }
            }
        };

        match downloader.open(uri).await {
            Ok(()) => (),
            Err(_) => show_error(&gettext("Failed to launch video downloader")),
        }
    }

    fn setup_gactions(&self) {
        let quit_action = gio::ActionEntry::builder("quit")
            .activate(move |app: &Self, _, _| app.quit())
            .build();
        let about_action = gio::ActionEntry::builder("about")
            .activate(move |app: &Self, _, _| app.show_about())
            .build();
        let preferences_action = gio::ActionEntry::builder("preferences")
            .activate(move |app: &Self, _, _| {
                TvPreferencesDialog::new().present(Some(&app.window()))
            })
            .build();
        let download_action = gio::ActionEntry::builder("download")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(move |app: &Self, _, variant| {
                if let Some(stream_url) = variant.and_then(|v| v.get()) {
                    spawn_clone!(app => app.download(stream_url))
                }
            })
            .build();

        self.add_action_entries([
            quit_action,
            about_action,
            preferences_action,
            download_action,
        ]);

        self.set_accels_for_action("app.quit", &["<primary>q"]);
        self.set_accels_for_action("app.preferences", &["<primary>comma"]);
        self.set_accels_for_action("window.close", &["<primary>w"]);
        self.set_accels_for_action("window.reload", &["<primary>r", "F5", "Reload"]);
        self.set_accels_for_action("window.search", &["<primary>f"]);
        self.set_accels_for_action("window.add-playlist", &["<primary>o"]);
        self.set_accels_for_action("window.manage-sources", &["<primary><shift>m"]);
        self.set_accels_for_action("window.toggle-fullscreen", &["F11"]);
        // like switching tabs, Ctrl+L and Ctrl+T are reserved for the
        // location and new tabs in GNOME
        self.set_accels_for_action("window.show-live", &["<alt>1"]);
        self.set_accels_for_action("window.show-mediathek", &["<alt>2"]);
        self.set_accels_for_action("window.show-streams", &["<alt>3"]);
    }

    fn show_about(&self) {
        let window = self.window();
        // About uses a private icon name deliberately distinct from the
        // desktop launcher: Flatpak renames the installed launcher PNG and
        // a stale hicolor cache can resolve its original name to a missing file.
        // Register the private name against this dialog's actual display.
        let display = gtk::prelude::WidgetExt::display(&window);
        if !register_about_icon(&display) {
            tracing::warn!("Broadcaster About icon resource could not be loaded by GTK");
        }

        let about = adw::AboutDialog::from_appdata(
            "/io/github/Certain1989/Broadcaster/io.github.Certain1989.Broadcaster.metainfo.xml",
            (PROFILE == "Release").then_some(VERSION),
        );

        about.set_version(VERSION);
        // translators: replace with your names, one per line
        about.set_translator_credits(&gettext("translator-credits"));
        about.set_application_icon(ABOUT_ICON_NAME);
        about.present(Some(&window));
    }
}

/// An application-private icon name. The original launcher icon still uses
/// the App ID and Flatpak's rename-icon rule; this one never touches hicolor.
const ABOUT_ICON_NAME: &str = "io.github.Certain1989.Broadcaster-about";
const ABOUT_ICON_RESOURCES: &str = "/io/github/Certain1989/Broadcaster/icons";
const ABOUT_ICON_DIRECT: &str = "/io/github/Certain1989/Broadcaster/icons/128x128/apps";

fn register_about_icon(display: &gtk::gdk::Display) -> bool {
    let theme = gtk::IconTheme::for_display(display);

    // GTK looks for hicolor subdirectories under the normal resource root,
    // and also supports the direct directory as a last-resort fallback.
    for path in [ABOUT_ICON_RESOURCES, ABOUT_ICON_DIRECT] {
        if !theme
            .resource_path()
            .iter()
            .any(|entry| entry.as_str() == path)
        {
            theme.add_resource_path(path);
        }
    }
    if !theme.has_icon(ABOUT_ICON_NAME) {
        return false;
    }

    // has_icon() only checks theme metadata. The previous four attempts
    // passed that check while GTK later tried a missing /app/share icon.
    // Resolve the actual GFile and decode its bytes, requiring a resource
    // URI so an obsolete disk icon cannot falsely satisfy this check.
    let paintable = theme.lookup_icon(
        ABOUT_ICON_NAME,
        &[],
        128,
        1,
        gtk::TextDirection::None,
        gtk::IconLookupFlags::empty(),
    );
    let Some(file) = paintable.file() else {
        return false;
    };
    if !file.has_uri_scheme("resource") {
        tracing::warn!("About icon lookup unexpectedly selected {}", file.uri());
        return false;
    }
    let Ok((bytes, _)) = file.load_bytes(None::<&gio::Cancellable>) else {
        return false;
    };
    gtk::gdk::Texture::from_bytes(&bytes).is_ok()
}

#[cfg(test)]
mod about_icon_runtime_tests {
    use super::*;

    #[test]
    #[ignore = "needs a live GDK display; CI runs it under xvfb-run"]
    fn icon_theme_resolves_about_icon_on_open_display() {
        let data = glib::Bytes::from_static(crate::GRESOURCE_BYTES);
        let resource = gio::Resource::from_data(&data).expect("bundled GResource is valid");
        gio::resources_register(&resource);
        gtk::init().expect("GTK must initialize with an active test display");

        let display = gtk::gdk::Display::default().expect("GDK display is open");
        assert!(
            register_about_icon(&display),
            "About must load the actual unique icon resource, not a stale file"
        );
        let icon_theme = gtk::IconTheme::for_display(&display);
        let paintable = icon_theme.lookup_icon(
            ABOUT_ICON_NAME,
            &[],
            128,
            1,
            gtk::TextDirection::None,
            gtk::IconLookupFlags::empty(),
        );
        let file = paintable.file().expect("resolved icon must have a source");
        assert!(
            file.has_uri_scheme("resource"),
            "About must not load an on-disk launcher icon: {}",
            file.uri()
        );
        let (bytes, _) = file
            .load_bytes(None::<&gio::Cancellable>)
            .expect("resolved icon source must be readable");
        let texture = gtk::gdk::Texture::from_bytes(&bytes)
            .expect("resolved icon source must decode to an image");
        assert_eq!(texture.width(), 128);
        assert_eq!(texture.height(), 128);
    }
}

/// The livestream channels, or why they couldn't be loaded.
pub type LiveChannels = Result<Rc<crate::zapp::ChannelInfoList>, Rc<eyre::Report>>;

/// Fixed, parser-recognized hints for one imported stream only.
/// Never use derive(Debug): these values may include private identifiers.
#[derive(Clone, Default)]
pub struct StreamHeaders {
    pub(crate) user_agent: Option<String>,
    pub(crate) referer: Option<String>,
}

impl StreamHeaders {
    pub(crate) fn imported(user_agent: Option<String>, referer: Option<String>) -> Self {
        Self {
            user_agent,
            referer,
        }
    }
}

impl std::fmt::Debug for StreamHeaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StreamHeaders(<redacted>)")
    }
}

#[derive(Debug)]
pub enum VideoInfo {
    Live {
        title: String,
        uri: String,
    },
    Stream {
        title: String,
        uri: String,
        headers: StreamHeaders,
    },
    Mediathek {
        title: String,
        preferred_quality: VideoQuality,
        uri_high: Option<String>,
        uri_medium: Option<String>,
        uri_low: Option<String>,
        subtitle_url: Option<String>,
    },
}

impl VideoInfo {
    /// The external-program launcher deliberately ignores imported hints.
    pub fn stream_headers(&self) -> Option<&StreamHeaders> {
        match self {
            VideoInfo::Stream { headers, .. } => Some(headers),
            VideoInfo::Live { .. } | VideoInfo::Mediathek { .. } => None,
        }
    }

    pub fn subtitle_url(&self) -> Option<&str> {
        match self {
            VideoInfo::Live { .. } | VideoInfo::Stream { .. } => None,
            VideoInfo::Mediathek { subtitle_url, .. } => subtitle_url.as_deref(),
        }
    }

    pub fn title(&self) -> &str {
        match self {
            VideoInfo::Live { title, .. }
            | VideoInfo::Stream { title, .. }
            | VideoInfo::Mediathek { title, .. } => title,
        }
    }

    /// Choose the preferred quality if available, otherwise the nearest
    /// available alternative. Do not panic on missing backend video URLs.
    pub fn uri(&self) -> Option<&str> {
        match self {
            VideoInfo::Live { uri, .. } | VideoInfo::Stream { uri, .. } => {
                (!uri.is_empty()).then_some(uri.as_str())
            }
            VideoInfo::Mediathek {
                preferred_quality,
                uri_high,
                uri_medium,
                uri_low,
                ..
            } => {
                let high = uri_high.as_deref().filter(|url| !url.is_empty());
                let medium = uri_medium.as_deref().filter(|url| !url.is_empty());
                let low = uri_low.as_deref().filter(|url| !url.is_empty());

                match preferred_quality {
                    VideoQuality::High => high.or(medium).or(low),
                    VideoQuality::Medium => medium.or(high).or(low),
                    VideoQuality::Low => low.or(medium).or(high),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{VideoInfo, VideoQuality};

    #[test]
    fn imported_stream_keeps_existing_player_contract() {
        let video = VideoInfo::Stream {
            title: "Fabricated Example".into(),
            uri: "https://example.invalid/demo.m3u8".into(),
            headers: super::StreamHeaders::default(),
        };
        assert_eq!(video.title(), "Fabricated Example");
        assert_eq!(video.uri(), Some("https://example.invalid/demo.m3u8"));
        assert_eq!(video.subtitle_url(), None);
        assert!(video.stream_headers().is_some());
        let empty = VideoInfo::Stream {
            title: "Empty".into(),
            uri: String::new(),
            headers: super::StreamHeaders::default(),
        };
        assert_eq!(empty.uri(), None);
    }

    #[test]
    fn missing_preferred_quality_falls_back_to_an_available_stream() {
        let video = VideoInfo::Mediathek {
            title: "Example".into(),
            preferred_quality: VideoQuality::High,
            uri_high: None,
            uri_medium: Some("medium".into()),
            uri_low: Some("low".into()),
            subtitle_url: None,
        };
        assert_eq!(video.uri(), Some("medium"));

        let video = VideoInfo::Mediathek {
            title: "Example".into(),
            preferred_quality: VideoQuality::Low,
            uri_high: Some("high".into()),
            uri_medium: None,
            uri_low: None,
            subtitle_url: None,
        };
        assert_eq!(video.uri(), Some("high"));
    }

    #[test]
    fn delayed_iptv_requests_cannot_overtake_live_or_mediathek() {
        let requests = super::PlaybackRequests::default();
        let pending_a = requests.begin();
        assert!(requests.is_current(pending_a));
        let live = requests.begin();
        assert!(
            !requests.is_current(pending_a),
            "delayed A must not replace Live"
        );
        assert!(requests.is_current(live));
        let pending_b = requests.begin();
        let mediathek = requests.begin();
        assert!(
            !requests.is_current(pending_b),
            "delayed B must not replace Mediathek"
        );
        assert!(!requests.is_current(live));
        assert!(requests.is_current(mediathek));
        let pending_c = requests.begin();
        let pending_d = requests.begin();
        assert!(!requests.is_current(pending_c), "later IPTV selection wins");
        assert!(requests.is_current(pending_d));
    }

    #[test]
    fn no_video_url_returns_none_instead_of_panicking() {
        let video = VideoInfo::Mediathek {
            title: "Unavailable".into(),
            preferred_quality: VideoQuality::High,
            uri_high: Some(String::new()),
            uri_medium: None,
            uri_low: None,
            subtitle_url: None,
        };
        assert_eq!(video.uri(), None);
        let video = VideoInfo::Live {
            title: "Unavailable".into(),
            uri: String::new(),
        };
        assert_eq!(video.uri(), None);
    }
}

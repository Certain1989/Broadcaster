// SPDX-FileCopyrightText: 2026 Televido contributors
// SPDX-FileContributor: Claude (Anthropic)
// SPDX-License-Identifier: GPL-3.0-or-later

//! Video playback with mpv.
//!
//! One managed mpv subprocess is active at a time. Replacing playback closes
//! the old player first, and the watch task drains output and reports errors.

use std::{cell::RefCell, ffi::OsStr, rc::Rc, time::Duration};

use adw::{gio, glib, prelude::*};
use futures_util::{
    future::{select, Either},
    lock::Mutex,
};
use gettextrs::gettext;

use crate::config::PKGDATADIR;
use crate::{
    application::{StreamHeaders, TvApplication},
    settings::TvSettings,
    utils::spawn,
    window::TvWindow,
};

pub const MPV: &str = "mpv";

/// Only application-owned categories may reach the UI. GLib process
/// errors and mpv output may contain the media URL or its credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    MissingPlayer,
    MissingRemoteResources,
    InvalidMediaUrl,
    ConflictingArguments,
    LaunchFailed,
}

impl StartError {
    pub fn message(self) -> String {
        match self {
            Self::MissingPlayer => {
                gettext("mpv is not installed. Broadcaster needs the mpv video player.")
            }
            Self::MissingRemoteResources => {
                gettext("Broadcaster's packaged mpv remote-control resources are missing.")
            }
            Self::InvalidMediaUrl => gettext("The video or subtitle URL is invalid."),
            Self::ConflictingArguments => gettext("Remove the mpv argument separator from custom player options to use stream headers."),
            Self::LaunchFailed => gettext("Failed to start mpv. Check the player installation."),
        }
    }
}

/// Do not allow literal control characters through to mpv command-line values.
/// Media URLs themselves remain opaque: Broadcaster must not reject supported
/// protocols, and the `--` argument already prevents option injection.
fn valid_media_inputs(uri: &str, subtitles: Option<&str>) -> bool {
    let valid = |value: &str| !value.trim().is_empty() && !value.chars().any(char::is_control);
    valid(uri) && subtitles.is_none_or(valid)
}

/// Mpv 0.41 has dedicated --user-agent and --referrer network options.
/// Imported metadata cannot select generic flags or http-header-fields.
fn safe_header_value(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.trim() == value
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

/// Recheck persisted metadata at the subprocess boundary; URL parsers may
/// normalize control characters, so run byte validation before URL parsing.
fn safe_referer(value: &str) -> bool {
    if !safe_header_value(value, 8192) {
        return false;
    }
    reqwest::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.as_str() == value
    })
}

/// One controlled argv element per validated channel-only option.
/// A bad imported hint is ignored with a value-free warning.
fn stream_header_options(headers: Option<&StreamHeaders>) -> Vec<String> {
    let Some(headers) = headers else {
        return Vec::new();
    };
    let mut options = Vec::with_capacity(2);
    if let Some(value) = headers.user_agent.as_deref() {
        if safe_header_value(value, 1024) {
            options.push(format!("--user-agent={value}"));
        } else {
            tracing::warn!("Ignoring unsafe imported User-Agent hint");
        }
    }
    if let Some(value) = headers.referer.as_deref() {
        if safe_referer(value) {
            options.push(format!("--referrer={value}"));
        } else {
            tracing::warn!("Ignoring unsafe imported Referer hint");
        }
    }
    options
}

/// A user's global argument separator would turn later generated options
/// into positional inputs. Refuse only the conflicting combination.
fn global_separator_conflicts(user_arguments: &[String], header_options: &[String]) -> bool {
    !header_options.is_empty() && user_arguments.iter().any(|arg| arg.trim() == "--")
}

/// Build a fresh argv per process, with safe per-stream flags after the
/// user's existing global settings but before the -- media separator.
fn player_argv(
    user_arguments: &[String],
    input_argument: &str,
    script_argument: &str,
    subtitles_argument: Option<&str>,
    title_argument: &str,
    header_options: &[String],
    uri: &str,
) -> Vec<String> {
    let mut args = vec![MPV.to_owned()];
    args.extend(
        user_arguments
            .iter()
            .map(|argument| argument.trim())
            .filter(|argument| !argument.is_empty())
            .map(str::to_owned),
    );
    args.extend([
        OSC_MOUSE_THRESHOLD.to_owned(),
        input_argument.to_owned(),
        script_argument.to_owned(),
    ]);
    args.extend(subtitles_argument.map(str::to_owned));
    args.extend(header_options.iter().cloned());
    args.push(title_argument.to_owned());
    args.push("--".to_owned());
    args.push(uri.to_owned());
    args
}

/// SIGTERM, lets mpv shut down gracefully.
const SIGTERM: i32 = 15;
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(3);
/// mpv exit code when quitting due to a signal or the quit key bindings.
const EXIT_CODE_QUIT: i32 = 4;
/// The bundled MPV 0.41 OSC starts on every synthetic mouse-motion event by
/// default, including the first event from a newly created Wayland window.
/// Ignore tiny/initial movement; deliberate mouse use still reveals stock OSC.
const OSC_MOUSE_THRESHOLD: &str = "--script-opt=osc-minmousemove=18";

/// The running mpv process.
struct Current {
    process: gio::Subprocess,
}

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
    // GLib callbacks may request playback concurrently while stop() awaits.
    static PLAY_LOCK: Rc<Mutex<()>> = Rc::new(Mutex::new(()));
}

pub fn is_installed() -> bool {
    glib::find_program_in_path(MPV).is_some()
}

/// Plays `uri` in a new mpv process, replacing the currently running one.
pub async fn play(
    title: &str,
    uri: &str,
    subtitles: Option<&str>,
    stream_headers: Option<&StreamHeaders>,
    on_exit: impl FnOnce() + 'static,
) -> Result<(), StartError> {
    // Keep stop/start atomic across asynchronous playback requests.
    let lock = PLAY_LOCK.with(Clone::clone);
    let _guard = lock.lock().await;
    stop().await;
    start(title, uri, subtitles, stream_headers, on_exit)
}

fn start(
    title: &str,
    uri: &str,
    subtitles: Option<&str>,
    stream_headers: Option<&StreamHeaders>,
    on_exit: impl FnOnce() + 'static,
) -> Result<(), StartError> {
    if !valid_media_inputs(uri, subtitles) {
        return Err(StartError::InvalidMediaUrl);
    }
    if !is_installed() {
        return Err(StartError::MissingPlayer);
    }

    let settings = TvSettings::get();
    let arguments = settings.mpv_arguments();
    let header_options = stream_header_options(stream_headers);
    // A global -- would make generated flags positional filenames. Refuse
    // that combination without changing the user's saved mpv settings.
    if global_separator_conflicts(&arguments, &header_options) {
        return Err(StartError::ConflictingArguments);
    }
    let title_argument = format!("--force-media-title={title}");
    let subtitles_argument = subtitles.map(|url| format!("--sub-file={url}"));

    // These bindings only apply to Broadcaster's managed mpv process.
    // Do not modify or depend on the user's global mpv input.conf.
    let input_conf = std::path::Path::new(PKGDATADIR).join("broadcaster-mpv-input.conf");
    let script = std::path::Path::new(PKGDATADIR).join("broadcaster-mpv-remote.lua");

    // Do not silently start an unresponsive remote-only player if a staged
    // native install or Flatpak omits either required resource.
    if !input_conf.is_file() || !script.is_file() {
        return Err(StartError::MissingRemoteResources);
    }

    let input_argument = format!("--input-conf={}", input_conf.display());
    let script_argument = format!("--script={}", script.display());

    // Preserve the native remote and OSC flags, GLib subprocess lifetime,
    // global user options and the media separator.
    let command = player_argv(
        &arguments,
        &input_argument,
        &script_argument,
        subtitles_argument.as_deref(),
        &title_argument,
        &header_options,
        uri,
    );
    let argv: Vec<&OsStr> = command.iter().map(OsStr::new).collect();

    // User-provided mpv flags, titles, media and subtitle URLs can contain
    // credentials or identifying parameters. Never print argv values.
    tracing::debug!("{}", safe_command_summary(&argv));

    let process = gio::Subprocess::newv(
        &argv,
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE,
    )
    .map_err(|_| StartError::LaunchFailed)?;

    CURRENT.with(|current| {
        current.replace(Some(Current {
            process: process.clone(),
        }))
    });
    spawn(watch(process, title.to_owned(), on_exit));

    Ok(())
}

/// Terminates the running mpv process and waits until it has exited.
async fn stop() {
    let Some(current) = CURRENT.with(|current| current.take()) else {
        return;
    };

    current.process.send_signal(SIGTERM);
    let exited = select(
        Box::pin(current.process.wait_future()),
        Box::pin(glib::timeout_future(TERMINATE_TIMEOUT)),
    )
    .await;

    if let Either::Right(_) = exited {
        tracing::warn!("mpv did not exit after SIGTERM, killing it");
        current.process.force_exit();
        let _ = current.process.wait_future().await;
    }
}

/// Terminates the running mpv process without waiting, e.g. on shutdown.
pub fn terminate() {
    if let Some(current) = CURRENT.with(|current| current.take()) {
        current.process.send_signal(SIGTERM);
    }
}

fn is_current(process: &gio::Subprocess) -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(|current| &current.process == process)
    })
}

/// Keeps the application alive while mpv is running and reports errors.
async fn watch(process: gio::Subprocess, title: String, on_exit: impl FnOnce() + 'static) {
    let _hold = TvApplication::get().hold();

    // Do not allow a full output pipe to block playback or shutdown.
    drain_process_output(&process).await;
    let _ = process.wait_future().await;

    // terminated on purpose by `stop()` or `terminate()`
    if !is_current(&process) {
        return;
    }
    CURRENT.with(|current| {
        current.take();
    });

    // This is the actual end of the current player, not the return from
    // play() (which only launches it). Replacement/shutdown watchers above
    // are stale and must never restore an older playback context.
    on_exit();

    if !process.has_exited() {
        return;
    }
    let status = process.exit_status();
    if status == 0 || status == EXIT_CODE_QUIT {
        return;
    }

    // Subprocess output is deliberately discarded, not filtered: arbitrary
    // mpv diagnostics may reproduce credential-bearing playback URLs.
    tracing::error!("mpv playback failed (exit status {status}; diagnostics suppressed)");
    let reason = safe_failure_reason(status);

    if let Some(window) = TvApplication::get()
        .active_window()
        .and_downcast::<TvWindow>()
    {
        window.add_toast(
            adw::Toast::builder()
                .title(
                    // translators: `{channel}` is replaced by the channel name or the title of the show, `{error}` by the error message of mpv
                    gettext("mpv could not play “{channel}”: {error}")
                        .replace("{channel}", &title)
                        .replace("{error}", &reason),
                )
                .use_markup(false)
                .timeout(10)
                .build(),
        );
    }
}

/// Consume subprocess output without retaining or forwarding untrusted bytes.
async fn drain_process_output(process: &gio::Subprocess) {
    if let Some(stdout) = process.stdout_pipe() {
        loop {
            match stdout
                .read_bytes_future(4096, glib::Priority::DEFAULT)
                .await
            {
                Ok(bytes) if !bytes.is_empty() => (),
                _ => break,
            }
        }
    }
}

/// The failure category is intentionally derived only from the exit status,
/// never from mpv's arbitrary output.
fn safe_failure_reason(status: i32) -> String {
    format!("exit status {status}")
}

/// A deliberately value-free startup diagnostic: even user-supplied mpv flags
/// or optional subtitle URLs can embed authentication tokens.
fn safe_command_summary(argv: &[&OsStr]) -> String {
    format!(
        "starting mpv with {} arguments (values redacted)",
        argv.len()
    )
}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn hint_validation_rejects_controls_and_bad_referers() {
        for invalid in [
            "",
            " leading",
            "trailing ",
            "bad\rvalue",
            "bad\nvalue",
            "bad\0value",
            "bad\tvalue",
            "☃",
        ] {
            assert!(!safe_header_value(invalid, 1024));
        }
        assert!(!safe_header_value(&"A".repeat(1025), 1024));
        for invalid in [
            "",
            "ftp://example.invalid/",
            "http://",
            "https://user:fake@example.invalid/",
            "https://example.invalid/#fragment",
            "https://example.invalid/\r\nInjected: true",
        ] {
            assert!(!safe_referer(invalid));
        }
        assert!(safe_header_value("Test Agent/1.0", 1024));
        assert!(safe_referer("https://example.invalid/watch?x=1"));
    }

    #[test]
    fn header_flags_are_single_args_and_follow_global_preferences() {
        let a = StreamHeaders {
            user_agent: Some("--script=not-an-option".into()),
            referer: Some("https://example.invalid/page".into()),
        };
        let options = stream_header_options(Some(&a));
        assert_eq!(
            options,
            [
                "--user-agent=--script=not-an-option",
                "--referrer=https://example.invalid/page",
            ]
        );
        assert!(global_separator_conflicts(&["--".to_owned()], &options));
        assert!(!global_separator_conflicts(&["--vo=gpu".to_owned()], &options));
        assert!(!global_separator_conflicts(&["--".to_owned()], &[]));
        let global = vec!["--user-agent=global".into(), "  ".into()];
        let argv = player_argv(
            &global,
            "--input-conf=/tmp/remote.conf",
            "--script=/tmp/remote.lua",
            None,
            "--force-media-title=Example",
            &options,
            "https://example.invalid/video?token=fake",
        );
        assert!(
            argv.iter()
                .position(|x| x == "--user-agent=global")
                .unwrap()
                < argv
                    .iter()
                    .position(|x| x == "--user-agent=--script=not-an-option")
                    .unwrap()
        );
        assert_eq!(argv[argv.len() - 2], "--");
        assert_eq!(
            argv[argv.len() - 1],
            "https://example.invalid/video?token=fake"
        );
        assert!(argv.contains(&"--input-conf=/tmp/remote.conf".to_owned()));
        assert!(argv.contains(&"--script=/tmp/remote.lua".to_owned()));
        assert_eq!(
            argv.iter().filter(|x| x.starts_with("--referrer=")).count(),
            1
        );
    }

    #[test]
    fn channel_a_b_live_mediathek_a_has_no_stale_header_flags() {
        let a = StreamHeaders {
            user_agent: Some("Channel-A/1".into()),
            referer: Some("https://example.invalid/a".into()),
        };
        let b = StreamHeaders::default();
        let a1 = stream_header_options(Some(&a));
        let b1 = stream_header_options(Some(&b));
        let live = stream_header_options(None);
        let mediathek = stream_header_options(None);
        let a2 = stream_header_options(Some(&a));
        assert_eq!(a1, a2);
        assert_eq!(a1.len(), 2);
        assert!(b1.is_empty());
        assert!(live.is_empty());
        assert!(mediathek.is_empty());
        let unsafe_hint = StreamHeaders {
            user_agent: Some("x\n--profile=bad".into()),
            referer: Some("file:///etc/passwd".into()),
        };
        assert!(stream_header_options(Some(&unsafe_hint)).is_empty());
    }

    #[test]
    fn private_osc_startup_option_preserves_mouse_controller() {
        assert_eq!(OSC_MOUSE_THRESHOLD, "--script-opt=osc-minmousemove=18");
        assert!(!OSC_MOUSE_THRESHOLD.contains("osc=no"));
        assert!(!OSC_MOUSE_THRESHOLD.contains("osc-visibility=never"));
    }

    #[test]
    fn malformed_media_inputs_are_rejected_without_protocol_restrictions() {
        assert!(valid_media_inputs(
            "https://example.invalid/video?token=fake",
            None
        ));
        assert!(valid_media_inputs(
            "rtmp://example.invalid/live",
            Some("file:///subs.srt")
        ));
        assert!(!valid_media_inputs("", None));
        assert!(!valid_media_inputs("--vo=null\n--script=bad", None));
        assert!(!valid_media_inputs(
            "https://example.invalid/video",
            Some("url\runsafe")
        ));
    }

    #[test]
    fn failures_have_only_code_owned_diagnostic_categories() {
        let private_url = "https://user:fake-password@example.invalid/video?token=fake-secret";
        let synthetic_mpv_output = format!("Failed to open {private_url}\nHTTP 403\r{private_url}");
        let private_query = "https://example.invalid/live?a=fake-secret&b=one&c=two";
        // The watcher never passes mpv output to logging or error display:
        // only status and a fixed message are used for the failure category.
        let status = 2;
        let message = format!("mpv playback failed (exit status {status}; diagnostics suppressed)");
        let reason = safe_failure_reason(status);
        for secret in ["fake-password", "fake-secret", "user:", "example.invalid"] {
            assert!(synthetic_mpv_output.contains(secret) || private_query.contains(secret));
            assert!(!message.contains(secret));
            assert!(!reason.contains(secret));
        }
        for error in [
            StartError::MissingPlayer,
            StartError::MissingRemoteResources,
            StartError::InvalidMediaUrl,
            StartError::ConflictingArguments,
            StartError::LaunchFailed,
        ] {
            assert!(!error.message().contains("fake-secret"));
        }
    }

    #[test]
    fn discarded_subprocess_output_does_not_block_later_playback() {
        // A genuine pipe-heavy subprocess, using only fabricated values.
        // The test shell is a fixture, not a media-launch implementation.
        let context = glib::MainContext::new();
        context.block_on(async {
            let scripts = [
                (
                    r#"i=0; while [ "$i" -lt 4000 ]; do printf '%s\n' 'https://fake-user:fake-pass@example.invalid/stream?token=fake'; i=$((i+1)); done; exit 9"#,
                    9,
                ),
                ("printf '%s\\n' 'normal playback'; exit 0", 0),
            ];
            for (script, expected_status) in scripts {
                let process = gio::Subprocess::newv(
                    &[OsStr::new("sh"), OsStr::new("-c"), OsStr::new(script)],
                    gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE,
                )
                .expect("test fixture must launch");
                // If the output is not drained, the first subprocess would
                // block on its pipe long before reaching its exit status.
                drain_process_output(&process).await;
                process.wait_future().await.expect("test fixture must exit");
                assert_eq!(process.exit_status(), expected_status);
            }
        });
    }

    #[test]
    fn startup_diagnostic_never_contains_media_or_option_tokens() {
        let argv = [
            OsStr::new("mpv"),
            OsStr::new("--sub-file=https://example.invalid/sub?token=private-secret"),
            OsStr::new("https://example.invalid/video?token=private-secret"),
        ];
        let message = safe_command_summary(&argv);
        assert!(message.contains("3 arguments"));
        assert!(!message.contains("private-secret"));
        assert!(!message.contains("example.invalid"));
    }
}

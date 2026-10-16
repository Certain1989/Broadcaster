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
use crate::{application::TvApplication, settings::TvSettings, utils::spawn, window::TvWindow};

pub const MPV: &str = "mpv";

/// SIGTERM, lets mpv shut down gracefully.
const SIGTERM: i32 = 15;
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(3);
/// Number of output bytes kept for error messages.
const OUTPUT_TAIL_SIZE: usize = 8 * 1024;
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
    on_exit: impl FnOnce() + 'static,
) -> eyre::Result<()> {
    // Keep stop/start atomic across asynchronous playback requests.
    let lock = PLAY_LOCK.with(Clone::clone);
    let _guard = lock.lock().await;
    stop().await;
    start(title, uri, subtitles, on_exit)
}

fn start(
    title: &str,
    uri: &str,
    subtitles: Option<&str>,
    on_exit: impl FnOnce() + 'static,
) -> eyre::Result<()> {
    if !is_installed() {
        eyre::bail!(gettext(
            "mpv is not installed. Broadcaster needs the mpv video player."
        ));
    }

    let settings = TvSettings::get();
    let arguments = settings.mpv_arguments();
    let title_argument = format!("--force-media-title={title}");
    let subtitles_argument = subtitles.map(|url| format!("--sub-file={url}"));

    // These bindings only apply to Broadcaster's managed mpv process.
    // Do not modify or depend on the user's global mpv input.conf.
    let input_conf = std::path::Path::new(PKGDATADIR).join("broadcaster-mpv-input.conf");
    let script = std::path::Path::new(PKGDATADIR).join("broadcaster-mpv-remote.lua");

    // Do not silently start an unresponsive remote-only player if a staged
    // native install or Flatpak omits either required resource.
    if !input_conf.is_file() || !script.is_file() {
        eyre::bail!(
            "Broadcaster MPV remote resources are missing: {} or {}",
            input_conf.display(),
            script.display()
        );
    }

    let input_argument = format!("--input-conf={}", input_conf.display());
    let script_argument = format!("--script={}", script.display());

    let argv: Vec<&OsStr> = [MPV]
        .into_iter()
        .chain(
            arguments
                .iter()
                .map(|argument| argument.trim())
                .filter(|argument| !argument.is_empty()),
        )
        // Keep the stock mouse OSC enabled, but avoid opening it on the
        // synthetic initial mouse event when the player window is created.
        // --script-opt adds one option (unlike --script-opts, which replaces
        // the whole list) and does not suppress user's other script options.
        .chain([
            OSC_MOUSE_THRESHOLD,
            input_argument.as_str(),
            script_argument.as_str(),
        ])
        .chain(subtitles_argument.iter().map(String::as_str))
        .chain([title_argument.as_str(), "--", uri])
        .map(OsStr::new)
        .collect();

    // User-provided mpv flags, titles, media and subtitle URLs can contain
    // credentials or identifying parameters. Never print argv values.
    tracing::debug!("{}", safe_command_summary(&argv));

    let process = gio::Subprocess::newv(
        &argv,
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE,
    )
    .map_err(|e| eyre::eyre!(e).wrap_err(gettext("Failed to start mpv")))?;

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

    // Always drain the output, otherwise mpv blocks once the pipe is full.
    let mut tail: Vec<u8> = Vec::new();
    if let Some(stdout) = process.stdout_pipe() {
        loop {
            match stdout
                .read_bytes_future(4096, glib::Priority::DEFAULT)
                .await
            {
                Ok(bytes) if !bytes.is_empty() => {
                    tail.extend_from_slice(&bytes);
                    if tail.len() > OUTPUT_TAIL_SIZE {
                        tail.drain(..tail.len() - OUTPUT_TAIL_SIZE);
                    }
                }
                _ => break,
            }
        }
    }

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

    let output = String::from_utf8_lossy(&tail);
    tracing::error!("mpv exited with status {status}:\n{output}");

    let reason = output
        .lines()
        .flat_map(|line| line.split('\r'))
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with("Exiting..."))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("exit status {status}"));

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
    fn private_osc_startup_option_preserves_mouse_controller() {
        assert_eq!(OSC_MOUSE_THRESHOLD, "--script-opt=osc-minmousemove=18");
        assert!(!OSC_MOUSE_THRESHOLD.contains("osc=no"));
        assert!(!OSC_MOUSE_THRESHOLD.contains("osc-visibility=never"));
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

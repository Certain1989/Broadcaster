<!-- SPDX-FileCopyrightText: David Cabot <d-k-bo@mailbox.org> -->
<!-- SPDX-FileCopyrightText: 2026 Televido contributors -->
<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-FileContributor: Claude (Anthropic) -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster

**English | [Deutsch](README.de.md)**

Broadcaster is a lightweight, native Linux application for watching German public-service television: search Mediathek archives, browse live channels and play programmes with mpv. Its GTK4/libadwaita interface works with a mouse and keyboard or a directional television remote, including on Wayland/KDE Plasma Bigscreen.

## What it does

- **Mediathek:** search German-language broadcaster catalogues through MediathekViewWeb. Filter by channel or duration, sort results, choose stream quality, and open programme details.
- **Live TV:** browse current programmes from the Zapp service, read programme descriptions, and select which live channels appear and in what order.
- **My Channels (v0.2.0, in development):** add local M3U files or HTTP(S) playlist addresses, manage multiple sources, filter by group/language/country/region, edit channel metadata without losing it on refresh, and play streams with the same mpv controls. Channel logos are fetched only as needed and may fall back to a generic icon.
- **TV controls:** fullscreen startup, remote-friendly navigation, and a dedicated mpv overlay for play/pause, seeking, playback speed, audio tracks and subtitles. Labels reflect available stream metadata.
- **Desktop integration:** native GTK4 interface, English and German text, optional external player and downloader, and Flatpak builds for x86-64 and ARM64.

To find playlists for **My Channels**, browse [IPTV-org's country, language and category playlists](https://github.com/iptv-org/iptv/blob/master/PLAYLISTS.md). The Add Playlist dialog links to the same third-party directory; nothing is subscribed automatically. Imported logos are cached on disk (up to 2,048 images or 256 MiB) and unreferenced images are removed after successful playlist refreshes or source deletion; memory textures remain deliberately small to keep remote input responsive.

Broadcaster neither hosts nor supplies media. Stream availability, channel metadata, subtitles, regional restrictions and service uptime are controlled by broadcasters and external catalogue providers. Remote key mapping depends on your desktop and hardware; Fedora KDE Plasma Bigscreen/Wayland is the primary tested TV environment. Other Linux desktops may work but are not guaranteed.

## Install v0.1.1

Download the `broadcaster-0.1.1-x86_64.flatpak` bundle for Intel/AMD PCs or `broadcaster-0.1.1-aarch64.flatpak` for ARM64 systems, **together with `SHA256SUMS`**, from the [v0.1.1 GitHub release](https://github.com/Certain1989/Broadcaster/releases/tag/v0.1.1). Run the following in the download directory (substitute `aarch64` everywhere for ARM64):

```bash
sha256sum -c SHA256SUMS
flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak install --user ./broadcaster-0.1.1-x86_64.flatpak
flatpak run io.github.Certain1989.Broadcaster
```

The checksum command checks both named files, so download **both** architectures or, if you downloaded only your own, use `grep 'broadcaster-0.1.1-x86_64.flatpak$' SHA256SUMS | sha256sum -c -` (replace `x86_64` with `aarch64` as appropriate). For removal use `flatpak uninstall --user io.github.Certain1989.Broadcaster`. To install system-wide for a separate TV account, use the equivalent `sudo flatpak remote-add --system ...`, `sudo flatpak install --system ./broadcaster-0.1.1-x86_64.flatpak`, and `sudo flatpak uninstall --system ...` operations; start the app without sudo.

Broadcaster requires the GNOME Platform 51 runtime, which Flatpak can download from Flathub. **Broadcaster itself is not distributed via Flathub.** Standalone Flatpak bundles do **not** configure automatic application updates; install later verified releases manually. The stable ID above is distinct from `io.github.Certain1989.Broadcaster.Devel` used for CI development builds.

### Black video after installing the Flatpak

The GNOME Platform 51 Flatpak uses mpv with the runtime's FFmpeg libraries. H.264 playback requires the matching Freedesktop codec extension, `org.freedesktop.Platform.codecs-extra//26.08-extra`. Flatpak normally installs this automatically, but an incomplete installation can result in **audio playing while the video stays black**. Codec extensions are versioned by runtime: earlier Broadcaster builds using GNOME Platform 50 require `25.08-extra` instead; merely having the newer extension installed cannot satisfy an older runtime.

Check the installed extensions and whether the running Broadcaster sandbox mounts `codecs-extra`:

```bash
flatpak list --runtime --columns=application,branch,installation | grep -E 'codecs-extra|org.gnome.Platform'
flatpak run --command=sh io.github.Certain1989.Broadcaster \
  -c "grep '^runtime-extensions=' /.flatpak-info"
```

If the matching `26.08-extra` extension is absent for this build, install it from Flathub and retry video playback:

```bash
flatpak install --user flathub org.freedesktop.Platform.codecs-extra//26.08-extra
```

For a system-wide Flatpak installation, use `--system` instead of `--user` (with Flathub configured system-wide). A per-user Broadcaster installation can also use a matching system-installed codec extension; **Broadcaster itself does not need to be installed system-wide**. If the extension is present and mounted but video still fails, report the Flatpak/runtime versions and a test with a public or synthetic video, not private stream URLs or credentials.

## Build from source (Fedora example)

```bash
sudo dnf install git rust cargo meson ninja-build blueprint-compiler gettext \
  gtk4-devel libadwaita-devel glib2-devel openssl-devel pkgconf-pkg-config \
  desktop-file-utils appstream mpv
git clone https://github.com/Certain1989/Broadcaster.git
cd Broadcaster
meson setup _build -Dprofile=development --prefix="$HOME/.local"
meson compile -C _build
meson test -C _build --print-errorlogs
meson install -C _build
GSETTINGS_SCHEMA_DIR="$HOME/.local/share/glib-2.0/schemas" \
  "$HOME/.local/bin/broadcaster"
```

Use `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, `lua tests/mpv-remote.lua`, and `reuse lint` for further checks. See [contribution guidance](CONTRIBUTING.md). Native builds use the host's mpv; release Flatpaks bundle mpv with Lua support.

## Playback and known limitations

On the tested Bigscreen setup, OK/Enter activates items and opens the playback controls; arrows navigate, Back closes open overlays or returns to the previous view, and Home/Stop leave playback. With the playback overlay hidden, the directional arrows can seek. F11 or the application menu toggles saved fullscreen state. Exact bindings depend on your remote and compositor.

The stock mpv mouse overlay can briefly flash at startup on some Wayland sessions; this is an accepted cosmetic limitation. Audio/subtitle labels are only as accurate as the stream metadata. The application interface is provided in English and German. Other translations are not currently shipped.

## Development, maintenance and licensing

Substantial implementation, testing and review used large language models. Automated checks and practical testing do not constitute independent certification or a guarantee against defects.

Broadcaster is a hobby project provided **as-is**, subject to the applicable licence and law. There is no promised issue response time, maintenance schedule, future feature development or universal platform compatibility. Please remove credentials, tokens and private stream URLs from issue reports. See [security reporting](SECURITY.md) and the [technical overview](docs/maintenance-baseline.md).

Application code is distributed under **GPL-3.0-or-later**, with some separately licensed assets and MIT-licensed inherited material. The subdued Broadcaster app icon was generated with ChatGPT and is offered under CC BY 4.0 to the extent protectable rights exist; this does not grant trademark or endorsement rights. See `COPYING`, `LICENSES/`, `REUSE.toml` and [artwork and attribution](docs/artwork-and-attribution.md).

Broadcaster incorporates and adapts code from [Televido](https://github.com/d-k-bo/televido) and [Televido-Fritz](https://github.com/45054/televido-fritz), originally under GPL-3.0-or-later. Original copyright notices and applicable third-party licences are retained. These upstream projects do not endorse Broadcaster; this clean public source repository does not contain their complete inherited Git history.

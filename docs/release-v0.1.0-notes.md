<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.1.0

First public source release of Broadcaster, a lightweight Rust/GTK4 application for German public-service television on Linux desktops and remote-controlled TV interfaces.

## Features

- MediathekViewWeb search, broadcaster/duration/sort filters and playback-quality choices.
- Zapp live-channel catalogue, programme details and configurable channel visibility and order.
- mpv playback with TV remote overlay, seeking, audio/subtitle selection, playback speed and fullscreen.
- English and German interfaces; Flatpak bundles for x86-64 and ARM64.

## Installation

Download your architecture's Flatpak bundle plus `SHA256SUMS` from this release. Verify the checksum, install the matching Flatpak, and launch it:

```bash
grep 'broadcaster-0.1.0-x86_64.flatpak$' SHA256SUMS | sha256sum -c -
flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak install --user ./broadcaster-0.1.0-x86_64.flatpak
flatpak run io.github.Certain1989.Broadcaster
```

For ARM64 replace `x86_64` with `aarch64`. Uninstall with `flatpak uninstall --user io.github.Certain1989.Broadcaster`. GNOME Platform 50 may be obtained from Flathub; **Broadcaster itself is not on Flathub**. Installing a standalone bundle does not configure automatic updates.

## Limitations and provenance

The app depends on external services and stream metadata. Some Wayland sessions briefly display the stock mpv controls on start. Remote mappings depend on hardware. English and German are the maintained UI languages.

Substantial portions were developed and reviewed using LLM assistance. Broadcaster is shared as-is, without a promised maintenance schedule or issue response time. Application code is GPL-3.0-or-later; third-party and icon licences are documented in `REUSE.toml`, `LICENSES/` and `docs/artwork-and-attribution.md`. Original upstream attribution remains intact.

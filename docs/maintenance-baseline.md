<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Technical overview and maintenance notes

This document describes the public v0.1.0 design. It is not a support promise or a claim of exhaustive testing.

- `src/mediathek/` integrates MediathekViewWeb search, filtering, pagination, programme metadata and quality choices. Catalogue and API changes may affect results.
- `src/live/` and `src/zapp.rs` integrate broadcaster livestream listings, current programme information and user-selected channel order.
- `src/mpv.rs` and `data/broadcaster-mpv-{input.conf,remote.lua}` manage the mpv subprocess and remote-friendly playback controls.
- `src/window.rs`, `src/application.rs`, `src/preferences/` and `data/resources/broadcaster.css` manage the GTK4/libadwaita UI and remote focus routing.
- `Cargo.toml`, `meson.build`, `build.rs` and `build-aux/*.json` describe native and Flatpak builds. The development app ID ends in `.Devel`.

The test workflow includes Rust formatting/Clippy/locked tests, GTK tests under Xvfb, Lua overlay tests, Meson, REUSE, and Flatpak checks for x86-64 and ARM64. Use `.github/workflows/` for the exact commands. Stream providers and remote hardware cannot be fully emulated in CI; actual programme playback needs manual smoke testing.

Known limitations include the brief stock mpv OSC flash on some Wayland starts, incomplete older translations beyond English/German, and audio/subtitle labels that can only use metadata provided by media sources. Remote-control keys vary with desktop and hardware. Broadcaster is supplied as-is with no guaranteed fix schedule or issue response time.

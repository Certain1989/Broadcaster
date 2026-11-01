<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Technical overview and maintenance notes

This is a **v0.2.5 architecture overview** for the public Broadcaster application, not a support commitment or a claim of exhaustive testing. Older release notes and checklists describe their respective historical versions.

## Components and behaviour

- `src/mediathek/` integrates MediathekViewWeb search, filtering, pagination, programme metadata and playback-quality choices. Upstream catalogue and API changes can affect results.
- `src/live/` and `src/zapp.rs` provide broadcaster livestream listings, programme progress, descriptions and user-selected channel visibility/order. The Live data source remains separate from IPTV XMLTV guides.
- `src/streams/` implements **My Channels**: local and HTTP(S) M3U sources, manual playlist refresh, source-scoped catalogue reconciliation, per-channel metadata overrides, logos, search/facets, and SQLite-backed persistence. Suspicious shrinkage and local-file reauthorization use explicit user flows; ordinary failed refreshes must not erase last-good channels.
- `src/epg/` handles explicitly configured local/HTTP(S) XMLTV guides, bounded import and cache, the optional automatic per-guide refresh scheduler, shared default and playlist-specific selection, deterministic matching, manual aliases and now/next queries. New guide dialogs preselect automatic refresh (24-hour age threshold, configurable 6–168 hours); the scheduler checks due, enabled sources while the app is running. M3U-advertised guide URLs are not automatically followed and adding a guide alone does not select it for playlists.
- Exact channel IDs and certain corroborated identity variants can match automatically; names yield suggestions that must be confirmed. Manual mapping and regional overrides remain available. Failure or absence of guide data must not block ordinary IPTV or Mediathek playback.
- `src/keyboard.rs` and `src/keyboard/` supply an optional, on-demand remote-operated text keyboard, with launchers integrated across multiple editable contexts; see [OSK verification](../tests/osk/README.md).
- `src/window.rs`, `src/application.rs`, `src/preferences/`, `src/streams/view.rs` and `data/resources/broadcaster.css` handle GTK4/libadwaita views, directional focus and overlay navigation.
- `src/mpv.rs` and `data/broadcaster-mpv-{input.conf,remote.lua}` manage playback and remote controls. Imported IPTV User-Agent/Referer hints apply only to managed IPTV playback, not to Live, Mediathek or an external player.

## Build, validation and packaging

`Cargo.toml`, `Cargo.lock`, `meson.build`, `build.rs` and `build-aux/*.json` define Rust, native and Flatpak builds. Production and development app IDs differ (development ends in `.Devel`); production Flatpak bundles are distributed through GitHub Releases, not Flathub.

The repository includes Rust format/Clippy/locked tests, GTK/Xvfb native navigation tests, Lua mpv tests, REUSE checks, and Flatpak packaging validation. CI provides a faster iteration path plus fuller integration/release workflows; inspect [the workflows](../.github/workflows/) and each particular run for exact checks and actual outcomes. Automated checks cannot establish physical remote behaviour, provider availability, or end-to-end audiovisual playback on every machine.

## Boundaries and limitations

Broadcaster is a hobby project supplied **as-is**, with no guaranteed issue response time, maintenance schedule or future feature commitment. It provides a Now/Next display, **not** a dedicated multi-day EPG grid; Windows and Android ports are not claimed. XMLTV associations, playlists, streaming media, network endpoints and provider rights remain user/provider responsibilities. Current known UI limitations include a brief stock mpv overlay flash on some Wayland sessions; audio/subtitle labels depend on supplied stream metadata. English and German are the maintained interface languages. Remote key mappings vary with the desktop, compositor and physical device.

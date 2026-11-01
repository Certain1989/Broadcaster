<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.2.5 — release notes

Broadcaster v0.2.5 adds optional electronic programme-guide information to **My Channels**, improves navigation with television remotes, and retains Mediathek search, Live TV, user-managed M3U playlists and mpv playback.

## Changes

- **XMLTV guides:** Add multiple guides from local XMLTV files or explicitly configured HTTP(S) addresses. Refresh on demand or use automatic per-guide refresh (enabled by default in the Add Guide dialog, with a 24-hour age threshold adjustable to 6–168 hours). Broadcaster checks due guides while running, including when the guide service starts; it does not refresh them after the app closes. Guide import failures do not prevent normal playback or Mediathek operations; last-good guide data can remain available where applicable.
- **Explicit guide selection:** Choose a guide as the **Shared Default** for playlists or bind a guide individually to a playlist. Merely adding an XMLTV source does not associate it with your playlists. A single shared guide may serve multiple playlists.
- **Now/Next:** My Channels displays current and upcoming programme information when guide data and a usable channel match exist. Exact IDs and certain corroborated identity variants match automatically; name-based suggestions do not become bindings without confirmation. Users may need to confirm or manually override uncertain matches, particularly regional variants.
- **Favourites:** Star channels and filter to favourites. Playlist stream addresses and playback headers remain separate from programme-guide associations.
- **Remote and keyboard controls:** Additional corrections to card actions, source and guide management, editor focus, favourites access, and the shared on-screen keyboard.
- **Compatibility and durability:** Updated compatible Rust dependencies, XMLTV parser adaptation and extended negative/capacity regression coverage.

## Installation

Download only published production Flatpak bundles and the matching `SHA256SUMS` for your architecture (x86_64 or aarch64). Verify the checksum before installing. Installation and runtime guidance is in the [English README](../README.md) and [German README](../README.de.md). Production application bundles are distributed via GitHub Releases rather than Flathub; GNOME Platform 51 runtime dependencies may be installed from Flathub.

## Scope and limitations

Broadcaster does not provide or endorse playlists, guide sources or media. Configure only sources you may lawfully use. M3U playlist refresh remains manual and independent of guide scheduling. Network XMLTV retrieval occurs only for guides deliberately configured by the user; playlist import alone does not authorize guide downloads. External guide providers may observe requests to their services.

XMLTV and channel metadata can be incomplete or inaccurate. Matching is deliberately conservative and may need manual intervention. Now/Next is not a full schedule-grid EPG. This release does not claim Windows or Android support or promise particular provider availability, maximum dataset performance or universal device compatibility. Existing Televido-derived code, third-party assets and contributors retain their notices and applicable licences.

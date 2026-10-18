<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.2.0 — feature and packaging notes

v0.2.0 was released on October 18, 2026. Download verified Flatpak bundles and
`SHA256SUMS` from the [v0.2.0 release](https://github.com/Certain1989/Broadcaster/releases/tag/v0.2.0).
The README contains the current installation instructions.

## My Channels

- Import local M3U files with the system file picker or add HTTP(S) M3U URLs.
  Manage multiple named sources and their enabled status, ordering and defaults.
- Search, sort and filter imported channels by source, group, language, country,
  and (when available) region. Explicit unknown metadata is filterable.
- Edit channel names and effective metadata. User overrides persist across
  ordinary playlist refreshes. Suspicious catalogue shrinkage requires explicit
  confirmation; failure should retain the previous working channels.
- Play imported HTTP(S) streams via Broadcaster's managed mpv playback path,
  with return-to-list navigation and directional remote controls.
- For visible rows only, attempt to fetch bounded PNG, JPEG or WebP channel
  logos. Unsupported or unavailable logos leave the existing generic icon.
  No channel requires a logo to play.

## Packaging, permissions and limitations

- Current Flatpak permissions allow network access, but do not grant blanket
  access to the host home directory. Local files are chosen through GTK/GIO
  and may require reauthorization if a document-portal grant expires.
- Playlist downloads are user-initiated. There is no background refresh,
  EPG, recording, DRM handling or IPTV provider login facility.
- Sources and overrides are stored in a local SQLite database in the user's
  application data directory; existing application preferences remain intact.
- Importing playlists cannot guarantee that their streams are reachable or
  licensed for viewing. Broadcaster does not supply channel subscriptions.
- The published release includes production Flatpak bundles for x86_64 and
  aarch64. Release automation passed, but this is not a claim of comprehensive
  testing across all TV remotes, compositors or devices.

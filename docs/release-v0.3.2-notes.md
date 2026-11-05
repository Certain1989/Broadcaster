<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.3.2 — release notes

Broadcaster v0.3.2 is a **maintenance and playback-control update** over v0.3.1. It brings the bundled media and Rust/SQLite dependencies forward and improves directional navigation in the on-screen mpv controls. It does not introduce new programme sources, screens, automatic app updates or new network permissions.

## Changes

- **Spatial TV-remote navigation in mpv:** The D-pad now moves to the nearest suitably aligned control in the actual on-screen layout, instead of following a three-button/five-button row index. For example, Down from Play/Pause selects Speed; Down from the left/right seek controls selects Audio/Exit. Horizontal navigation stays within the visible row without wrapping. Timeline seeking, OK, Back/Home, submenus, audio/subtitle choices, fullscreen and auto-hide retain their existing roles.
- **Media runtime refresh:** The standalone Flatpak bundles mpv **0.42.0**, libplacebo **7.387.0** and a pinned current LuaJIT 2.1 development commit. Both production and development manifests use matching source revisions. The media stack retains the existing stream-header/HLS handling and remote-control script.
- **Rust and database refresh:** Updates include rusqlite **0.41.1** with bundled SQLite **3.54.1**, zbus **6.2.1** and compatible locked Rust dependency versions. Existing supported databases and exports, including v0.3.1 data, are intended to remain usable after upgrade. The backup/import interaction and retained migration recovery copies are unchanged.
- **Validation coverage:** More checks cover old SQLite databases and exports, network consent after restart, media HTTP headers and redirects, certificate rejection, real LuaJIT loading and spatial remote behavior. These are regression safeguards, not new permissions or a claim of support for every stream provider.

## HTTPS compatibility note

mpv 0.42 uses stricter HTTPS certificate verification. A stream served using a **self-signed, expired or otherwise untrusted certificate** may now fail to play even if it worked with an older mpv. This is not a change to the separate playlist-fetch LAN consent rules. Check the source's certificate chain and configure an appropriate trusted CA where justified; **do not disable TLS verification globally**. Compatibility of individual private IPTV servers remains dependent on their configuration.

## Upgrade and installation

**A normal v0.3.1 → v0.3.2 upgrade does not require database export/import, uninstallation or deleting application data.** Close Broadcaster, install the verified new production bundle in the same `--user` or `--system` installation scope, and restart. Backups remain advisable for important data. `Import and Replace Database` is a separate, explicitly confirmed full replacement after restart—not part of normal upgrading.

Once **v0.3.2 has actually been published**, obtain `broadcaster-0.3.2-x86_64.flatpak` or `broadcaster-0.3.2-aarch64.flatpak` together with `SHA256SUMS` from the [v0.3.2 GitHub release](https://github.com/Certain1989/Broadcaster/releases/tag/v0.3.2). Verify the bundle checksum, then for example run `flatpak install --user --or-update ./broadcaster-0.3.2-x86_64.flatpak` (substitute the other architecture where applicable). See [installation instructions](../README.md#install-broadcaster) and [release verification](release-verification.md). **Release-candidate CI artifacts are not tagged, production-attested releases.** No claim of published v0.3.2 assets is made by this document alone.

Broadcaster continues to use **GNOME Platform 51**; H.264 playback may require `org.freedesktop.Platform.codecs-extra//26.08-extra`. Distribution remains **standalone Flatpak bundles on GitHub Releases**, not Flathub, and does not supply an independently signed automatic-update remote.

## Scope and limitations

Playlist, guide and logo destination checks and source-specific private-LAN opt-in remain as documented in [network trust](network-trust.md). Playlist permission does **not** automatically permit nested streams, HLS requests or external players; those consumers have their own network behavior. Managed mpv HTTP(S) stream and header checks are not a universal sandbox or SSRF guarantee. Stream availability, subtitles and TLS configuration remain provider-dependent.

SQLite backups and historical migration recovery files may contain private URLs or tokens. Keep them confidential; do not attach raw databases or playlists to public bug reports. Scheduled playlist/guide refresh runs only while the app is open. See the [v0.3.1 release notes](release-v0.3.1-notes.md) for the previous security and consent improvements.

This hobby project is offered as-is without a promised support or release cadence. Please [report security problems privately](../SECURITY.md) where possible.

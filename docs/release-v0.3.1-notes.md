<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.3.1 — release notes

Broadcaster v0.3.1 is primarily a **security, stability and release-maintenance update** to v0.3.0, not a new screen or feature milestone. It strengthens handling of untrusted network input and playlist permissions, extends regression coverage, and improves release verification. The existing Mediathek, Live TV, My Channels, XMLTV Now/Next, favourites, scheduled refresh and backup/restore functions remain available.

## Changes

- **Untrusted playback and external targets:** Tighter HTTP(S) URL admission before handing provider-supplied media, website/download or subtitle references to managed mpv or a configured external application. Unsafe primary schemes, local-file references and embedded URL user information are rejected. IPTV stream imports retain their separate HTTP(S) admission checks.
- **Network fetching:** Playlist, XMLTV guide and logo fetchers apply checked destination rules across redirects and DNS resolutions, with bounded and source-specific access rules. Logos remain public-network-only. Private-LAN playlist access requires a deliberate, source-specific opt-in; guides retain their separate explicitly configured LAN rules.
- **Private-LAN playlist consent and revocation:** Each remote playlist starts with private-LAN access **disabled**. On **Add Playlist** or **Edit Source Defaults**, use **Allow this playlist to connect to private LAN addresses (explicit opt-in)** only for a trusted playlist URL. Permission is tied to the selected source and exact URL; replacing the URL requires a new decision. A separate durable denial record prevents a revoked permission from being restored by stale GSettings consent after an ordinary process restart. If a revocation cannot be persisted, Broadcaster reports the failure and denies access for that running process. **If all persistent writes fail, a previously saved grant can reappear after restart**: retry the revocation once storage is writable, before closing the app.
- **Scheduled playlist refresh:** Optional automatic M3U refresh remains **off by default** and runs only while Broadcaster is open. Scheduled requests must satisfy the source's *current* network authorization, including after restart or revocation; a stale saved setting alone does not grant LAN access. Unexpectedly large list shrinkage still requires manual review.
- **EPG, storage and reliability:** Additional storage/EPG profiling and maintenance checks, source-management and remote-navigation regression coverage, and more robust handling of potentially hostile or incomplete data.
- **Release and dependency assurance:** Release-candidate verification includes locked Rust checks, RustSec advisory auditing, REUSE licensing checks, native validation and both production Flatpak architectures. Tagged releases use additional artifact checks and GitHub build provenance, **provided the tagged release workflow completes successfully**. These are supply-chain checks, not a promise of independent code certification or automatic updates.

## Upgrade and installation

When this release is published, download **`broadcaster-0.3.1-x86_64.flatpak`** or **`broadcaster-0.3.1-aarch64.flatpak`** and the matching `SHA256SUMS` from [Broadcaster v0.3.1 on GitHub Releases](https://github.com/Certain1989/Broadcaster/releases/tag/v0.3.1). Check the selected bundle against the checksum file, then use `flatpak install --user --or-update ./broadcaster-0.3.1-x86_64.flatpak` (substitute `aarch64` where appropriate). For an existing system-wide installation, use `sudo flatpak install --system --or-update ...` and keep that same installation scope. See the [English installation instructions](../README.md#install-broadcaster), the [German README](../README.de.md) and [release verification instructions](release-verification.md).

**Upgrading from v0.3.0 does not require database export/import, uninstallation or deleting app data.** Supported existing data remains in its normal Flatpak application location. An explicitly confirmed **Import and Replace Database** operation is different: it replaces the entire database on the next restart, not just selected entries. Historical migration recovery copies remain on disk intentionally.

The Flatpak uses **GNOME Platform 51**. H.264 playback may need the corresponding `org.freedesktop.Platform.codecs-extra//26.08-extra` extension. Broadcaster is distributed as **standalone GitHub Releases Flatpak bundles**, not as a Flathub application. No independently signed automatic-update remote is shipped: later releases must be installed manually. Release provenance and checksums can be checked using [the documented workflow](release-verification.md); the historical [v0.3.0 notes](release-v0.3.0-notes.md) remain available.

## Scope and limitations

Private-LAN consent covers **playlist fetching** under its documented rules; it does not automatically authorize every URL inside the playlist, nested HLS/DASH/media resource, external player or downloader. These consumers and their own network behavior are not fully sandboxed by the playlist fetch policy. This is not an absolute SSRF-prevention or storage/power-failure guarantee. Certain protected addresses and unsafe redirects remain denied even with consent; some corporate proxies may not be usable by the guarded fetchers. See [network trust details](network-trust.md) and the [threat model](threat-model.md).

External M3U/XMLTV files, provider streams, media players and programme data are outside Broadcaster's control. URLs, source tokens and HTTP headers can be sensitive; SQLite exports and retained historical recovery copies can preserve them. Broadcaster cannot guarantee provider availability, complete EPG information or compatibility with every remote. Playlist and guide schedules do not run while the application is closed.

Broadcaster is a hobby project provided as-is, without guaranteed support or release cadence. Please [report security issues privately](../SECURITY.md) where possible and avoid posting source URLs, tokens, raw databases or confidential logs.

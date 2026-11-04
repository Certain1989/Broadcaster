<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster threat model (current shipped design)

This is a maintainer-oriented overview, not proof that every reachable input
has been audited or an assertion of support for particular private media
services. Network security policy is specific to each source and consumer.

## Assets and attackers

Protect user-selected source URLs, playlist tokens and HTTP headers, the
SQLite database and its exported or historical recovery copies, local file
access, credentials in process arguments or logs, and the host running mpv.
An attacker might control an M3U playlist, XMLTV file, logo URL, remote
redirect/DNS answer, public catalogue response, Git upstream dependency or
download mirror. A hostile process with the *same user identity* is outside
the strong filesystem isolation assumption for app-private data.

## Entry points, protections and residual risks

| Entry | Existing boundary | Residual concern |
| --- | --- | --- |
| Local or HTTP(S) M3U playlists | User-selected sources, bounded import and refresh, redirect handling and download limits | Credential-bearing URLs, changing DNS, unsolicited media URLs and user-approved local-network sources |
| XMLTV EPG imports | Explicit guide selection and refresh, independently bounded parsing/storage and network fetch rules | Feed size, intentionally different guide-vs-playlist LAN consent, malformed external metadata, storage pressure |
| Remote channel logos | Untrusted remote visual assets fetched on demand | Third-party tracking, image decoding and endpoint availability |
| MediathekViewWeb and Zapp catalogues | Separate provider integration and playback choices | Untrusted provider metadata and stream URLs; catalogue availability outside app control |
| mpv and optional external player/downloader | Playback is delegated to installed components | Media protocol handlers, subtitles, helper processes, user-configured players and host-level dependencies |
| SQLite database export/import | Explicit user action, compatibility checks, private restore/recovery handling | A chosen export folder may have broader access; exports and old recovery copies retain historical secrets |
| Flatpak bundles and dependencies | Pinned upstream source commits, locked Rust dependencies, release build validation and checksums | Build runner/upstream compromise, owner-account compromise, third-party runtime/media bugs and unsigned update distribution |

## Network/proxy boundaries

The code contains several independent clients and workflows; do not assume
one uniform URL, redirect or proxy policy applies to all of them. The
playlist and EPG fetchers with pinned DNS use reqwest clients configured
with `no_proxy()` to avoid a proxy bypassing peer-address validation.
This is a security tradeoff: enterprise proxy-only networks may be unable
to use those imports. It is **not** a statement that the entire application
or mpv disables proxies. Existing LAN consent and playback protocol behavior
must be changed only after explicit compatibility and security analysis.

## Release trust and reporting

GitHub Releases is the standalone Flatpak distribution path. The release
workflow validates artifacts and SHA256SUMS. Future GitHub artifact provenance
can bind a file digest to a repository/workflow/source ref, but does **not**
sign an OSTree remote, defend against a compromised maintainer control plane,
or imply an automatic update channel. See [verification](release-verification.md).
Privately report suspected issues per [SECURITY.md](../SECURITY.md), without
uploading sensitive playlist URLs or databases.

This document describes the current intended boundaries. It is not an
acceptance report for future changes to playback protocols, LAN policy,
SQLite defensive flags or release-signing keys.

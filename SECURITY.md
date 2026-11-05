<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Security policy and reports

## Private vulnerability reports

If you suspect a vulnerability in Broadcaster, use this repository's GitHub
**Security → Advisories → Report a vulnerability** facility if it is available.
If private reporting is unavailable, open a public issue requesting a *private
contact method*, without disclosing exploit details, secrets or identifying
URLs. Do not attach a private playlist, database, backup, authentication
headers, or a reproduction log containing credentials.

Broadcaster is a hobby project distributed as-is. Reports are welcome, but
there is **no guaranteed acknowledgement, fix deadline, release cadence or
continued maintenance commitment**. The maintainer determines the scope and
timing of any remediation.

## Release authenticity and supported distribution

Installable standalone x86-64 and AArch64 Flatpak bundles are distributed
through the project's GitHub Releases. No auto-update channel or independently
signed OSTree remote is promised. Watch the repository's Releases and Security
Advisories for announcements; there is no guarantee of notifications or updates.

A SHA256SUMS file detects a corrupted/downloaded asset against *that file*;
alone it does not authenticate the release publisher. Version-tag builds using the provenance-enabled workflow can carry GitHub
artifact attestations once their tag jobs actually complete successfully; the
historical v0.3.0 bundles have no such attestations. See [release verification](docs/release-verification.md)
for digest and workflow/source-identity checks and the limitations of this
trust model. Existing releases must not be assumed to have attestations.

## Data and network trust

Source playlists and XMLTV guides can contain credentials in URLs and may
reference third-party media, logos and endpoints. Backups and historical
recovery databases can retain these values. Keep them private, avoid sharing
raw debug logs or URLs, and remove tokens before opening any public issue.
Some pinned-DNS fetchers deliberately disable system proxy configuration;
other network requests or external playback tools may behave differently.
See [public threat model](docs/threat-model.md) for scope and remaining
external dependencies.

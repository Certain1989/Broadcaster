<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.1.1

Security, dependency and reliability maintenance for the existing v0.1.x Linux television application. This version supersedes v0.1.0; IPTV/M3U import belongs to a future release.

## Security and reliability updates

- **Safer playback diagnostics:** Broadcaster drains and discards managed mpv output instead of forwarding arbitrary player diagnostics to logs or error toasts. Startup failures retain application-owned, actionable error categories.
- **Bounded network responses:** Zapp channel and programme requests and the Mediathek channel catalogue use size-limited, chunk-based reading. Provider-supplied channel identifiers are encoded as a single URL path segment.
- **Safer display and error handling:** Remote programme markup is sanitized through maintained `ammonia 4.2.4`; network and catalogue errors are translated into trusted user-facing messages. Player command-line arguments remain separated from media URLs.
- **Dependency maintenance:** The vulnerable `html2pango → ammonia 3.x` chain was removed; Rust's resolved TLS dependency includes `rustls 0.23.46`; D-Bus integration uses `zbus 6.0.0`. Other Rust dependency updates are locked in `Cargo.lock`.
- **Release supply-chain protections:** The tagged-release pipeline enforces `cargo audit --deny warnings` as a blocking RustSec check, SHA-pins GitHub Actions, pins bundled native Flatpak sources to commits and verifies checksums of both architecture-specific bundles.

## Packaging and compatibility

- Production and development Flatpaks use **GNOME Platform 51**. They bundle mpv, LuaJIT, libass and libplacebo for x86-64 and AArch64.
- Live TV, Mediathek search, remote navigation, quality selection, subtitles, fullscreen behaviour and optional external-player integration retain their existing interfaces.
- The matching Freedesktop H.264 codec extension for this runtime is `org.freedesktop.Platform.codecs-extra//26.08-extra`. An older Broadcaster installation using GNOME Platform 50 instead requires `25.08-extra`.
- A clean-install H.264 decode test without pre-existing codec extensions remains an outstanding hardware acceptance check; CI builds do not establish first-install codec resolution on every desktop.

## Install or upgrade

Download the `broadcaster-0.1.1-x86_64.flatpak` or `broadcaster-0.1.1-aarch64.flatpak` asset and `SHA256SUMS` from this release. On x86-64, in the download directory:

```bash
grep 'broadcaster-0.1.1-x86_64.flatpak$' SHA256SUMS | sha256sum -c -
flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak install --user ./broadcaster-0.1.1-x86_64.flatpak
flatpak run io.github.Certain1989.Broadcaster
```

Replace `x86_64` with `aarch64` for ARM64. For an existing system-wide installation, use `sudo flatpak install --system ./broadcaster-0.1.1-x86_64.flatpak` instead and launch the application as an ordinary user. Broadcaster is not distributed on Flathub; standalone Flatpak bundles do not configure automatic application updates.

## Verification and limitations

Tagged release assets are produced only after the release workflow's Rust formatting, Clippy, tests, strict RustSec audit, REUSE check, both production Flatpak builds and assembled checksum checks have succeeded. Check the workflow result for this tag before using the assets.

External broadcasters and players determine stream availability and diagnostics. Some Wayland sessions briefly display mpv's stock controls on startup. Remote mappings depend on hardware. If video is black while audio works, check the matching codec extension as described in the [installation guide](../README.md).

Substantial portions of Broadcaster were developed and reviewed using LLM assistance. The software is provided as-is, without guaranteed maintenance or security response times. See [SECURITY.md](../SECURITY.md) for responsible reporting, and remove credentials or private stream URLs from reports.

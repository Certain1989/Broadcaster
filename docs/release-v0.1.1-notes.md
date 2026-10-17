<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.1.1 (release preparation)

Security and reliability corrections for the existing v0.1.x application. This patch does not add IPTV imports or change the television navigation interface.

## Changes

- Managed mpv output is drained and discarded, rather than copied into error toasts or application logs. Failures retain a numeric exit status; missing player, invalid media input, absent remote resources, and launch failures retain specific messages.
- HTTP, programme catalogue, external-player and Mediathek errors are presented as trusted application messages instead of forwarding arbitrary diagnostic strings.
- Zapp responses are size-bounded; channel identifiers are encoded as a single URL path segment rather than concatenated into a request URL.
- The resolved Rustls dependency is updated to 0.23.46.
- Focused input-handling and diagnostic regression tests are included.

## Compatibility and verification

Existing Live, Mediathek, mpv control, subtitle, fullscreen, and remote navigation behaviour is intended to remain unchanged. Automated Rust, GTK, Lua, Meson, REUSE and both Flatpak-architecture checks must complete before publication; actual remote-controlled playback on target hardware requires a smoke test.

External media players, system components, and stream providers control their own diagnostics and logs; Broadcaster cannot guarantee that they will never expose an original media URL. Report security issues privately as described in [SECURITY.md](../SECURITY.md). Do not include private stream URLs or credentials in public bug reports.

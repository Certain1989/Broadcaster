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

For the GNOME Platform 50 Flatpak, H.264 decoding also depends on the matching `org.freedesktop.Platform.codecs-extra//25.08-extra` runtime extension. Flatpak normally installs it automatically, but a user installation was observed without usable H.264 decoding until the matching extension became available. Both v0.1.0 and the v0.1.1 candidate subsequently played video successfully; this was not established as a v0.1.1 regression. The English and German installation guides now describe the codec-extension check and recovery procedure.

A future clean-install acceptance check should use a **new user environment without previously installed system codecs-extra extensions**, confirm `25.08-extra` is installed and mounted, and decode a known-good local H.264 sample using the bundled mpv with `--hwdec=no` before testing network streams. Successful builds and playback on machines where the extension is already installed do not establish that first-install dependency resolution works. This check is outstanding and is not claimed to have run in CI.

External media players, system components, and stream providers control their own diagnostics and logs; Broadcaster cannot guarantee that they will never expose an original media URL. Report security issues privately as described in [SECURITY.md](../SECURITY.md). Do not include private stream URLs or credentials in public bug reports.

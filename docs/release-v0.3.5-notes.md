<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.3.5 — release notes

This maintenance release refreshes **38 semver-compatible locked Rust dependencies** and updates the pinned **LuaJIT** source used by both production and development Flatpak builds. It introduces no new application features.

## Security and compatibility

- **RustSec RUSTSEC-2026-0592:** `url` 2.5.8 → **2.5.9**, fixing a possible stack overflow in nested `blob:` URL origin calculation.
- **zbus** 6.3.0 → 6.5.0 and associated D-Bus dependencies; **regex** 1.13.1 → 1.13.2.
- GTK-related and network-stack dependencies receive compatible maintenance updates.
- **LuaJIT** advances 11 upstream `v2.1` commits, from `ea9eb7edb5eac4a9d175d74523db34fac0fa5180` to `96f7b297326e9fd454b7186c6858b2773f94a0dc`, in both Flatpak manifests. The mpv version remains **0.42.0**.
- `tower-http` remains at **0.6.11**, the latest version compatible with the current upstream `reqwest` 0.13.5 dependency requirement. No unsupported override has been made.

## Complete Rust dependency updates

These are the **38** package changes in `Cargo.lock` relative to v0.3.4, including transitive crates:

| Crate | Previous | Updated |
| --- | --- | --- |
| `bumpalo` | 3.20.3 | 3.20.4 |
| `bytes` | 1.12.1 | 1.12.2 |
| `cairo-rs` | 0.22.9 | 0.22.11 |
| `cc` | 1.11.0 | 1.11.3 |
| `cfg-expr` | 0.20.10 | 0.20.11 |
| `crc32fast` | 1.5.4 | 1.5.5 |
| `errno` | 0.3.14 | 0.3.15 |
| `form_urlencoded` | 1.2.2 | 1.2.3 |
| `gdk-pixbuf` | 0.22.0 | 0.22.11 |
| `gio` | 0.22.10 | 0.22.11 |
| `gio-sys` | 0.22.9 | 0.22.11 |
| `glib` | 0.22.10 | 0.22.11 |
| `glib-macros` | 0.22.9 | 0.22.11 |
| `glib-sys` | 0.22.9 | 0.22.11 |
| `hashbrown` | 0.17.1 | 0.17.2 |
| `itoa` | 1.0.18 | 1.0.19 |
| `mime` | 0.3.17 | 0.3.18 |
| `mio` | 1.2.4 | 1.2.5 |
| `pango-sys` | 0.22.9 | 0.22.11 |
| `pin-project-lite` | 0.2.17 | 0.2.18 |
| `regex` | 1.13.1 | 1.13.2 |
| `regex-automata` | 0.4.18 | 0.4.19 |
| `regex-syntax` | 0.8.11 | 0.8.12 |
| `rustls-pki-types` | 1.15.1 | 1.16.0 |
| `rustls-webpki` | 0.103.16 | 0.103.17 |
| `serde_spanned` | 1.1.2 | 1.1.3 |
| `smallvec` | 1.16.4 | 1.16.6 |
| `syn` | 3.0.9 | 3.0.10 |
| `toml` | 1.2.0+spec-1.1.0 | 1.2.2+spec-1.1.0 |
| `toml_datetime` | 1.1.2+spec-1.1.0 | 1.1.3+spec-1.1.0 |
| `toml_edit` | 0.25.18+spec-1.1.0 | 0.25.20+spec-1.1.0 |
| `toml_parser` | 1.1.5+spec-1.1.0 | 1.1.6+spec-1.1.0 |
| `url` | 2.5.8 | 2.5.9 |
| `zbus` | 6.3.0 | 6.5.0 |
| `zbus_macros` | 6.3.0 | 6.5.0 |
| `zcheapstr` | 1.2.0 | 1.4.0 |
| `zeroize` | 1.9.1 | 1.9.2 |
| `zruntime` | 0.2.2 | 0.2.4 |

## Upgrade and verification

No application database-schema migrations, new network permissions, Flatpak sandbox changes or application feature changes were introduced. The normal standalone Flatpak upgrade procedure remains unchanged. Preserve important application data as usual, and install in the same `--user` or `--system` scope as the existing installation.

Before publication, the dependency candidate passed the [locked RustSec audit](https://github.com/Certain1989/Broadcaster/actions/runs/45255038110), [Fast CI](https://github.com/Certain1989/Broadcaster/actions/runs/45255608459) and the owner's functional playback test; the separate [full integration and dual-architecture Flatpak checks](https://github.com/Certain1989/Broadcaster/actions/runs/45255659998) and the release-candidate workflow supply the final release gates. A test of an untagged candidate is not an attestation of a later tagged release.

See the [installation instructions](../README.md#install-broadcaster), [release verification guide](release-verification.md) and [previous v0.3.4 release notes](release-v0.3.4-notes.md).

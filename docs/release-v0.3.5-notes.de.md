<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.3.5 — Versionshinweise

Diese Wartungsversion aktualisiert **38 mit den bestehenden Versionsvorgaben kompatible Rust-Abhängigkeiten** sowie die festgelegte **LuaJIT**-Revision in den produktiven und Entwicklungs-Flatpak-Manifests. Neue Anwendungsfunktionen werden nicht eingeführt.

## Sicherheit und Kompatibilität

- **RustSec RUSTSEC-2026-0592:** `url` 2.5.8 → **2.5.9** behebt einen möglichen Stacküberlauf bei der Ursprungsbestimmung verschachtelter `blob:`-URLs.
- **zbus** 6.3.0 → 6.5.0 und zugehörige D-Bus-Abhängigkeiten; **regex** 1.13.1 → 1.13.2.
- Weitere kompatible Wartungsupdates betreffen GTK- und Netzwerkbibliotheken.
- **LuaJIT** wird in beiden Flatpak-Manifests um 11 Commits auf dem Upstream-Zweig `v2.1` aktualisiert: von `ea9eb7edb5eac4a9d175d74523db34fac0fa5180` auf `96f7b297326e9fd454b7186c6858b2773f94a0dc`. **mpv 0.42.0** bleibt unverändert.
- `tower-http` bleibt bei **0.6.11**, der neuesten mit den Vorgaben von `reqwest` 0.13.5 kompatiblen Version. Ein erzwungenes Upgrade findet nicht statt.

## Vollständige Rust-Abhängigkeitsupdates

Die folgenden **38** Einträge in `Cargo.lock` wurden gegenüber v0.3.4 aktualisiert; transitive Abhängigkeiten sind eingeschlossen:

| Crate | Bisher | Neu |
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

## Upgrade und Prüfung

Es wurden keine neuen Anwendungsfunktionen, Datenbankschema-Migrationen, Netzwerkberechtigungen oder Änderungen an den Flatpak-Sandboxrechten eingeführt. Der bisherige Upgradeweg für die eigenständigen Flatpak-Dateien bleibt unverändert. Wichtige Daten sollten gesichert bleiben; eine Aktualisierung erfolgt im selben `--user`- bzw. `--system`-Installationsbereich wie die vorhandene Installation.

Der Abhängigkeitskandidat hat die [RustSec-Prüfung](https://github.com/Certain1989/Broadcaster/actions/runs/45255038110), [Fast CI](https://github.com/Certain1989/Broadcaster/actions/runs/45255608459) und den Funktionstest des Eigentümers bestanden. Die [vollständigen Integrations- und Flatpak-Prüfungen für beide Architekturen](https://github.com/Certain1989/Broadcaster/actions/runs/45255659998) sowie die Release-Kandidatenprüfung bilden die abschließenden Freigabekriterien. Der Test eines ungetaggten Kandidaten ist keine Attestierung einer späteren Veröffentlichung.

Siehe die [Installationsanleitung](../README.de.md#broadcaster-installieren), die [Release-Prüfanleitung](release-verification.md) und die [Versionshinweise zu v0.3.4](release-v0.3.4-notes.de.md).

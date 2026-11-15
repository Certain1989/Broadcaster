<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.3.3 — Versionshinweise

Dieses Wartungsupdate aktualisiert sieben Rust-Pakete (fünf direkte und zwei transitive Abhängigkeiten):

- **ammonia:** 4.2.4 → 4.2.5 (Sicherheitskorrektur für die SVG-/MathML-HTML-Bereinigung; RUSTSEC-2026-0533).
- **flate2:** 1.1.10 → 1.1.11 (Korrekturen bei Komprimierung und Dekomprimierung).
- **libsqlite3-sys:** 0.39.1 → 0.40.0 (von rusqlite verwendete SQLite-Bindings; transitiv).
- **rusqlite:** 0.41.1 → 0.42.0 (Pflege der SQLite-Anbindung).
- **uuid:** 1.29.0 → 1.29.2 (Fehlerkorrekturen und Kompatibilitätsverbesserungen).
- **zbus:** 6.2.1 → 6.3.0 (Korrekturen für D-Bus-Verbindungen und -Protokoll).
- **zbus_macros:** 6.2.1 → 6.3.0 (passende prozedurale D-Bus-Makros; transitiv).

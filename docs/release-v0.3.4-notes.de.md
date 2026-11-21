<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.3.4 — Versionshinweise

Dieses Wartungsupdate aktualisiert gegenüber v0.3.3 zwei Rust-Abhängigkeiten:

- **uuid:** 1.29.2 → 1.29.3 (Verbesserungen bei der Serde-Deserialisierung und Kompatibilitätskorrekturen, unter anderem für eingebettete Systeme).
- **rusqlite:** 0.42.0 → 0.43.0 (Fehlerkorrekturen für Speicherzugriffe in mehreren SQLite-Erweiterungs-APIs, eine robustere Verarbeitung von Zeitwerten und die Zurückweisung leerer oder ausschließlich aus Kommentaren bestehender SQL-Anweisungen).

Die Abhängigkeitsaktualisierungen führen keine neuen Anwendungsfunktionen, Datenbankschema-Migrationen, Netzwerkberechtigungen oder Änderungen an der Medienlaufzeit ein. Vorhandene unterstützte Anwendungsdaten sollten nach einem normalen Flatpak-Update weiterhin verwendbar sein. Eine Sicherung wichtiger Daten bleibt empfehlenswert.

Die bisherige Verteilung als eigenständige Flatpak-Bundles und die Installationsschritte bleiben unverändert. Siehe die [Installationsanleitung](../README.de.md#broadcaster-installieren) und die [Hinweise zur Release-Prüfung](release-verification.md). Die älteren [Versionshinweise zu v0.3.3](release-v0.3.3-notes.de.md) bleiben verfügbar.

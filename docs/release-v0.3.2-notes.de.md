<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.3.2 — Versionshinweise

Broadcaster v0.3.2 ist ein **Wartungsupdate mit verbesserter Wiedergabesteuerung** gegenüber v0.3.1. Die mitgelieferten Medien- und Rust-/SQLite-Abhängigkeiten werden aktualisiert; die Richtungstasten bewegen den Fokus in der mpv-Bedienoberfläche genauer. Neue Senderquellen, Ansichten, automatische Anwendungsupdates oder zusätzliche Netzwerkberechtigungen gehören nicht dazu.

## Änderungen

- **Räumliche Navigation mit der Fernbedienung:** Die Richtungstasten wählen in der mpv-Einblendung nun die nächstgelegene passend ausgerichtete Schaltfläche anhand ihrer tatsächlichen Position. Statt die Spaltennummer zwischen einer Dreier- und Fünferreihe zu übernehmen, führt beispielsweise Unten von Wiedergabe/Pause zu Geschwindigkeit, von den linken/rechten Sprungtasten zu Audio/Beenden. Innerhalb einer Reihe springt der Fokus nicht über den Rand. Zeitleiste und Suchsprünge, OK, Zurück/Home, Untermenüs, Tonspuren, Untertitel, Vollbild und automatisches Ausblenden bleiben erhalten.
- **Aktualisierte Medienbibliotheken:** Das Flatpak enthält mpv **0.42.0**, libplacebo **7.387.0** und einen festgelegten Entwicklungsstand von LuaJIT 2.1. Produktions- und Entwicklungsmanifest verwenden übereinstimmende Quellrevisionen. Die bisherigen Regeln für HTTP-Header, HLS und die mpv-Fernbedienung bleiben bestehen.
- **Rust und Datenbank:** Unter anderem werden rusqlite auf **0.41.1** mit gebündeltem SQLite **3.54.1**, zbus auf **6.2.1** und kompatible weitere Rust-Abhängigkeiten aktualisiert. Unterstützte bisherige Datenbanken und Exporte, auch aus v0.3.1, sollen weiter verwendbar bleiben. Sicherung/Import und aufbewahrte historische Migrationskopien ändern sich nicht.
- **Regressionstests:** Zusätzliche Prüfungen betreffen alte SQLite-Datenbanken und Exporte, den LAN-Berechtigungswiderruf nach Neustart, HTTP-Header und Weiterleitungen, die Ablehnung ungültiger TLS-Zertifikate, echtes LuaJIT und die räumliche Fernbedienungsnavigation. Damit sind keine neuen Freigaben oder Zusagen für sämtliche Anbieter verbunden.

## Hinweis zu HTTPS-Zertifikaten

mpv 0.42 überprüft HTTPS-Zertifikate strenger. Streams mit **selbstsignierten, abgelaufenen oder anderweitig nicht vertrauenswürdigen Zertifikaten** können daher scheitern, obwohl sie mit einer älteren mpv-Version funktionierten. Das ist von den gesonderten LAN-Regeln für Playlist-Abrufe zu unterscheiden. Zertifikatskette und gegebenenfalls eine bewusst eingerichtete vertrauenswürdige Zertifizierungsstelle prüfen; **die TLS-Prüfung nicht global ausschalten**. Ob ein privater IPTV-Server funktioniert, hängt von seiner Konfiguration ab.

## Upgrade und Installation

**Ein normales Upgrade von v0.3.1 auf v0.3.2 benötigt weder Datenbankexport/-import noch eine Deinstallation oder das Löschen der Anwendungsdaten.** Broadcaster schließen, das geprüfte neue Produktions-Flatpak im bisherigen `--user`- oder `--system`-Bereich installieren und neu starten. Für wichtige Daten empfiehlt sich trotzdem eine separate Sicherung. `Import and Replace Database` ersetzt nach gesonderter Bestätigung und Neustart die *gesamte* Datenbank; das gehört nicht zum normalen Update.

**Erst nach tatsächlicher Veröffentlichung** von v0.3.2 die passende Datei `broadcaster-0.3.2-x86_64.flatpak` oder `broadcaster-0.3.2-aarch64.flatpak` samt `SHA256SUMS` vom [GitHub-Release v0.3.2](https://github.com/Certain1989/Broadcaster/releases/tag/v0.3.2) herunterladen. Prüfsumme kontrollieren und beispielsweise `flatpak install --user --or-update ./broadcaster-0.3.2-x86_64.flatpak` ausführen (für ARM64 den Dateinamen anpassen). Siehe [Installationsanleitung](../README.de.md#broadcaster-installieren) und [Release-Verifikation](release-verification.md). **CI-Artefakte eines Release-Kandidaten sind keine mit einem Versionstag veröffentlichten, produktionsattestierten Pakete.** Diese Versionshinweise allein belegen keine Veröffentlichung.

Weiterhin wird **GNOME Platform 51** verwendet; für H.264 kann `org.freedesktop.Platform.codecs-extra//26.08-extra` erforderlich sein. Die Bereitstellung erfolgt als **eigenständiges Flatpak über GitHub Releases**, nicht über Flathub. Ein unabhängig signiertes Repository für automatische Updates wird nicht ausgeliefert.

## Einschränkungen

Die bisherigen Regeln für Playlist-, XMLTV- und Logo-Netzwerkziele sowie die quellenspezifische LAN-Zustimmung gelten unverändert (siehe [Netzwerkvertrauen](network-trust.md)). Eine Playlist-Freigabe schaltet **nicht** automatisch verschachtelte Streams, HLS-Abfragen oder externe Player frei. Die HTTP(S)-Prüfungen für mpv und Header bilden keine umfassende Sandbox oder absolute Absicherung gegen unerwünschte Netzwerkzugriffe. Verfügbarkeit, Untertitel und TLS-Konfiguration hängen von den Anbietern ab.

SQLite-Sicherungen und historische Wiederherstellungskopien können vertrauliche URLs oder Tokens enthalten: sicher verwahren und keine Rohdatenbanken oder Playlists öffentlich an Fehlerberichte anhängen. Automatische Quellenaktualisierungen erfolgen nur bei laufender Anwendung. Die [Versionshinweise zu v0.3.1](release-v0.3.1-notes.de.md) dokumentieren die dort eingeführten Sicherheits- und Berechtigungsverbesserungen.

Dieses Hobbyprojekt wird ohne garantierte Support- oder Veröffentlichungsfristen bereitgestellt. Sicherheitsprobleme möglichst [vertraulich melden](../SECURITY.md).

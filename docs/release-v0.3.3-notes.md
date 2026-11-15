<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.3.3 — release notes

This maintenance release updates seven Rust packages (five direct dependencies and two transitive dependencies):

- **ammonia:** 4.2.4 → 4.2.5 (security fix for SVG/MathML HTML sanitization; RUSTSEC-2026-0533).
- **flate2:** 1.1.10 → 1.1.11 (upstream compression and decompression fixes).
- **libsqlite3-sys:** 0.39.1 → 0.40.0 (SQLite bindings used by rusqlite; transitive).
- **rusqlite:** 0.41.1 → 0.42.0 (SQLite integration maintenance).
- **uuid:** 1.29.0 → 1.29.2 (upstream fixes and compatibility improvements).
- **zbus:** 6.2.1 → 6.3.0 (D-Bus connection and protocol fixes).
- **zbus_macros:** 6.2.1 → 6.3.0 (matching D-Bus procedural macros; transitive).

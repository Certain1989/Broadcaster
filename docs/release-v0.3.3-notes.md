<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.3.3 — release notes

This maintenance release updates five Rust dependencies:

- **ammonia:** 4.2.4 → 4.2.5 (security fix for SVG/MathML HTML sanitization, RUSTSEC-2026-0533).
- **flate2:** 1.1.10 → 1.1.11 (compression and decompression fixes).
- **rusqlite:** 0.41.1 → 0.42.0 (SQLite integration maintenance; matching bundled libsqlite3-sys updated).
- **uuid:** 1.29.0 → 1.29.2 (upstream fixes and compatibility improvements).
- **zbus:** 6.2.1 → 6.3.0 (D-Bus connection and protocol fixes; matching macros updated).

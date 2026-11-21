<!-- SPDX-FileCopyrightText: 2026 Broadcaster contributors -->
<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Broadcaster v0.3.4 — release notes

This maintenance release updates two Rust dependencies compared with v0.3.3:

- **uuid:** 1.29.2 → 1.29.3 (Serde deserialization improvements and compatibility fixes, including for embedded targets).
- **rusqlite:** 0.42.0 → 0.43.0 (upstream fixes for memory-safety issues in several SQLite extension APIs, improved time-value handling, and rejection of empty or comment-only SQL statements).

No new application features, database-schema migrations, network permissions, or media-runtime updates are introduced by these dependency changes. Existing supported application data should remain usable after the normal Flatpak update. As with any application update, retaining a backup of important data is advisable.

This update retains the established standalone Flatpak distribution and existing installation procedure. See the [installation instructions](../README.md#install-broadcaster) and the [release verification guide](release-verification.md). The previous [v0.3.3 release notes](release-v0.3.3-notes.md) remain available.

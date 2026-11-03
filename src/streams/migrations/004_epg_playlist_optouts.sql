-- SPDX-FileCopyrightText: 2026 Broadcaster contributors
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Approved G1 v3 -> v4 migration. A row opts its playlist out of inherited matching.
PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;
CREATE TABLE epg_playlist_guide_optouts (
    playlist_source_id TEXT NOT NULL PRIMARY KEY
        REFERENCES iptv_sources(id) ON DELETE CASCADE,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
);
PRAGMA user_version = 4;
COMMIT;

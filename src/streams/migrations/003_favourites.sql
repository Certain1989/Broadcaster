-- SPDX-FileCopyrightText: 2026 Broadcaster contributors
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Forward-only v3: source-qualified row identity is the existing channel UUID.
BEGIN IMMEDIATE;
CREATE TABLE iptv_channel_favourites (
    channel_id TEXT NOT NULL PRIMARY KEY REFERENCES iptv_channels(id) ON DELETE CASCADE
);
CREATE TABLE iptv_view_preferences (
    id INTEGER NOT NULL PRIMARY KEY CHECK(id=1),
    favourites_only INTEGER NOT NULL DEFAULT 0 CHECK(favourites_only IN (0,1))
);
INSERT INTO iptv_view_preferences(id,favourites_only) VALUES(1,0);
PRAGMA user_version = 3;
COMMIT;

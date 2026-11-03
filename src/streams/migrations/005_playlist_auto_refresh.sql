-- SPDX-FileCopyrightText: 2026 Broadcaster contributors
-- SPDX-License-Identifier: GPL-3.0-or-later

PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;
ALTER TABLE iptv_sources
    ADD COLUMN auto_refresh INTEGER NOT NULL DEFAULT 0
        CHECK (auto_refresh IN (0,1));
ALTER TABLE iptv_sources
    ADD COLUMN refresh_interval_seconds INTEGER NOT NULL DEFAULT 86400
        CHECK (refresh_interval_seconds BETWEEN 21600 AND 604800);
CREATE INDEX idx_iptv_sources_refresh_due
    ON iptv_sources(enabled,auto_refresh,last_success_at,last_attempt_at);
PRAGMA user_version = 5;
COMMIT;

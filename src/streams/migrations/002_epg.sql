-- Broadcaster SQLite v1 -> v2 EPG migration.
-- Preconditions imposed by caller: private DB, application_id=0x42524354,
-- user_version=1, validated v1 schema, successful recoverable backup.
-- Run with foreign_keys=ON and busy_timeout set on an exclusive write worker.
BEGIN IMMEDIATE;

CREATE TABLE epg_sources (
    id TEXT NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    name TEXT NOT NULL COLLATE NOCASE CHECK (length(trim(name)) BETWEEN 1 AND 128),
    origin_kind TEXT NOT NULL CHECK (origin_kind IN ('local_file','remote_url')),
    origin_locator TEXT NOT NULL CHECK (length(trim(origin_locator)) BETWEEN 1 AND 8192),
    trust_scope TEXT NOT NULL DEFAULT 'public' CHECK (trust_scope IN ('public','explicit_lan')),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    auto_refresh INTEGER NOT NULL DEFAULT 0 CHECK (auto_refresh IN (0,1)),
    refresh_interval_seconds INTEGER NOT NULL DEFAULT 86400
        CHECK (refresh_interval_seconds BETWEEN 21600 AND 604800),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_attempt_at INTEGER,
    last_success_at INTEGER,
    last_error_code TEXT CHECK (last_error_code IS NULL OR length(last_error_code) BETWEEN 1 AND 64),
    UNIQUE(name),
    CHECK (origin_kind != 'local_file' OR trust_scope = 'public')
);

CREATE TABLE epg_channels (
    source_id TEXT NOT NULL REFERENCES epg_sources(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL CHECK (length(trim(channel_id)) BETWEEN 1 AND 512),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 512),
    PRIMARY KEY (source_id, channel_id)
);

CREATE TABLE epg_programmes (
    source_id TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    start_utc INTEGER NOT NULL,
    stop_utc INTEGER NOT NULL,
    title TEXT NOT NULL CHECK (length(trim(title)) BETWEEN 1 AND 512),
    subtitle TEXT CHECK (subtitle IS NULL OR length(subtitle) <= 512),
    description TEXT CHECK (description IS NULL OR length(description) <= 2048),
    PRIMARY KEY(source_id,channel_id,start_utc,stop_utc,title),
    FOREIGN KEY (source_id,channel_id)
        REFERENCES epg_channels(source_id,channel_id) ON DELETE CASCADE,
    CHECK (stop_utc > start_utc),
    CHECK (stop_utc - start_utc <= 86400)
);

-- One optional guide preference per IPTV playlist; absent means no explicit link.
CREATE TABLE epg_playlist_links (
    playlist_source_id TEXT NOT NULL PRIMARY KEY
        REFERENCES iptv_sources(id) ON DELETE CASCADE,
    guide_source_id TEXT NOT NULL
        REFERENCES epg_sources(id) ON DELETE CASCADE,
    updated_at INTEGER NOT NULL
);

-- Explicit mapping survives guide refreshes where guide channel temporarily disappears.
-- A deleted guide/channel is reported unresolved on lookup, never silently remapped.
CREATE TABLE epg_channel_overrides (
    iptv_channel_id TEXT NOT NULL PRIMARY KEY
        REFERENCES iptv_channels(id) ON DELETE CASCADE,
    guide_source_id TEXT NOT NULL
        REFERENCES epg_sources(id) ON DELETE CASCADE,
    guide_channel_id TEXT NOT NULL
        CHECK (length(trim(guide_channel_id)) BETWEEN 1 AND 512),
    updated_at INTEGER NOT NULL
);

-- Single application-wide EPG default. Applies to all eligible M3U playlists.
-- Playlist links are explicitly chosen exceptions, never implicit necessities.
CREATE TABLE epg_settings (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    shared_guide_source_id TEXT REFERENCES epg_sources(id) ON DELETE SET NULL
);
INSERT INTO epg_settings(singleton,shared_guide_source_id) VALUES (1,NULL);

-- Owner-confirmed TVG-ID mapping aliases shared across every playlist using
-- the same guide source. Exact, case-sensitive keys; never infer region changes.
-- No FK to epg_channels: aliases survive temporary absence during guide refresh.
CREATE TABLE epg_global_id_aliases (
    guide_source_id TEXT NOT NULL REFERENCES epg_sources(id) ON DELETE CASCADE,
    iptv_tvg_id TEXT NOT NULL CHECK (length(trim(iptv_tvg_id)) BETWEEN 1 AND 512),
    guide_channel_id TEXT NOT NULL CHECK (length(trim(guide_channel_id)) BETWEEN 1 AND 512),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (guide_source_id, iptv_tvg_id)
);

CREATE INDEX idx_epg_programmes_now
    ON epg_programmes(source_id,channel_id,start_utc,stop_utc);
-- Complementary index supports time-range overlap queries for a future
-- multi-channel programme-guide grid, independent of the now/next UI.
-- Required E1 cached XMLTV-only window query: start_utc < :to_utc AND stop_utc > :from_utc.
-- Caller bounds: 1..64 selected guide channels, 0 < (to_utc-from_utc) <= 172800 s,
-- <=4096 returned rows + 1 sentinel for explicit truncation, stable ordering.
-- Observed min/max programme timestamps and last_success_at are not evidence of
-- complete broadcast coverage; no new grid-specific table is needed.
CREATE INDEX idx_epg_programmes_window_stop
    ON epg_programmes(source_id,channel_id,stop_utc,start_utc);
CREATE INDEX idx_epg_programmes_retention
    ON epg_programmes(source_id,stop_utc);
CREATE INDEX idx_epg_playlist_guide ON epg_playlist_links(guide_source_id);
CREATE INDEX idx_epg_override_guide ON epg_channel_overrides(guide_source_id,guide_channel_id);
CREATE INDEX idx_epg_sources_enabled ON epg_sources(enabled);
CREATE INDEX idx_epg_aliases_target ON epg_global_id_aliases(guide_source_id,guide_channel_id);

PRAGMA user_version = 2;
COMMIT;

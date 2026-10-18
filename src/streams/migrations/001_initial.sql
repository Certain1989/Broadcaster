-- Broadcaster v0.2.0 — SQLITE SCHEMA v1 — normative, not illustrative.
-- Must only be executed on a new, empty Broadcaster IPTV DB with user_version=0.
-- The storage layer MUST inspect application_id/user_version before running it;
-- reject nonempty unknown databases and future schema versions without mutation.
-- journal_mode and busy_timeout are connection settings applied separately.
-- foreign_keys MUST be enabled before BEGIN, on every connection.
PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

CREATE TABLE iptv_sources (
    id TEXT NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    name TEXT NOT NULL COLLATE NOCASE
        CHECK (length(trim(name)) BETWEEN 1 AND 128),
    origin_kind TEXT NOT NULL
        CHECK (origin_kind IN ('local_file', 'remote_url')),
    origin_locator TEXT NOT NULL
        CHECK (length(trim(origin_locator)) BETWEEN 1 AND 8192),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    source_order INTEGER NOT NULL DEFAULT 0 CHECK (source_order >= 0),
    default_region_code TEXT CHECK (
        default_region_code IS NULL OR
        length(trim(default_region_code)) BETWEEN 1 AND 64
    ),
    default_region_label TEXT CHECK (
        default_region_label IS NULL OR
        length(trim(default_region_label)) BETWEEN 1 AND 128
    ),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_attempt_at INTEGER,
    last_success_at INTEGER,
    last_error_code TEXT CHECK (
        last_error_code IS NULL OR length(last_error_code) BETWEEN 1 AND 64
    ),
    UNIQUE(name)
);

-- Zero or more default languages/countries; source defaults are FALLBACK,
-- never added to nonempty imported metadata or explicit user overrides.
CREATE TABLE iptv_source_default_facets (
    source_id TEXT NOT NULL
        REFERENCES iptv_sources(id) ON DELETE CASCADE,
    facet TEXT NOT NULL CHECK (facet IN ('language', 'country')),
    value TEXT NOT NULL CHECK (length(trim(value)) BETWEEN 1 AND 64),
    PRIMARY KEY (source_id, facet, value)
);

CREATE TABLE iptv_channels (
    id TEXT NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    source_id TEXT NOT NULL
        REFERENCES iptv_sources(id) ON DELETE CASCADE,
    playlist_order INTEGER NOT NULL CHECK (playlist_order >= 0),
    imported_name TEXT NOT NULL
        CHECK (length(trim(imported_name)) BETWEEN 1 AND 512),
    stream_url TEXT NOT NULL
        CHECK (length(trim(stream_url)) BETWEEN 1 AND 8192),
    tvg_id TEXT CHECK (tvg_id IS NULL OR length(tvg_id) <= 512),
    tvg_name TEXT CHECK (tvg_name IS NULL OR length(tvg_name) <= 512),
    group_title TEXT CHECK (group_title IS NULL OR length(group_title) <= 256),
    logo_url TEXT CHECK (logo_url IS NULL OR length(logo_url) <= 8192),
    channel_number TEXT CHECK (
        channel_number IS NULL OR length(channel_number) <= 64
    ),
    imported_region_code TEXT CHECK (
        imported_region_code IS NULL OR
        length(trim(imported_region_code)) BETWEEN 1 AND 64
    ),
    imported_region_label TEXT CHECK (
        imported_region_label IS NULL OR
        length(trim(imported_region_label)) BETWEEN 1 AND 128
    ),
    user_agent TEXT CHECK (user_agent IS NULL OR length(user_agent) <= 1024),
    http_referer TEXT CHECK (
        http_referer IS NULL OR length(http_referer) <= 8192
    ),
    is_present INTEGER NOT NULL DEFAULT 1
        CHECK (is_present IN (0,1)),
    missing_since INTEGER,
    last_seen_revision INTEGER NOT NULL DEFAULT 0
        CHECK (last_seen_revision >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (
        (is_present = 1 AND missing_since IS NULL) OR
        (is_present = 0 AND missing_since IS NOT NULL)
    )
);

-- Imported channel language/country metadata, possibly multiple values.
CREATE TABLE iptv_channel_imported_facets (
    channel_id TEXT NOT NULL
        REFERENCES iptv_channels(id) ON DELETE CASCADE,
    facet TEXT NOT NULL CHECK (facet IN ('language', 'country')),
    value TEXT NOT NULL CHECK (length(trim(value)) BETWEEN 1 AND 64),
    PRIMARY KEY (channel_id, facet, value)
);

-- Manual overrides never alter imported fields. NULL display_name_override
-- means inherit imported_name. The three modes for other fields mean:
-- auto = imported if present, otherwise source default where applicable;
-- replace = use only manually supplied value(s); unknown = deliberately unset.
CREATE TABLE iptv_channel_overrides (
    channel_id TEXT NOT NULL PRIMARY KEY
        REFERENCES iptv_channels(id) ON DELETE CASCADE,
    display_name_override TEXT CHECK (
        display_name_override IS NULL OR
        length(trim(display_name_override)) BETWEEN 1 AND 512
    ),
    group_mode TEXT NOT NULL DEFAULT 'auto'
        CHECK (group_mode IN ('auto','replace','unknown')),
    group_override TEXT CHECK (
        group_override IS NULL OR length(trim(group_override)) BETWEEN 1 AND 256
    ),
    language_mode TEXT NOT NULL DEFAULT 'auto'
        CHECK (language_mode IN ('auto','replace','unknown')),
    country_mode TEXT NOT NULL DEFAULT 'auto'
        CHECK (country_mode IN ('auto','replace','unknown')),
    region_mode TEXT NOT NULL DEFAULT 'auto'
        CHECK (region_mode IN ('auto','replace','unknown')),
    region_code_override TEXT CHECK (
        region_code_override IS NULL OR
        length(trim(region_code_override)) BETWEEN 1 AND 64
    ),
    region_label_override TEXT CHECK (
        region_label_override IS NULL OR
        length(trim(region_label_override)) BETWEEN 1 AND 128
    ),
    updated_at INTEGER NOT NULL,
    CHECK (
        (group_mode = 'replace' AND group_override IS NOT NULL) OR
        (group_mode != 'replace' AND group_override IS NULL)
    ),
    CHECK (
        (region_mode = 'replace' AND
         (region_code_override IS NOT NULL OR region_label_override IS NOT NULL))
        OR
        (region_mode != 'replace' AND
         region_code_override IS NULL AND region_label_override IS NULL)
    )
);

-- Manual multi-value replacements; used ONLY when corresponding mode=replace.
CREATE TABLE iptv_channel_override_facets (
    channel_id TEXT NOT NULL
        REFERENCES iptv_channels(id) ON DELETE CASCADE,
    facet TEXT NOT NULL CHECK (facet IN ('language', 'country')),
    value TEXT NOT NULL CHECK (length(trim(value)) BETWEEN 1 AND 64),
    PRIMARY KEY (channel_id, facet, value)
);

CREATE INDEX idx_iptv_sources_enabled_order
    ON iptv_sources(enabled, source_order);
CREATE INDEX idx_iptv_channels_source_present_order
    ON iptv_channels(source_id, is_present, playlist_order);
CREATE INDEX idx_iptv_channels_source_group
    ON iptv_channels(source_id, is_present, group_title);
CREATE INDEX idx_iptv_channels_source_tvgid
    ON iptv_channels(source_id, tvg_id)
    WHERE tvg_id IS NOT NULL;
CREATE INDEX idx_iptv_channels_missing
    ON iptv_channels(source_id, missing_since)
    WHERE is_present = 0;
CREATE INDEX idx_iptv_imported_facet_filter
    ON iptv_channel_imported_facets(facet, value, channel_id);
CREATE INDEX idx_iptv_override_facet_filter
    ON iptv_channel_override_facets(facet, value, channel_id);
CREATE INDEX idx_iptv_source_default_facet_filter
    ON iptv_source_default_facets(facet, value, source_id);

PRAGMA application_id = 0x42524354; -- ASCII "BRCT"
PRAGMA user_version = 1;
COMMIT;

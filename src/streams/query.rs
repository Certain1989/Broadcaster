// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! One SQL effective-value algorithm for browsing AND filter option/count queries.
use super::{
    model::*,
    storage::{load_channel_detail, map_sql, Storage, StorageError, StorageResult},
};
use rusqlite::{params, Connection};

// An override facet participates only when the corresponding mode is replace.
// Imported sets take precedence over source default sets (never a union).
// The CTE computes effective values BEFORE search and selection; query_channels
// and query_filter_options share it verbatim.
const CORE: &str = r#"
WITH b AS (
  SELECT c.id, c.source_id, s.name AS source_name, s.source_order,
         c.playlist_order, c.imported_name, c.stream_url, c.logo_url,
         c.channel_number, COALESCE(o.display_name_override,c.imported_name) AS display_name,
         CASE COALESCE(o.group_mode,'auto')
           WHEN 'replace' THEN o.group_override
           WHEN 'unknown' THEN NULL
           ELSE NULLIF(TRIM(c.group_title),'') END AS eff_group,
         CASE COALESCE(o.region_mode,'auto')
           WHEN 'replace' THEN COALESCE(o.region_code_override,o.region_label_override)
           WHEN 'unknown' THEN NULL
           ELSE COALESCE(c.imported_region_code,c.imported_region_label,
                         s.default_region_code,s.default_region_label)
         END AS eff_region,
         COALESCE(o.language_mode,'auto') AS lang_mode,
         COALESCE(o.country_mode,'auto') AS country_mode,
         EXISTS (SELECT 1 FROM iptv_channel_overrides u WHERE u.channel_id=c.id) AS modified
  FROM iptv_channels c JOIN iptv_sources s ON s.id=c.source_id
       LEFT JOIN iptv_channel_overrides o ON o.channel_id=c.id
  WHERE s.enabled=1 AND c.is_present=1
),
facets AS (
  SELECT b.id AS channel_id, v.facet, v.value
  FROM b JOIN iptv_channel_override_facets v ON v.channel_id=b.id
  WHERE (v.facet='language' AND b.lang_mode='replace')
     OR (v.facet='country' AND b.country_mode='replace')
  UNION ALL
  SELECT b.id, v.facet, v.value
  FROM b JOIN iptv_channel_imported_facets v ON v.channel_id=b.id
  WHERE (v.facet='language' AND b.lang_mode='auto')
     OR (v.facet='country' AND b.country_mode='auto')
  UNION ALL
  SELECT b.id, v.facet, v.value
  FROM b JOIN iptv_source_default_facets v ON v.source_id=b.source_id
  WHERE ((v.facet='language' AND b.lang_mode='auto')
      OR (v.facet='country' AND b.country_mode='auto'))
    AND NOT EXISTS (
      SELECT 1 FROM iptv_channel_imported_facets i
      WHERE i.channel_id=b.id AND i.facet=v.facet
    )
),
matched AS (
  SELECT b.* FROM b
  WHERE (?1 IS NULL OR instr(lower(b.display_name),lower(?1))>0
        OR instr(lower(COALESCE(b.eff_group,'')),lower(?1))>0)
    AND (?2 IS NULL OR b.source_id=?2)
    AND (?3=0 OR (?3=2 AND b.eff_group IS NULL) OR (?3=1 AND b.eff_group=?4))
    AND (?5=0 OR (?5=2 AND NOT EXISTS (
          SELECT 1 FROM facets x WHERE x.channel_id=b.id AND x.facet='language'
        )) OR (?5=1 AND EXISTS (
          SELECT 1 FROM facets x WHERE x.channel_id=b.id AND x.facet='language' AND x.value=?6
        )))
    AND (?7=0 OR (?7=2 AND NOT EXISTS (
          SELECT 1 FROM facets x WHERE x.channel_id=b.id AND x.facet='country'
        )) OR (?7=1 AND EXISTS (
          SELECT 1 FROM facets x WHERE x.channel_id=b.id AND x.facet='country' AND x.value=?8
        )))
    AND (?9=0 OR (?9=2 AND b.eff_region IS NULL) OR (?9=1 AND b.eff_region=?10))
)
"#;
fn selection(s: &FacetSelection) -> (i32, Option<&str>) {
    match s {
        FacetSelection::Any => (0, None),
        FacetSelection::Value(value) => (1, Some(value.as_str())),
        FacetSelection::Unknown => (2, None),
    }
}
fn valid_query(q: &ChannelQuery) -> StorageResult<()> {
    if q.limit == 0
        || q.limit > 500
        || q.search
            .as_ref()
            .is_some_and(|s| s.len() > 512 || s.chars().any(char::is_control))
    {
        return Err(StorageError::InvalidInput);
    }
    for (field, facet) in [
        (&q.language, FacetDimension::Language),
        (&q.country, FacetDimension::Country),
    ] {
        if let FacetSelection::Value(value) = field {
            if normalize_facet(value, facet)? != *value {
                return Err(StorageError::InvalidInput);
            }
        }
    }
    for sel in [&q.group, &q.region] {
        if let FacetSelection::Value(s) = sel {
            bounded(s, 256)?;
        }
    }
    if q.source_id
        .as_ref()
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
    {
        return Err(StorageError::InvalidInput);
    }
    Ok(())
}
type QueryArgs<'a> = (
    Option<&'a str>,
    Option<&'a str>,
    (i32, Option<&'a str>),
    (i32, Option<&'a str>),
    (i32, Option<&'a str>),
    (i32, Option<&'a str>),
);
fn args(q: &ChannelQuery) -> QueryArgs<'_> {
    (
        q.search.as_deref().filter(|s| !s.is_empty()),
        q.source_id.as_deref(),
        selection(&q.group),
        selection(&q.language),
        selection(&q.country),
        selection(&q.region),
    )
}
fn query_ids(conn: &Connection, q: &ChannelQuery) -> StorageResult<(Vec<String>, u64)> {
    let (search, source, group, language, country, region) = args(q);
    let count_sql = format!("{CORE} SELECT count(*) FROM matched");
    let total: i64 = conn
        .query_row(
            &count_sql,
            params![
                search, source, group.0, group.1, language.0, language.1, country.0, country.1,
                region.0, region.1,
            ],
            |r| r.get(0),
        )
        .map_err(map_sql)?;
    let order = ordering(q.sort);
    let sql = format!("{CORE} SELECT id FROM matched ORDER BY {order} LIMIT ?11 OFFSET ?12");
    let mut stmt = conn.prepare(&sql).map_err(map_sql)?;
    let ids = stmt
        .query_map(
            params![
                search, source, group.0, group.1, language.0, language.1, country.0, country.1,
                region.0, region.1, q.limit, q.offset,
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(map_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_sql)?;
    Ok((ids, total as u64))
}

fn ordering(sort: SortOrder) -> &'static str {
    match sort {
        SortOrder::PlaylistOrder => "source_order,playlist_order,id",
        SortOrder::Name => "lower(display_name),source_order,playlist_order,id",
        SortOrder::Group => "lower(COALESCE(eff_group,'')),source_order,playlist_order,id",
    }
}

impl Storage {
    /// Zero-based rank of a stable channel ID in the *full* effective query.
    /// Paging bounds are deliberately ignored; ranking uses the exact same
    /// matched CTE and ordering as query_channels.
    pub fn channel_position(
        &self,
        query: &ChannelQuery,
        id: &ChannelId,
    ) -> StorageResult<Option<u64>> {
        valid_query(query)?;
        if uuid::Uuid::parse_str(id).is_err() {
            return Err(StorageError::InvalidInput);
        }
        let conn = self.connect()?;
        let (search, source, group, language, country, region) = args(query);
        let sql = format!(
            "{CORE} SELECT position FROM (
               SELECT id, ROW_NUMBER() OVER (ORDER BY {}) - 1 AS position FROM matched
             ) WHERE id=?11",
            ordering(query.sort)
        );
        use rusqlite::OptionalExtension;
        let rank: Option<i64> = conn
            .query_row(
                &sql,
                params![
                    search, source, group.0, group.1, language.0, language.1,
                    country.0, country.1, region.0, region.1, id,
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sql)?;
        rank.map(|value| u64::try_from(value).map_err(|_| StorageError::CorruptDatabase))
            .transpose()
    }
    pub fn query_channels(&self, query: &ChannelQuery) -> StorageResult<ChannelPage> {
        valid_query(query)?;
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(map_sql)?;
        let (ids, total) = query_ids(&tx, query)?;
        let items = ids
            .iter()
            .map(|id| load_channel_detail(&tx, id).map(|d| d.effective))
            .collect::<StorageResult<Vec<_>>>()?;
        tx.commit().map_err(map_sql)?;
        Ok(ChannelPage { items, total })
    }

    /// Counts are SQL DISTINCT channel counts, never a 10k-row GTK reduction.
    /// The target facet selection is deliberately ignored.
    pub fn query_filter_options(
        &self,
        query: &ChannelQuery,
        dimension: FilterDimension,
    ) -> StorageResult<FilterOptions> {
        let mut q = query.clone();
        match dimension {
            FilterDimension::Source => q.source_id = None,
            FilterDimension::Group => q.group = FacetSelection::Any,
            FilterDimension::Language => q.language = FacetSelection::Any,
            FilterDimension::Country => q.country = FacetSelection::Any,
            FilterDimension::Region => q.region = FacetSelection::Any,
        };
        valid_query(&q)?;
        let conn = self.connect()?;
        let (search, source, group, language, country, region) = args(&q);
        let sql = match dimension {
            FilterDimension::Source => format!("{CORE} SELECT source_id,count(*) FROM matched GROUP BY source_id ORDER BY source_id"),
            FilterDimension::Group => format!("{CORE} SELECT eff_group,count(*) FROM matched GROUP BY eff_group ORDER BY eff_group IS NULL,eff_group COLLATE NOCASE"),
            FilterDimension::Region => format!("{CORE} SELECT eff_region,count(*) FROM matched GROUP BY eff_region ORDER BY eff_region IS NULL,eff_region COLLATE NOCASE"),
            FilterDimension::Language | FilterDimension::Country => {
                let facet = if dimension == FilterDimension::Language { "language" } else { "country" };
                format!("{CORE}
                 SELECT v,n FROM (
                    SELECT value AS v,COUNT(DISTINCT channel_id) AS n FROM facets
                    WHERE facet='{facet}' AND channel_id IN (SELECT id FROM matched)
                    GROUP BY value
                    UNION ALL
                    SELECT NULL AS v,COUNT(*) AS n FROM matched m
                    WHERE NOT EXISTS (
                        SELECT 1 FROM facets f WHERE f.facet='{facet}' AND f.channel_id=m.id
                    ) HAVING COUNT(*)>0
                 ) ORDER BY v IS NULL,v COLLATE NOCASE")
            }
        };
        let mut stmt = conn.prepare(&sql).map_err(map_sql)?;
        let choices = stmt
            .query_map(
                params![
                    search, source, group.0, group.1, language.0, language.1, country.0, country.1,
                    region.0, region.1
                ],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)),
            )
            .map_err(map_sql)?
            .map(|row| {
                let (value, count) = row.map_err(map_sql)?;
                Ok(FacetChoice {
                    value: value.map(FacetValue::Known).unwrap_or(FacetValue::Unknown),
                    count: count as u64,
                })
            })
            .collect::<StorageResult<Vec<_>>>()?;
        Ok(FilterOptions { dimension, choices })
    }
}

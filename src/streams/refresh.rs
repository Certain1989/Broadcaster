// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Source-scoped, unambiguous identity reconciliation and transactional refresh.
//! Cancellation/supersession is checked by S3 immediately BEFORE invoking the
//! blocking commit. Once BEGIN IMMEDIATE is entered, success is authoritative.
use super::{
    model::*,
    storage::{load_source, map_sql, Storage, StorageError, StorageResult},
};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
struct Existing {
    id: String,
    tvg: Option<String>,
    url: String,
    group: Option<String>,
    name: String,
    present: bool,
}
#[derive(Clone, Debug)]
struct Plan {
    // incoming index -> existing row index, where unambiguously matched
    matches: Vec<Option<usize>>,
    old_present: usize,
    matched: usize,
    missing: usize,
}
fn normalize_key(text: &str) -> String {
    text.trim().to_lowercase()
}
fn pair_key(group: Option<&str>, name: &str) -> String {
    // Length prefix prevents name/group separator collision.
    let g = normalize_key(group.unwrap_or(""));
    let n = normalize_key(name);
    format!("{}:{}{}:{}", g.len(), g, n.len(), n)
}
fn load_old(conn: &Connection, source_id: &str) -> StorageResult<Vec<Existing>> {
    let mut stmt = conn
        .prepare(
            "SELECT id,tvg_id,stream_url,group_title,imported_name,is_present
         FROM iptv_channels WHERE source_id=?1 ORDER BY id",
        )
        .map_err(map_sql)?;
    let result = stmt
        .query_map([source_id], |r| {
            Ok(Existing {
                id: r.get(0)?,
                tvg: r.get(1)?,
                url: r.get(2)?,
                group: r.get(3)?,
                name: r.get(4)?,
                present: r.get::<_, i32>(5)? != 0,
            })
        })
        .map_err(map_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_sql);
    result
}
fn match_entries(old: &[Existing], new: &[IncomingChannel]) -> Plan {
    let mut matches = vec![None; new.len()];
    let mut consumed = HashSet::new();
    for priority in 0..3 {
        let mut old_keys: HashMap<String, Vec<usize>> = HashMap::new();
        let mut new_keys: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, row) in old.iter().enumerate() {
            let key = match priority {
                0 => row
                    .tvg
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(normalize_key),
                1 => Some(row.url.trim().to_owned()),
                _ => Some(pair_key(row.group.as_deref(), &row.name)),
            };
            if let Some(key) = key {
                old_keys.entry(key).or_default().push(i);
            }
        }
        for (i, row) in new.iter().enumerate() {
            let key = match priority {
                0 => row
                    .tvg_id
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(normalize_key),
                1 => Some(row.stream_url.trim().to_owned()),
                _ => Some(pair_key(row.group_title.as_deref(), &row.imported_name)),
            };
            if let Some(key) = key {
                new_keys.entry(key).or_default().push(i);
            }
        }
        for (key, new_idxs) in new_keys {
            if new_idxs.len() != 1 {
                continue;
            }
            let Some(old_idxs) = old_keys.get(&key) else {
                continue;
            };
            if old_idxs.len() != 1 {
                continue;
            }
            let ni = new_idxs[0];
            let oi = old_idxs[0];
            if matches[ni].is_none() && !consumed.contains(&oi) {
                matches[ni] = Some(oi);
                consumed.insert(oi);
            }
        }
    }
    let old_present = old.iter().filter(|row| row.present).count();
    let matched = matches.iter().filter(|x| x.is_some()).count();
    let newly_seen: HashSet<_> = matches.iter().filter_map(|x| *x).collect();
    let missing = old
        .iter()
        .enumerate()
        .filter(|(i, row)| row.present && !newly_seen.contains(i))
        .count();
    Plan {
        matches,
        old_present,
        matched,
        missing,
    }
}

impl Storage {
    /// Fully validates and inserts a source and first nonempty channel catalogue in
    /// a single IMMEDIATE transaction. No public placeholder creation API exists.
    pub fn create_source_with_initial_snapshot(
        &self,
        id: &str,
        draft: &SourceDraft,
        snapshot: &ValidatedSnapshot,
        now: i64,
    ) -> StorageResult<Source> {
        draft.validate()?;
        if uuid::Uuid::parse_str(id).is_err() || snapshot.source_id() != id {
            return Err(StorageError::InvalidInput);
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        tx.execute(
            "INSERT INTO iptv_sources
             (id,name,origin_kind,origin_locator,enabled,source_order,
              default_region_code,default_region_label,revision,created_at,updated_at,
              last_attempt_at,last_success_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,0,?9,?9,?9,?9)",
            params![
                id,
                draft.name,
                draft.origin.kind.sql(),
                draft.origin.locator,
                draft.enabled as i32,
                draft.order,
                draft.defaults.region.as_ref().and_then(|r| r.code.as_ref()),
                draft
                    .defaults
                    .region
                    .as_ref()
                    .and_then(|r| r.label.as_ref()),
                now
            ],
        )
        .map_err(map_sql)?;
        for (dimension, values) in [
            (FacetDimension::Language, &draft.defaults.languages),
            (FacetDimension::Country, &draft.defaults.countries),
        ] {
            for tag in values {
                tx.execute(
                    "INSERT INTO iptv_source_default_facets(source_id,facet,value) VALUES(?1,?2,?3)",
                    params![id,dimension.sql(),tag],
                ).map_err(map_sql)?;
            }
        }
        let plan = match_entries(&[], snapshot.entries());
        apply_snapshot(&tx, id, snapshot.entries(), &[], &plan, now, 1)?;
        tx.execute("UPDATE iptv_sources SET revision=1 WHERE id=?1", [id])
            .map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        load_source(&conn, id)
    }

    pub fn preview_refresh(&self, snapshot: &ValidatedSnapshot) -> StorageResult<RefreshPreview> {
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(map_sql)?;
        let source = load_source(&tx, snapshot.source_id())?;
        let old = load_old(&tx, snapshot.source_id())?;
        let plan = match_entries(&old, snapshot.entries());
        tx.commit().map_err(map_sql)?;
        Ok(build_preview(snapshot, &source, &plan))
    }

    pub fn commit_refresh(
        &self,
        snapshot: &ValidatedSnapshot,
        preview: &RefreshPreview,
        decision: &RefreshDecision,
        now: i64,
    ) -> StorageResult<Source> {
        self.commit_refresh_internal(snapshot, preview, decision, None, now)
    }

    /// Reauthorization or URL change is atomic with snapshot and revision;
    /// failed/declined/stale work never modifies the old origin.
    pub fn replace_source_origin_and_snapshot(
        &self,
        snapshot: &ValidatedSnapshot,
        preview: &RefreshPreview,
        decision: &RefreshDecision,
        new_origin: SourceOrigin,
        now: i64,
    ) -> StorageResult<Source> {
        new_origin.validate()?;
        self.commit_refresh_internal(snapshot, preview, decision, Some(new_origin), now)
    }

    fn commit_refresh_internal(
        &self,
        snapshot: &ValidatedSnapshot,
        preview: &RefreshPreview,
        decision: &RefreshDecision,
        replacement_origin: Option<SourceOrigin>,
        now: i64,
    ) -> StorageResult<Source> {
        if preview.source_id != snapshot.source_id()
            || preview.fingerprint != snapshot.fingerprint()
        {
            return Err(StorageError::SnapshotMismatch);
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        let source = load_source(&tx, snapshot.source_id())?;
        if source.revision != preview.expected_revision {
            return Err(StorageError::StaleRevision);
        }
        let old = load_old(&tx, snapshot.source_id())?;
        let plan = match_entries(&old, snapshot.entries());
        let authoritative = build_preview(snapshot, &source, &plan);
        if &authoritative != preview {
            return Err(StorageError::SnapshotMismatch);
        }
        if authoritative.requires_confirmation {
            match decision {
                RefreshDecision::ExplicitApproval {
                    source_id,
                    expected_revision,
                    fingerprint,
                } if source_id == &preview.source_id
                    && *expected_revision == preview.expected_revision
                    && fingerprint == &preview.fingerprint => {}
                RefreshDecision::NotRequired => return Err(StorageError::ApprovalRequired),
                _ => return Err(StorageError::SnapshotMismatch),
            }
        }
        let next_rev = source
            .revision
            .checked_add(1)
            .ok_or(StorageError::StorageFailure)?;
        apply_snapshot(
            &tx,
            snapshot.source_id(),
            snapshot.entries(),
            &old,
            &plan,
            now,
            next_rev,
        )?;
        if let Some(origin) = &replacement_origin {
            tx.execute(
                "UPDATE iptv_sources SET origin_kind=?1,origin_locator=?2 WHERE id=?3",
                params![origin.kind.sql(), origin.locator, snapshot.source_id()],
            )
            .map_err(map_sql)?;
        }
        tx.execute(
            "UPDATE iptv_sources SET revision=?1,updated_at=?2,last_attempt_at=?2,
             last_success_at=?2,last_error_code=NULL WHERE id=?3",
            params![next_rev, now, snapshot.source_id()],
        )
        .map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        load_source(&conn, snapshot.source_id())
    }
}

fn build_preview(snapshot: &ValidatedSnapshot, source: &Source, plan: &Plan) -> RefreshPreview {
    let new_present = snapshot.entries().len();
    RefreshPreview {
        source_id: source.id.clone(),
        expected_revision: source.revision,
        fingerprint: snapshot.fingerprint().to_owned(),
        old_present: plan.old_present,
        new_present,
        added: new_present - plan.matched,
        matched: plan.matched,
        missing: plan.missing,
        // Strictly GREATER than 75%; 20 -> 4 requires confirmation, 20 -> 5 does not.
        requires_confirmation: plan.old_present >= 20
            && new_present.saturating_mul(4) < plan.old_present,
    }
}
fn apply_snapshot(
    tx: &Transaction<'_>,
    source_id: &str,
    incoming: &[IncomingChannel],
    old: &[Existing],
    plan: &Plan,
    at: i64,
    revision: i64,
) -> StorageResult<()> {
    if incoming.is_empty() {
        return Err(StorageError::InvalidInput);
    }
    // Keep tombstone timestamps on previously missing channels; changes only
    // active rows to missing, in this same transaction.
    tx.execute(
        "UPDATE iptv_channels SET is_present=0,missing_since=?1,updated_at=?1
         WHERE source_id=?2 AND is_present=1",
        params![at, source_id],
    )
    .map_err(map_sql)?;
    for (index, item) in incoming.iter().enumerate() {
        let id = plan.matches[index]
            .map(|oi| old[oi].id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        tx.execute(
            "INSERT INTO iptv_channels
              (id,source_id,playlist_order,imported_name,stream_url,tvg_id,tvg_name,
               group_title,logo_url,channel_number,imported_region_code,imported_region_label,
               user_agent,http_referer,is_present,missing_since,last_seen_revision,created_at,updated_at)
             VALUES
              (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,1,NULL,?15,?16,?16)
             ON CONFLICT(id) DO UPDATE SET
              playlist_order=excluded.playlist_order,imported_name=excluded.imported_name,
              stream_url=excluded.stream_url,tvg_id=excluded.tvg_id,tvg_name=excluded.tvg_name,
              group_title=excluded.group_title,logo_url=excluded.logo_url,
              channel_number=excluded.channel_number,
              imported_region_code=excluded.imported_region_code,
              imported_region_label=excluded.imported_region_label,
              user_agent=excluded.user_agent,http_referer=excluded.http_referer,
              is_present=1,missing_since=NULL,last_seen_revision=excluded.last_seen_revision,
              updated_at=excluded.updated_at",
            params![
                id,source_id,item.order,item.imported_name,item.stream_url,item.tvg_id,item.tvg_name,
                item.group_title,item.logo_url,item.channel_number,
                item.region.as_ref().and_then(|r|r.code.as_ref()),
                item.region.as_ref().and_then(|r|r.label.as_ref()),
                item.user_agent,item.referer,revision,at,
            ],
        ).map_err(map_sql)?;
        tx.execute(
            "DELETE FROM iptv_channel_imported_facets WHERE channel_id=?1",
            [&id],
        )
        .map_err(map_sql)?;
        for (facet, values) in [
            (FacetDimension::Language, &item.languages),
            (FacetDimension::Country, &item.countries),
        ] {
            for value in values {
                tx.execute(
                    "INSERT INTO iptv_channel_imported_facets(channel_id,facet,value)
                     VALUES(?1,?2,?3)",
                    params![id, facet.sql(), value],
                )
                .map_err(map_sql)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    fn fake(name: &str, order: u32, tvg: Option<&str>, url: &str) -> IncomingChannel {
        IncomingChannel {
            order,
            imported_name: name.into(),
            stream_url: url.into(),
            tvg_id: tvg.map(str::to_owned),
            tvg_name: None,
            group_title: None,
            logo_url: None,
            channel_number: None,
            region: None,
            languages: Default::default(),
            countries: Default::default(),
            user_agent: None,
            referer: None,
        }
    }
    #[test]
    fn unique_tvg_survives_token_change_and_reorder() {
        let old = vec![Existing {
            id: "a".into(),
            tvg: Some("tag".into()),
            url: "https://example.invalid/old".into(),
            group: None,
            name: "First".into(),
            present: true,
        }];
        let new = vec![fake(
            "Renamed",
            99,
            Some("tag"),
            "https://example.invalid/new?token=fake",
        )];
        assert_eq!(match_entries(&old, &new).matches, vec![Some(0)]);
    }
    #[test]
    fn ambiguous_duplicates_do_not_share_identity() {
        let old = vec![
            Existing {
                id: "a".into(),
                tvg: Some("dup".into()),
                url: "https://example.invalid/x".into(),
                group: None,
                name: "Dup".into(),
                present: true,
            },
            Existing {
                id: "b".into(),
                tvg: Some("dup".into()),
                url: "https://example.invalid/x".into(),
                group: None,
                name: "Dup".into(),
                present: true,
            },
        ];
        let new = vec![fake("Dup", 0, Some("dup"), "https://example.invalid/x")];
        assert_eq!(match_entries(&old, &new).matches, vec![None]);
    }
}

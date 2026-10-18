// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Typed IPTV persistence boundaries. Parser (S2) and service (S3) consume these types.
use std::collections::BTreeSet;

use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};

use super::storage::{StorageError, StorageResult};

pub type SourceId = String;
pub type ChannelId = String;

// ISO 3166-1 alpha-2 assigned country codes (249). No IP/title inference.
const ISO_3166_ALPHA2: &str = concat!(
    "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ ",
    "BA BB BD BE BF BG BH BI BJ BL BM BN BO BQ BR BS ",
    "BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN ",
    "CO CR CU CV CW CX CY CZ DE DJ DK DM DO DZ EC EE ",
    "EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF ",
    "GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK HM ",
    "HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM ",
    "JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC ",
    "LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ",
    "ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA ",
    "NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG ",
    "PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW ",
    "SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ",
    "ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO ",
    "TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI ",
    "VN VU WF WS YE YT ZA ZM ZW ",
);

impl std::fmt::Debug for SourceOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceOrigin")
            .field("kind", &self.kind)
            .field("locator", &"<redacted>")
            .finish()
    }
}
impl std::fmt::Debug for IncomingChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncomingChannel")
            .field("order", &self.order)
            .field("stream_url", &"<redacted>")
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for ChannelEffective {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelEffective")
            .field("id", &self.id)
            .field("stream_url", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Untrusted values must never be included in diagnostics or public logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginKind {
    LocalFile,
    RemoteUrl,
}
impl OriginKind {
    pub(crate) fn sql(self) -> &'static str {
        match self {
            Self::LocalFile => "local_file",
            Self::RemoteUrl => "remote_url",
        }
    }
    pub(crate) fn from_sql(value: &str) -> StorageResult<Self> {
        match value {
            "local_file" => Ok(Self::LocalFile),
            "remote_url" => Ok(Self::RemoteUrl),
            _ => Err(StorageError::CorruptDatabase),
        }
    }
}

#[derive(Clone)]
pub struct SourceOrigin {
    pub kind: OriginKind,
    /// Sensitive: never log, print, or display this value unredacted.
    pub locator: String,
}
impl SourceOrigin {
    pub fn validate(&self) -> StorageResult<()> {
        bounded(&self.locator, 8192)?;
        if self.kind == OriginKind::RemoteUrl {
            let url = reqwest::Url::parse(&self.locator).map_err(|_| StorageError::InvalidInput)?;
            if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
                return Err(StorageError::InvalidInput);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Region {
    pub code: Option<String>,
    pub label: Option<String>,
}
impl Region {
    pub fn validate(&self) -> StorageResult<()> {
        bounded_optional(&self.code, 64)?;
        bounded_optional(&self.label, 128)?;
        if self.code.as_deref().is_some_and(|s| s.trim() != s)
            || self.label.as_deref().is_some_and(|s| s.trim() != s)
        {
            return Err(StorageError::InvalidInput);
        }
        if self.code.is_none() && self.label.is_none() {
            return Err(StorageError::InvalidInput);
        }
        Ok(())
    }
    pub fn key(&self) -> String {
        self.code
            .clone()
            .or_else(|| self.label.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct SourceDefaults {
    pub languages: BTreeSet<String>,
    pub countries: BTreeSet<String>,
    pub region: Option<Region>,
}
impl SourceDefaults {
    pub fn validate(&self) -> StorageResult<()> {
        validate_facets(&self.languages, FacetDimension::Language)?;
        validate_facets(&self.countries, FacetDimension::Country)?;
        if let Some(value) = &self.region {
            value.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct SourceDraft {
    pub name: String,
    pub origin: SourceOrigin,
    pub enabled: bool,
    pub order: u32,
    pub defaults: SourceDefaults,
}
impl SourceDraft {
    pub fn validate(&self) -> StorageResult<()> {
        bounded(&self.name, 128)?;
        self.origin.validate()?;
        self.defaults.validate()
    }
}
#[derive(Clone, Debug)]
pub struct Source {
    pub id: SourceId,
    pub name: String,
    pub origin: SourceOrigin,
    pub enabled: bool,
    pub order: u32,
    pub defaults: SourceDefaults,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub last_error_code: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingChannel {
    pub order: u32,
    pub imported_name: String,
    /// Sensitive: may include authentication tokens.
    pub stream_url: String,
    pub tvg_id: Option<String>,
    pub tvg_name: Option<String>,
    pub group_title: Option<String>,
    pub logo_url: Option<String>,
    pub channel_number: Option<String>,
    pub region: Option<Region>,
    pub languages: BTreeSet<String>,
    pub countries: BTreeSet<String>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
}
impl IncomingChannel {
    pub fn validate(&self) -> StorageResult<()> {
        bounded(&self.imported_name, 512)?;
        bounded(&self.stream_url, 8192)?;
        let url = reqwest::Url::parse(&self.stream_url).map_err(|_| StorageError::InvalidInput)?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err(StorageError::InvalidInput);
        }
        optional_with_empty(&self.tvg_id, 512)?;
        optional_with_empty(&self.tvg_name, 512)?;
        optional_with_empty(&self.group_title, 256)?;
        optional_with_empty(&self.logo_url, 8192)?;
        optional_with_empty(&self.channel_number, 64)?;
        optional_with_empty(&self.user_agent, 1024)?;
        optional_with_empty(&self.referer, 8192)?;
        if let Some(region) = &self.region {
            region.validate()?;
        }
        validate_facets(&self.languages, FacetDimension::Language)?;
        validate_facets(&self.countries, FacetDimension::Country)
    }
}

/// Only this constructor produces an importable snapshot.
#[derive(Clone, Debug)]
pub struct ValidatedSnapshot {
    source_id: SourceId,
    entries: Vec<IncomingChannel>,
    fingerprint: String,
}
impl ValidatedSnapshot {
    pub fn new(source_id: SourceId, entries: Vec<IncomingChannel>) -> StorageResult<Self> {
        if uuid::Uuid::parse_str(&source_id).is_err()
            || entries.is_empty()
            || entries.len() > 50_000
        {
            return Err(StorageError::InvalidInput);
        }
        for item in &entries {
            item.validate()?;
        }
        // BTreeSet preserves canonical ordering; serialization includes tokenized URLs
        // but the digest is never used as a diagnostic string containing original data.
        let bytes = serde_json::to_vec(&entries).map_err(|_| StorageError::InvalidInput)?;
        let fingerprint = digest(&SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            source_id,
            entries,
            fingerprint,
        })
    }
    pub fn source_id(&self) -> &str {
        &self.source_id
    }
    pub fn entries(&self) -> &[IncomingChannel] {
        &self.entries
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefreshPreview {
    pub source_id: SourceId,
    pub expected_revision: i64,
    pub fingerprint: String,
    pub old_present: usize,
    pub new_present: usize,
    pub added: usize,
    pub matched: usize,
    pub missing: usize,
    pub requires_confirmation: bool,
}
impl RefreshPreview {
    /// Call exclusively after an explicit positive user confirmation.
    pub fn explicit_approval(&self) -> RefreshDecision {
        RefreshDecision::ExplicitApproval {
            source_id: self.source_id.clone(),
            expected_revision: self.expected_revision,
            fingerprint: self.fingerprint.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshDecision {
    NotRequired,
    ExplicitApproval {
        source_id: SourceId,
        expected_revision: i64,
        fingerprint: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FacetMode {
    Auto,
    Replace,
    Unknown,
}
impl FacetMode {
    pub(crate) fn sql(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Replace => "replace",
            Self::Unknown => "unknown",
        }
    }
    pub(crate) fn from_sql(s: &str) -> StorageResult<Self> {
        match s {
            "auto" => Ok(Self::Auto),
            "replace" => Ok(Self::Replace),
            "unknown" => Ok(Self::Unknown),
            _ => Err(StorageError::CorruptDatabase),
        }
    }
}
#[derive(Clone, Debug)]
pub struct GroupOverride {
    pub mode: FacetMode,
    pub value: Option<String>,
}
#[derive(Clone, Debug)]
pub struct RegionOverride {
    pub mode: FacetMode,
    pub value: Option<Region>,
}
#[derive(Clone, Debug)]
pub struct SetOverride {
    pub mode: FacetMode,
    pub values: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct ChannelOverrides {
    pub display_name: Option<String>,
    pub group: GroupOverride,
    pub region: RegionOverride,
    pub languages: SetOverride,
    pub countries: SetOverride,
}
impl Default for ChannelOverrides {
    fn default() -> Self {
        Self {
            display_name: None,
            group: GroupOverride {
                mode: FacetMode::Auto,
                value: None,
            },
            region: RegionOverride {
                mode: FacetMode::Auto,
                value: None,
            },
            languages: SetOverride {
                mode: FacetMode::Auto,
                values: BTreeSet::new(),
            },
            countries: SetOverride {
                mode: FacetMode::Auto,
                values: BTreeSet::new(),
            },
        }
    }
}
impl ChannelOverrides {
    pub fn validate(&self) -> StorageResult<()> {
        bounded_optional(&self.display_name, 512)?;
        match self.group.mode {
            FacetMode::Replace => bounded(
                self.group
                    .value
                    .as_deref()
                    .ok_or(StorageError::InvalidInput)?,
                256,
            )?,
            _ if self.group.value.is_some() => return Err(StorageError::InvalidInput),
            _ => {}
        }
        match self.region.mode {
            FacetMode::Replace => self
                .region
                .value
                .as_ref()
                .ok_or(StorageError::InvalidInput)?
                .validate()?,
            _ if self.region.value.is_some() => return Err(StorageError::InvalidInput),
            _ => {}
        }
        for (set, dimension) in [
            (&self.languages, FacetDimension::Language),
            (&self.countries, FacetDimension::Country),
        ] {
            if set.mode == FacetMode::Replace && set.values.is_empty()
                || set.mode != FacetMode::Replace && !set.values.is_empty()
            {
                return Err(StorageError::InvalidInput);
            }
            validate_facets(&set.values, dimension)?;
        }
        Ok(())
    }
    pub fn has_values(&self) -> bool {
        self.display_name.is_some()
            || self.group.mode != FacetMode::Auto
            || self.region.mode != FacetMode::Auto
            || self.languages.mode != FacetMode::Auto
            || self.countries.mode != FacetMode::Auto
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FacetSelection {
    #[default]
    Any,
    Value(String),
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortOrder {
    PlaylistOrder,
    Name,
    Group,
}
#[derive(Clone, Debug)]
pub struct ChannelQuery {
    pub search: Option<String>,
    pub source_id: Option<SourceId>,
    pub group: FacetSelection,
    pub language: FacetSelection,
    pub country: FacetSelection,
    pub region: FacetSelection,
    pub sort: SortOrder,
    pub limit: u32,
    pub offset: u32,
}
impl Default for ChannelQuery {
    fn default() -> Self {
        Self {
            search: None,
            source_id: None,
            group: FacetSelection::Any,
            language: FacetSelection::Any,
            country: FacetSelection::Any,
            region: FacetSelection::Any,
            sort: SortOrder::PlaylistOrder,
            limit: 100,
            offset: 0,
        }
    }
}
#[derive(Clone)]
pub struct ChannelEffective {
    pub id: ChannelId,
    pub source_id: SourceId,
    pub source_name: String,
    pub visible_name: String,
    pub stream_url: String,
    pub group: Option<String>,
    pub languages: BTreeSet<String>,
    pub countries: BTreeSet<String>,
    pub region: Option<Region>,
    pub logo_url: Option<String>,
    pub channel_number: Option<String>,
    pub has_user_overrides: bool,
}
#[derive(Clone, Debug)]
pub struct ChannelPage {
    pub items: Vec<ChannelEffective>,
    pub total: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOrigin {
    Manual,
    Imported,
    SourceFallback,
    Unknown,
}
#[derive(Clone, Debug)]
pub struct EditProvenance {
    pub name: FieldOrigin,
    pub group: FieldOrigin,
    pub languages: FieldOrigin,
    pub countries: FieldOrigin,
    pub region: FieldOrigin,
}
#[derive(Clone, Debug)]
pub struct ChannelDetail {
    pub effective: ChannelEffective,
    pub imported: IncomingChannel,
    pub source_defaults: SourceDefaults,
    pub overrides: ChannelOverrides,
    pub provenance: EditProvenance,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterDimension {
    Source,
    Group,
    Language,
    Country,
    Region,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FacetValue {
    Known(String),
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FacetChoice {
    pub value: FacetValue,
    pub count: u64,
}
#[derive(Clone, Debug)]
pub struct FilterOptions {
    pub dimension: FilterDimension,
    pub choices: Vec<FacetChoice>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FacetDimension {
    Language,
    Country,
}
impl FacetDimension {
    pub(crate) fn sql(self) -> &'static str {
        match self {
            Self::Language => "language",
            Self::Country => "country",
        }
    }
}
pub fn normalize_facet(raw: &str, dimension: FacetDimension) -> StorageResult<String> {
    let text = raw.trim();
    if text.is_empty() || text.len() > 64 || text.chars().any(char::is_control) {
        return Err(StorageError::InvalidInput);
    }
    match dimension {
        FacetDimension::Country => {
            if text.len() != 2 || !text.is_ascii() || !text.bytes().all(|b| b.is_ascii_alphabetic())
            {
                return Err(StorageError::InvalidInput);
            }
            let code = text.to_ascii_uppercase();
            if !ISO_3166_ALPHA2.split_ascii_whitespace().any(|v| v == code) {
                return Err(StorageError::InvalidInput);
            }
            Ok(code)
        }
        FacetDimension::Language => {
            let parts: Vec<_> = text.split('-').collect();
            if parts.is_empty()
                || parts.len() > 8
                || !(2..=8).contains(&parts[0].len())
                || !parts[0].bytes().all(|b| b.is_ascii_alphabetic())
                || parts.iter().skip(1).any(|p| {
                    p.is_empty() || p.len() > 8 || !p.bytes().all(|b| b.is_ascii_alphanumeric())
                })
            {
                return Err(StorageError::InvalidInput);
            }
            let mut result = parts[0].to_ascii_lowercase();
            for part in parts.into_iter().skip(1) {
                result.push('-');
                if part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic()) {
                    result.push_str(&part.to_ascii_uppercase());
                } else if part.len() == 4 && part.bytes().all(|b| b.is_ascii_alphabetic()) {
                    let mut chars = part.chars();
                    result.push(chars.next().expect("nonempty").to_ascii_uppercase());
                    result.push_str(&chars.as_str().to_ascii_lowercase());
                } else {
                    result.push_str(&part.to_ascii_lowercase());
                }
            }
            Ok(result)
        }
    }
}
fn validate_facets(values: &BTreeSet<String>, dimension: FacetDimension) -> StorageResult<()> {
    if values.len() > 64 {
        return Err(StorageError::InvalidInput);
    }
    for item in values {
        if normalize_facet(item, dimension)? != *item {
            return Err(StorageError::InvalidInput);
        }
    }
    Ok(())
}
pub(crate) fn bounded(value: &str, max: usize) -> StorageResult<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(StorageError::InvalidInput);
    }
    Ok(())
}
pub(crate) fn bounded_optional(value: &Option<String>, max: usize) -> StorageResult<()> {
    if let Some(s) = value {
        bounded(s, max)?;
    }
    Ok(())
}
fn optional_with_empty(value: &Option<String>, max: usize) -> StorageResult<()> {
    bounded_optional(value, max)
}

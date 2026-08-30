use chrono::{DateTime, Utc};
use sqlx::{FromRow, types::Json};

use super::{
    Harness, HarnessError, HarnessProvider, HarnessRevision, HarnessRevisionStatus, JsonObject,
};

pub(super) const HARNESS_COLUMNS: &str = "harness_id, slug, display_name, description, supported_providers, enabled, active_revision_id, created_at, updated_at";

pub(super) const REVISION_COLUMNS: &str = "r.harness_revision_id, r.harness_id, r.revision, r.contract_version, \
     r.default_config, r.config_schema, r.first_activated_at, r.retired_at, r.created_at, \
     h.active_revision_id";

#[derive(FromRow)]
pub(super) struct HarnessRow {
    pub harness_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub supported_providers: Json<Vec<HarnessProvider>>,
    pub enabled: bool,
    pub active_revision_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<HarnessRow> for Harness {
    fn from(row: HarnessRow) -> Self {
        Self {
            harness_id: row.harness_id,
            slug: row.slug,
            display_name: row.display_name,
            description: row.description,
            supported_providers: row.supported_providers.0,
            enabled: row.enabled,
            active_revision_id: row.active_revision_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(FromRow)]
pub(super) struct RevisionRow {
    pub harness_revision_id: String,
    pub harness_id: String,
    pub revision: String,
    pub contract_version: i64,
    pub default_config: Json<JsonObject>,
    pub config_schema: Option<Json<JsonObject>>,
    pub first_activated_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub active_revision_id: Option<String>,
}

impl TryFrom<RevisionRow> for HarnessRevision {
    type Error = HarnessError;

    fn try_from(row: RevisionRow) -> Result<Self, Self::Error> {
        let status = if row.retired_at.is_some() {
            HarnessRevisionStatus::Retired
        } else if row.active_revision_id.as_deref() == Some(&row.harness_revision_id) {
            HarnessRevisionStatus::Active
        } else if row.first_activated_at.is_some() {
            HarnessRevisionStatus::Deprecated
        } else {
            HarnessRevisionStatus::Registered
        };
        let contract_version = u32::try_from(row.contract_version).map_err(|_| {
            HarnessError::InvalidStoredData("contract_version is outside the u32 range".to_owned())
        })?;

        Ok(Self {
            harness_revision_id: row.harness_revision_id,
            harness_id: row.harness_id,
            revision: row.revision,
            contract_version,
            status,
            default_config: row.default_config.0,
            config_schema: row.config_schema.map(|schema| schema.0),
            first_activated_at: row.first_activated_at,
            retired_at: row.retired_at,
            created_at: row.created_at,
        })
    }
}

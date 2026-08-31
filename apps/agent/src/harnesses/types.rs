use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

pub use agent_contracts::{
    HarnessProvider, HarnessReasoningLevel, HarnessRevision, HarnessRevisionStatus,
};

pub type JsonObject = Map<String, Value>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Harness {
    pub harness_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub supported_providers: Vec<HarnessProvider>,
    pub supported_reasoning_levels: Vec<HarnessReasoningLevel>,
    pub enabled: bool,
    pub active_revision_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HarnessActivation {
    pub harness: Harness,
    pub active_revision: HarnessRevision,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateHarness {
    pub harness_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub supported_providers: Vec<HarnessProvider>,
    #[serde(default)]
    pub supported_reasoning_levels: Vec<HarnessReasoningLevel>,
}

impl CreateHarness {
    pub(super) fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        validate_id(&mut issues, "harness_id", &self.harness_id);
        validate_slug(&mut issues, "slug", &self.slug);
        validate_trimmed(&mut issues, "display_name", &self.display_name, 1, 128);
        if let Some(description) = &self.description {
            validate_trimmed(&mut issues, "description", description, 1, 2_000);
        }
        validate_supported_providers(&mut issues, &self.supported_providers);
        validate_reasoning_levels(&mut issues, &self.supported_reasoning_levels);
        finish(issues)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateHarness {
    pub display_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_description")]
    pub description: Option<Option<String>>,
    pub supported_providers: Option<Vec<HarnessProvider>>,
    pub supported_reasoning_levels: Option<Vec<HarnessReasoningLevel>>,
}

impl UpdateHarness {
    pub(super) fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.display_name.is_none()
            && self.description.is_none()
            && self.supported_providers.is_none()
            && self.supported_reasoning_levels.is_none()
        {
            issue(&mut issues, "$", "must update at least one field");
        }
        if let Some(display_name) = &self.display_name {
            validate_trimmed(&mut issues, "display_name", display_name, 1, 128);
        }
        if let Some(Some(description)) = &self.description {
            validate_trimmed(&mut issues, "description", description, 1, 2_000);
        }
        if let Some(supported_providers) = &self.supported_providers {
            validate_supported_providers(&mut issues, supported_providers);
        }
        if let Some(supported_reasoning_levels) = &self.supported_reasoning_levels {
            validate_reasoning_levels(&mut issues, supported_reasoning_levels);
        }
        finish(issues)
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetHarnessEnabled {
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterHarnessRevision {
    pub harness_revision_id: String,
    pub revision: String,
    pub contract_version: u32,
    pub default_config: JsonObject,
    pub config_schema: Option<JsonObject>,
}

impl RegisterHarnessRevision {
    pub(super) fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        validate_id(
            &mut issues,
            "harness_revision_id",
            &self.harness_revision_id,
        );
        validate_trimmed(&mut issues, "revision", &self.revision, 1, 128);
        if self.contract_version == 0 {
            issue(&mut issues, "contract_version", "must be greater than zero");
        }
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetActiveRevision {
    pub harness_revision_id: String,
}

impl SetActiveRevision {
    pub(super) fn validate(&self) -> Result<(), ValidationError> {
        validate_path_id("harness_revision_id", &self.harness_revision_id)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessListQuery {
    pub enabled: Option<bool>,
    pub slug: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

impl HarnessListQuery {
    pub(super) fn validate(&self) -> Result<(), ValidationError> {
        let Some(slug) = &self.slug else {
            return Ok(());
        };
        let mut issues = Vec::new();
        validate_slug(&mut issues, "slug", slug);
        finish(issues)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionListQuery {
    pub status: Option<HarnessRevisionStatus>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ValidationIssue {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("request validation failed")]
pub struct ValidationError {
    pub issues: Vec<ValidationIssue>,
}

pub(super) fn validate_path_id(path: &str, value: &str) -> Result<(), ValidationError> {
    let mut issues = Vec::new();
    validate_id(&mut issues, path, value);
    finish(issues)
}

fn validate_id(issues: &mut Vec<ValidationIssue>, path: &str, value: &str) {
    if value.is_empty() {
        issue(issues, path, "must not be empty");
    } else if value != value.trim() {
        issue(issues, path, "must not have surrounding whitespace");
    }
}

fn validate_slug(issues: &mut Vec<ValidationIssue>, path: &str, value: &str) {
    let bytes = value.as_bytes();
    let valid_length = (1..=64).contains(&bytes.len());
    let valid_first = bytes.first().is_some_and(u8::is_ascii_lowercase);
    let valid_segments = value.split('-').all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    });
    if !valid_length || !valid_first || !valid_segments {
        issue(issues, path, "must be a lowercase slug of at most 64 bytes");
    }
}

fn validate_trimmed(
    issues: &mut Vec<ValidationIssue>,
    path: &str,
    value: &str,
    minimum: usize,
    maximum: usize,
) {
    if value != value.trim() {
        issue(issues, path, "must not have surrounding whitespace");
    }
    let length = value.chars().count();
    if !(minimum..=maximum).contains(&length) {
        issue(
            issues,
            path,
            format!("must contain between {minimum} and {maximum} characters"),
        );
    }
}

fn validate_supported_providers(
    issues: &mut Vec<ValidationIssue>,
    supported_providers: &[HarnessProvider],
) {
    let mut provider_ids = HashSet::new();
    for (provider_index, provider) in supported_providers.iter().enumerate() {
        let provider_path = format!("supported_providers[{provider_index}]");
        validate_id(
            issues,
            &format!("{provider_path}.provider_id"),
            provider.provider_id.as_str(),
        );
        if !provider_ids.insert(provider.provider_id.as_str()) {
            issue(
                issues,
                &format!("{provider_path}.provider_id"),
                "must be unique",
            );
        }
        if provider.model_ids.is_empty() {
            issue(
                issues,
                &format!("{provider_path}.model_ids"),
                "must contain at least one model id",
            );
        }
        let mut model_ids = HashSet::new();
        for (model_index, model_id) in provider.model_ids.iter().enumerate() {
            let model_path = format!("{provider_path}.model_ids[{model_index}]");
            validate_id(issues, &model_path, model_id.as_str());
            if !model_ids.insert(model_id.as_str()) {
                issue(issues, &model_path, "must be unique within the provider");
            }
        }
    }
}

fn validate_reasoning_levels(
    issues: &mut Vec<ValidationIssue>,
    supported_reasoning_levels: &[HarnessReasoningLevel],
) {
    let mut levels = HashSet::new();
    for (index, level) in supported_reasoning_levels.iter().enumerate() {
        if !levels.insert(level) {
            issue(
                issues,
                &format!("supported_reasoning_levels[{index}]"),
                "must be unique",
            );
        }
    }
}

fn issue(issues: &mut Vec<ValidationIssue>, path: &str, message: impl Into<String>) {
    issues.push(ValidationIssue {
        path: path.to_owned(),
        message: message.into(),
    });
}

fn finish(issues: Vec<ValidationIssue>) -> Result<(), ValidationError> {
    if issues.is_empty() {
        Ok(())
    } else {
        Err(ValidationError { issues })
    }
}

fn deserialize_optional_description<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CreateHarness, UpdateHarness};

    #[test]
    fn validates_slug_shape() {
        let valid = CreateHarness {
            harness_id: "harness-1".to_owned(),
            slug: "coding-agent-2".to_owned(),
            display_name: "Coding agent".to_owned(),
            description: None,
            supported_providers: Vec::new(),
            supported_reasoning_levels: Vec::new(),
        };
        assert!(valid.validate().is_ok());

        let invalid = CreateHarness {
            slug: "Coding--Agent".to_owned(),
            ..valid
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn distinguishes_an_absent_description_from_null() {
        let absent: UpdateHarness = serde_json::from_value(json!({})).expect("absent update");
        let clear: UpdateHarness =
            serde_json::from_value(json!({ "description": null })).expect("clear update");

        assert_eq!(absent.description, None);
        assert_eq!(clear.description, Some(None));
    }

    #[test]
    fn validates_supported_provider_catalogs() {
        let duplicate: CreateHarness = serde_json::from_value(json!({
            "harness_id": "harness-1",
            "slug": "coding-agent",
            "display_name": "Coding agent",
            "supported_providers": [
                {"provider_id": "openai", "model_ids": ["gpt-5", "gpt-5"]},
                {"provider_id": "openai", "model_ids": ["gpt-5-mini"]}
            ]
        }))
        .expect("provider catalog");

        let error = duplicate.validate().expect_err("duplicate catalog");
        assert_eq!(error.issues.len(), 2);
        assert_eq!(error.issues[0].path, "supported_providers[0].model_ids[1]");
        assert_eq!(error.issues[1].path, "supported_providers[1].provider_id");

        let empty_models: UpdateHarness = serde_json::from_value(json!({
            "supported_providers": [{"provider_id": "openai", "model_ids": []}]
        }))
        .expect("provider update");
        assert!(empty_models.validate().is_err());

        let clear: UpdateHarness = serde_json::from_value(json!({
            "supported_providers": []
        }))
        .expect("clear providers");
        assert!(clear.validate().is_ok());
    }

    #[test]
    fn validates_unique_reasoning_levels() {
        let duplicate: CreateHarness = serde_json::from_value(json!({
            "harness_id": "harness-1",
            "slug": "coding-agent",
            "display_name": "Coding agent",
            "supported_reasoning_levels": ["low", "low"]
        }))
        .expect("reasoning levels");

        let error = duplicate.validate().expect_err("duplicate reasoning level");
        assert_eq!(error.issues.len(), 1);
        assert_eq!(error.issues[0].path, "supported_reasoning_levels[1]");
    }
}

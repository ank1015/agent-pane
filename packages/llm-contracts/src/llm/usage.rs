use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::validation::{Validate, ValidationError, ValidationIssue, require_non_negative_finite};

/// Cost breakdown in USD.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsageCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    pub total: f64,
}

impl Validate for UsageCost {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        validate_optional_cost(&mut issues, "usage.cost.input", self.input);
        validate_optional_cost(&mut issues, "usage.cost.output", self.output);
        validate_optional_cost(&mut issues, "usage.cost.cache_read", self.cache_read);
        validate_optional_cost(&mut issues, "usage.cost.cache_write", self.cache_write);
        require_non_negative_finite(&mut issues, "usage.cost.total", self.total);
        finish(issues)
    }
}

/// Token usage and optional calculated cost for one model request.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<UsageCost>,
}

impl Validate for Usage {
    fn validate(&self) -> Result<(), ValidationError> {
        self.cost.as_ref().map_or(Ok(()), Validate::validate)
    }
}

fn validate_optional_cost(issues: &mut Vec<ValidationIssue>, path: &str, value: Option<f64>) {
    if let Some(value) = value {
        require_non_negative_finite(issues, path, value);
    }
}

fn finish(issues: Vec<ValidationIssue>) -> Result<(), ValidationError> {
    ValidationError::from_issues(issues).map_or(Ok(()), Err)
}

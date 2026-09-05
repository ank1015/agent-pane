use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A single problem found while validating a contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationIssue {
    pub path: String,
    pub message: String,
}

/// One or more semantic contract validation failures.
#[derive(Clone, Debug, Deserialize, Error, PartialEq, Serialize)]
#[error("contract validation failed: {issues:?}")]
#[serde(deny_unknown_fields)]
pub struct ValidationError {
    pub issues: Vec<ValidationIssue>,
}

impl ValidationError {
    #[must_use]
    pub fn single(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            issues: vec![ValidationIssue {
                path: path.into(),
                message: message.into(),
            }],
        }
    }

    #[must_use]
    pub fn from_issues(issues: Vec<ValidationIssue>) -> Option<Self> {
        (!issues.is_empty()).then_some(Self { issues })
    }
}

/// Semantic validation implemented by top-level contract types.
pub trait Validate {
    fn validate(&self) -> Result<(), ValidationError>;
}

pub(crate) fn issue(
    issues: &mut Vec<ValidationIssue>,
    path: impl Into<String>,
    message: impl Into<String>,
) {
    issues.push(ValidationIssue {
        path: path.into(),
        message: message.into(),
    });
}

pub(crate) fn require_non_empty(issues: &mut Vec<ValidationIssue>, path: &str, value: &str) {
    if value.trim().is_empty() {
        issue(issues, path, "must not be empty");
    }
}

pub(crate) fn require_non_negative_finite(
    issues: &mut Vec<ValidationIssue>,
    path: &str,
    value: f64,
) {
    if !value.is_finite() || value < 0.0 {
        issue(issues, path, "must be finite and non-negative");
    }
}

pub(crate) fn finish(issues: Vec<ValidationIssue>) -> Result<(), ValidationError> {
    ValidationError::from_issues(issues).map_or(Ok(()), Err)
}

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A single semantic validation problem.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationIssue {
    pub path: String,
    pub message: String,
}

/// One or more semantic validation problems.
#[derive(Clone, Debug, Deserialize, Error, Eq, PartialEq, Serialize)]
#[error("validation failed: {issues:?}")]
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

/// Semantic validation for values crossing an execution boundary.
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

pub(crate) fn require_non_empty(
    issues: &mut Vec<ValidationIssue>,
    path: impl Into<String>,
    value: &str,
) {
    if value.trim().is_empty() {
        issue(issues, path, "must not be empty");
    }
}

pub(crate) fn append_nested(
    issues: &mut Vec<ValidationIssue>,
    parent: impl fmt::Display,
    result: Result<(), ValidationError>,
) {
    if let Err(error) = result {
        for nested in error.issues {
            issue(issues, format!("{parent}.{}", nested.path), nested.message);
        }
    }
}

pub(crate) fn finish(issues: Vec<ValidationIssue>) -> Result<(), ValidationError> {
    ValidationError::from_issues(issues).map_or(Ok(()), Err)
}

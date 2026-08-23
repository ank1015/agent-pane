use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

/// A single semantic problem in a contract value.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ValidationIssue {
    pub path: String,
    pub message: String,
}

/// One or more semantic contract failures.
#[derive(Clone, Debug, Deserialize, Error, JsonSchema, PartialEq, Serialize)]
#[error("contract validation failed: {issues:?}")]
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

/// Semantic validation implemented by top-level contract values.
pub trait Validate {
    fn validate(&self) -> Result<(), ValidationError>;
}

/// Error returned when parsing an untrusted JSON contract.
#[derive(Debug, Error)]
pub enum ContractError {
    #[error("invalid contract JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Validation(#[from] ValidationError),
}

/// Deserializes JSON and then checks semantic constraints.
pub fn parse_json<T>(input: &str) -> Result<T, ContractError>
where
    T: DeserializeOwned + Validate,
{
    let value: T = serde_json::from_str(input)?;
    value.validate()?;
    Ok(value)
}

/// Deserializes a JSON value and then checks semantic constraints.
pub fn from_json_value<T>(input: serde_json::Value) -> Result<T, ContractError>
where
    T: DeserializeOwned + Validate,
{
    let value: T = serde_json::from_value(input)?;
    value.validate()?;
    Ok(value)
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

use llm_contracts::{ValidationError, ValidationIssue};

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

pub(crate) fn append_nested(
    issues: &mut Vec<ValidationIssue>,
    prefix: &str,
    error: ValidationError,
) {
    for nested in error.issues {
        issue(issues, format!("{prefix}.{}", nested.path), nested.message);
    }
}

pub(crate) fn finish(issues: Vec<ValidationIssue>) -> Result<(), ValidationError> {
    ValidationError::from_issues(issues).map_or(Ok(()), Err)
}

pub(crate) fn validate_id(issues: &mut Vec<ValidationIssue>, path: &str, value: &str) {
    if value.is_empty() {
        issue(issues, path, "must not be empty");
    } else if value != value.trim() {
        issue(issues, path, "must not have surrounding whitespace");
    }
}

pub(crate) fn validate_trimmed(
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

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{
    ExecutionHostId, RootId, SupervisorGenerationId, Validate, ValidationError,
    validation::{append_nested, finish, issue, require_non_empty},
};

/// Operating system reported by an execution host.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "name", rename_all = "snake_case")]
pub enum OperatingSystem {
    Linux,
    Macos,
    Windows,
    Other(String),
}

/// Native path syntax used by an execution host.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathConvention {
    Unix,
    Windows,
}

/// A filesystem root made available through the execution API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRoot {
    pub id: RootId,
    pub name: String,
    /// Native path used only for display and target-side resolution.
    pub native_path: String,
    #[serde(default)]
    pub read_only: bool,
}

impl Validate for ExecutionRoot {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        require_non_empty(&mut issues, "name", &self.name);
        require_non_empty(&mut issues, "native_path", &self.native_path);
        finish(issues)
    }
}

/// Exact optional behavior supported by one execution host.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionFeatures {
    pub pty: bool,
    pub process_signals: bool,
    pub file_revisions: bool,
}

/// Limits callers can use to bound individual requests.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_read_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_write_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_process_read_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_process_input_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_processes: Option<u32>,
}

impl Validate for ExecutionLimits {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.max_read_bytes == Some(0) {
            issue(&mut issues, "max_read_bytes", "must be greater than zero");
        }
        if self.max_write_bytes == Some(0) {
            issue(&mut issues, "max_write_bytes", "must be greater than zero");
        }
        if self.max_process_read_bytes == Some(0) {
            issue(
                &mut issues,
                "max_process_read_bytes",
                "must be greater than zero",
            );
        }
        if self.max_process_input_bytes == Some(0) {
            issue(
                &mut issues,
                "max_process_input_bytes",
                "must be greater than zero",
            );
        }
        if self.max_concurrent_processes == Some(0) {
            issue(
                &mut issues,
                "max_concurrent_processes",
                "must be greater than zero",
            );
        }
        finish(issues)
    }
}

/// Description of a concrete execution host and supervisor generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionHostDescriptor {
    pub host_id: ExecutionHostId,
    pub supervisor_generation_id: SupervisorGenerationId,
    pub operating_system: OperatingSystem,
    pub architecture: String,
    pub path_convention: PathConvention,
    pub roots: Vec<ExecutionRoot>,
    pub features: ExecutionFeatures,
    #[serde(default)]
    pub limits: ExecutionLimits,
}

impl Validate for ExecutionHostDescriptor {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        require_non_empty(&mut issues, "architecture", &self.architecture);
        if let OperatingSystem::Other(name) = &self.operating_system {
            require_non_empty(&mut issues, "operating_system.name", name);
        }
        if self.roots.is_empty() {
            issue(&mut issues, "roots", "must contain at least one root");
        }

        let mut root_ids = HashSet::new();
        for (index, root) in self.roots.iter().enumerate() {
            append_nested(&mut issues, format!("roots[{index}]"), root.validate());
            if !root_ids.insert(root.id.clone()) {
                issue(
                    &mut issues,
                    format!("roots[{index}].id"),
                    "must be unique within the descriptor",
                );
            }
        }

        append_nested(&mut issues, "limits", self.limits.validate());
        if !self.features.file_revisions {
            issue(
                &mut issues,
                "features.file_revisions",
                "must be enabled for the execution-core filesystem contract",
            );
        }
        finish(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(id: &str) -> ExecutionRoot {
        ExecutionRoot {
            id: RootId::new(id).expect("valid root ID"),
            name: id.to_string(),
            native_path: format!("/tmp/{id}"),
            read_only: false,
        }
    }

    fn descriptor() -> ExecutionHostDescriptor {
        ExecutionHostDescriptor {
            host_id: ExecutionHostId::generate(),
            supervisor_generation_id: SupervisorGenerationId::generate(),
            operating_system: OperatingSystem::Linux,
            architecture: "x86_64".to_string(),
            path_convention: PathConvention::Unix,
            roots: vec![root("workspace")],
            features: ExecutionFeatures {
                pty: true,
                process_signals: true,
                file_revisions: true,
            },
            limits: ExecutionLimits::default(),
        }
    }

    #[test]
    fn descriptor_rejects_duplicate_root_ids() {
        let mut descriptor = descriptor();
        descriptor.roots.push(root("workspace"));
        let error = descriptor
            .validate()
            .expect_err("duplicate roots should fail");
        assert!(error.issues.iter().any(|value| value.path == "roots[1].id"));
    }

    #[test]
    fn descriptor_requires_file_revisions() {
        let mut descriptor = descriptor();
        descriptor.features.file_revisions = false;
        let error = descriptor
            .validate()
            .expect_err("missing revisions should fail");
        assert!(
            error
                .issues
                .iter()
                .any(|value| value.path == "features.file_revisions")
        );
    }
}

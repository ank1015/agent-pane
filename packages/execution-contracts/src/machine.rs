use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Capability, MachineId, ProtocolVersion, Validate, ValidationError, WorkspaceRoot,
    validation::{append_nested, finish, issue, require_non_empty},
};

/// Operating system reported by an execution machine.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperatingSystem {
    Linux,
    Macos,
    Windows,
    FreeBsd,
    Other { name: String },
}

/// Native path syntax used by a machine.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathConvention {
    Posix,
    Windows,
}

/// Default shell selected on the target rather than inferred by a cloud host.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ShellDescriptor {
    pub name: String,
    pub executable: String,
}

/// Serializable description and capability manifest for a machine.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MachineDescriptor {
    pub protocol_version: ProtocolVersion,
    pub machine_id: MachineId,
    pub name: String,
    pub operating_system: OperatingSystem,
    pub architecture: String,
    pub path_convention: PathConvention,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_shell: Option<ShellDescriptor>,
    #[serde(default)]
    pub workspace_roots: Vec<WorkspaceRoot>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

impl Validate for MachineDescriptor {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        append_nested(&mut issues, "machine", self.protocol_version.validate());
        require_non_empty(&mut issues, "machine.name", &self.name);
        require_non_empty(&mut issues, "machine.architecture", &self.architecture);
        if let OperatingSystem::Other { name } = &self.operating_system {
            require_non_empty(&mut issues, "machine.operating_system.name", name);
        }
        if let Some(shell) = &self.default_shell {
            require_non_empty(&mut issues, "machine.default_shell.name", &shell.name);
            require_non_empty(
                &mut issues,
                "machine.default_shell.executable",
                &shell.executable,
            );
        }

        let mut root_ids = HashSet::new();
        for (index, root) in self.workspace_roots.iter().enumerate() {
            append_nested(
                &mut issues,
                format_args!("machine.workspace_roots[{index}]"),
                root.validate(),
            );
            if !root_ids.insert(&root.id) {
                issue(
                    &mut issues,
                    "machine.workspace_roots",
                    format!("workspace root `{}` is duplicated", root.id),
                );
            }
        }

        let mut capabilities = HashSet::new();
        for (index, capability) in self.capabilities.iter().enumerate() {
            append_nested(
                &mut issues,
                format_args!("machine.capabilities[{index}]"),
                capability.validate(),
            );
            if !capabilities.insert((&capability.id, capability.major)) {
                issue(
                    &mut issues,
                    "machine.capabilities",
                    format!(
                        "capability `{}` major version {} is duplicated",
                        capability.id, capability.major
                    ),
                );
            }
        }
        finish(issues)
    }
}

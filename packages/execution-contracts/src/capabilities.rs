use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    CapabilityId, Validate, ValidationError,
    validation::{finish, issue},
};

pub const WORKSPACE_QUERY_CAPABILITY: &str = "workspace.query";
pub const WORKSPACE_MUTATION_CAPABILITY: &str = "workspace.mutation";
pub const PROCESS_SESSION_CAPABILITY: &str = "process.session";
pub const ARTIFACTS_CAPABILITY: &str = "artifacts";
pub const BASIC_FILESYSTEM_CAPABILITY: &str = "filesystem.basic";
pub const CODE_INTELLIGENCE_CAPABILITY: &str = "code_intelligence";
pub const MEDIA_PROCESSING_CAPABILITY: &str = "media_processing";

/// Version of the overall execution protocol.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const V1: Self = Self { major: 1, minor: 0 };
}

impl Validate for ProtocolVersion {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.major == 0 {
            issue(
                &mut issues,
                "protocol_version.major",
                "must be greater than zero",
            );
        }
        finish(issues)
    }
}

/// Independently versioned environment capability.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct Capability {
    pub id: CapabilityId,
    pub major: u16,
    pub minor: u16,
}

impl Capability {
    pub fn v1(id: impl Into<String>) -> Result<Self, ValidationError> {
        Ok(Self {
            id: CapabilityId::new(id)?,
            major: 1,
            minor: 0,
        })
    }
}

impl Validate for Capability {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.major == 0 {
            issue(&mut issues, "capability.major", "must be greater than zero");
        }
        finish(issues)
    }
}

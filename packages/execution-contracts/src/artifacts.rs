use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ArtifactId, Base64Data, TimestampMs, Validate, ValidationError,
    validation::{finish, issue},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    ProcessOutput,
    File,
    Media,
    MutationPreview,
    Other,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ArtifactMetadata {
    pub artifact_id: ArtifactId,
    pub kind: ArtifactKind,
    pub size: u64,
    pub created_at: TimestampMs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<TimestampMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct GetArtifactMetadataRequest {
    pub artifact_id: ArtifactId,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct OpenArtifactRequest {
    pub artifact_id: ArtifactId,
    #[serde(default)]
    pub offset: u64,
    pub max_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ArtifactChunk {
    pub artifact_id: ArtifactId,
    pub offset: u64,
    pub data: Base64Data,
    pub eof: bool,
}

impl Validate for OpenArtifactRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.max_bytes == 0 {
            issue(
                &mut issues,
                "artifact.max_bytes",
                "must be greater than zero",
            );
        }
        finish(issues)
    }
}

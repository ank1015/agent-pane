use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ArtifactId;

/// Unix timestamp in milliseconds.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct TimestampMs(pub u64);

/// Bytes encoded with the standard Base64 alphabet.
///
/// Keeping the encoding explicit makes generated schemas agree with JSON. Binary
/// transports may carry the decoded bytes in transport-specific frames instead.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Base64Data(pub String);

/// Content supplied inline or by reference to an existing artifact.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentSource {
    Text {
        content: String,
    },
    Base64 {
        data: Base64Data,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    Artifact {
        artifact_id: ArtifactId,
    },
}

/// Binary content returned inline or spilled to the artifact store.
///
/// Keeping this separate from [`ContentSource`] prevents a filesystem read from
/// claiming that arbitrary bytes are UTF-8 text.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BinaryContent {
    Base64 {
        data: Base64Data,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    Artifact {
        artifact_id: ArtifactId,
    },
}

/// Half-open byte range beginning at `offset`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ByteRange {
    pub offset: u64,
    pub length: u64,
}

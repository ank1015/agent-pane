use std::{fmt, str::FromStr};

use execution_core::{
    CreateDirectoryRequest, ExecutionError, ExecutionHandle, ExecutionHostDescriptor, FileMetadata,
    ListDirectoryRequest, ListDirectoryResult, ReadExecutionRequest, ReadExecutionResult,
    ReadFileRequest, ReadFileResult, RemovePathRequest, RemovePathResult, ResizePtyRequest,
    SignalExecutionRequest, StartExecutionRequest, StatRequest, TerminateExecutionRequest,
    TerminateExecutionResult, Validate, ValidationError, WriteFileRequest, WriteFileResult,
    WriteProcessInputRequest, WriteProcessInputResult,
};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// Stable name of the execution protocol, independent of its numeric version.
pub const PROTOCOL_NAME: &str = "agent-pane.execution";

/// Wire version implemented by this crate.
pub const PROTOCOL_VERSION: u32 = 1;

/// Correlates one request with exactly one response.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RequestId(String);

impl RequestId {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ValidationError::single("request_id", "must not be empty"));
        }
        if value.contains('\0') {
            return Err(ValidationError::single(
                "request_id",
                "must not contain a null byte",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for RequestId {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for RequestId {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A provider-neutral operation addressed to one execution runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "operation",
    content = "request",
    rename_all = "snake_case"
)]
pub enum Operation {
    Describe,
    FilesystemStat(StatRequest),
    FilesystemRead(ReadFileRequest),
    FilesystemWrite(WriteFileRequest),
    FilesystemCreateDirectory(CreateDirectoryRequest),
    FilesystemRemove(RemovePathRequest),
    FilesystemList(ListDirectoryRequest),
    ProcessStart(StartExecutionRequest),
    ProcessRead(ReadExecutionRequest),
    ProcessWrite(WriteProcessInputRequest),
    ProcessResize(ResizePtyRequest),
    ProcessSignal(SignalExecutionRequest),
    ProcessTerminate(TerminateExecutionRequest),
}

impl Operation {
    /// Applies semantic validation before an operation reaches an implementation.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Describe
            | Self::ProcessWrite(_)
            | Self::ProcessSignal(_)
            | Self::ProcessTerminate(_) => Ok(()),
            Self::FilesystemStat(request) => request.validate(),
            Self::FilesystemRead(request) => request.validate(),
            Self::FilesystemWrite(request) => request.validate(),
            Self::FilesystemCreateDirectory(request) => request.validate(),
            Self::FilesystemRemove(request) => request.validate(),
            Self::FilesystemList(request) => request.validate(),
            Self::ProcessStart(request) => request.validate(),
            Self::ProcessRead(request) => request.validate(),
            Self::ProcessResize(request) => request.validate(),
        }
    }
}

/// Successful result of one [`Operation`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "result",
    content = "value",
    rename_all = "snake_case"
)]
pub enum OperationResult {
    HostDescriptor(ExecutionHostDescriptor),
    FileMetadata(FileMetadata),
    ReadFile(ReadFileResult),
    WriteFile(WriteFileResult),
    RemovePath(RemovePathResult),
    ListDirectory(ListDirectoryResult),
    ExecutionHandle(ExecutionHandle),
    ReadExecution(ReadExecutionResult),
    ProcessInput(WriteProcessInputResult),
    TerminateExecution(TerminateExecutionResult),
    Unit,
}

/// Versioned request sent to an execution endpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub version: u32,
    pub request_id: RequestId,
    pub operation: Operation,
}

impl RequestEnvelope {
    #[must_use]
    pub fn new(request_id: RequestId, operation: Operation) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            operation,
        }
    }
}

/// Versioned response returned by an execution endpoint.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "status", rename_all = "snake_case")]
pub enum ResponseEnvelope {
    Success {
        version: u32,
        request_id: RequestId,
        result: OperationResult,
    },
    Error {
        version: u32,
        request_id: RequestId,
        error: ExecutionError,
    },
}

impl ResponseEnvelope {
    #[must_use]
    pub fn success(request_id: RequestId, result: OperationResult) -> Self {
        Self::Success {
            version: PROTOCOL_VERSION,
            request_id,
            result,
        }
    }

    #[must_use]
    pub fn error(request_id: RequestId, error: ExecutionError) -> Self {
        Self::Error {
            version: PROTOCOL_VERSION,
            request_id,
            error,
        }
    }

    #[must_use]
    pub const fn version(&self) -> u32 {
        match self {
            Self::Success { version, .. } | Self::Error { version, .. } => *version,
        }
    }

    #[must_use]
    pub fn request_id(&self) -> &RequestId {
        match self {
            Self::Success { request_id, .. } | Self::Error { request_id, .. } => request_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use execution_core::{
        ExecutionErrorCode, ExecutionId, ReadExecutionRequest, SupervisorGenerationId,
    };

    use super::*;

    #[test]
    fn request_ids_validate_at_construction_and_deserialization() {
        RequestId::new("request-1").expect("valid request ID");
        RequestId::new(" ").expect_err("blank request ID must fail");
        serde_json::from_str::<RequestId>("\"\"")
            .expect_err("blank serialized request ID must fail");
    }

    #[test]
    fn describe_request_has_a_compact_stable_shape() {
        let request = RequestEnvelope::new(
            RequestId::new("request-1").expect("valid request ID"),
            Operation::Describe,
        );
        let value = serde_json::to_value(request).expect("serialize request");
        assert_eq!(value["version"], PROTOCOL_VERSION);
        assert_eq!(value["request_id"], "request-1");
        assert_eq!(value["operation"]["operation"], "describe");
        assert!(value["operation"].get("request").is_none());
    }

    #[test]
    fn request_envelopes_and_operations_reject_unknown_fields() {
        serde_json::from_str::<RequestEnvelope>(
            r#"{"version":1,"request_id":"request-1","operation":{"operation":"describe"},"extra":true}"#,
        )
        .expect_err("unknown envelope field must fail");
        serde_json::from_str::<RequestEnvelope>(
            r#"{"version":1,"request_id":"request-1","operation":{"operation":"describe","extra":true}}"#,
        )
        .expect_err("unknown operation field must fail");
    }

    #[test]
    fn operation_round_trips_core_request_types() {
        let operation = Operation::ProcessRead(ReadExecutionRequest {
            execution_id: ExecutionId::generate(),
            supervisor_generation_id: SupervisorGenerationId::generate(),
            after_sequence: 41,
            max_bytes: 1024,
            wait_ms: Some(500),
        });
        let json = serde_json::to_string(&operation).expect("serialize operation");
        let decoded: Operation = serde_json::from_str(&json).expect("deserialize operation");
        assert_eq!(decoded, operation);
    }

    #[test]
    fn error_response_round_trips_structured_execution_errors() {
        let response = ResponseEnvelope::error(
            RequestId::new("request-1").expect("valid request ID"),
            ExecutionError::new(ExecutionErrorCode::Unavailable, "temporarily unavailable")
                .retryable(true),
        );
        let json = serde_json::to_string(&response).expect("serialize response");
        let decoded: ResponseEnvelope = serde_json::from_str(&json).expect("deserialize response");
        assert_eq!(decoded, response);
    }

    #[test]
    fn operation_validation_rejects_unbounded_process_reads() {
        let operation = Operation::ProcessRead(ReadExecutionRequest {
            execution_id: ExecutionId::generate(),
            supervisor_generation_id: SupervisorGenerationId::generate(),
            after_sequence: 0,
            max_bytes: 0,
            wait_ms: None,
        });
        operation
            .validate()
            .expect_err("zero-byte process read must fail");
    }
}

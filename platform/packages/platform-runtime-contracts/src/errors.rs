use serde::{Deserialize, Serialize};

/// Recovery decisions must use codes, never human-readable messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConflictCode {
    LeaseLost,
    RunVersionConflict,
    SessionRevisionConflict,
    CheckpointVersionConflict,
    IdempotencyKeyConflict,
    WorkerIdentityConflict,
    WorkerStateConflict,
    InputStateConflict,
    WaitStateConflict,
    RunStateConflict,
    SessionStateConflict,
    HarnessDisabled,
    RuntimeConstraintConflict,
    RuntimeConflict,
}

impl ConflictCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LeaseLost => "LEASE_LOST",
            Self::RunVersionConflict => "RUN_VERSION_CONFLICT",
            Self::SessionRevisionConflict => "SESSION_REVISION_CONFLICT",
            Self::CheckpointVersionConflict => "CHECKPOINT_VERSION_CONFLICT",
            Self::IdempotencyKeyConflict => "IDEMPOTENCY_KEY_CONFLICT",
            Self::WorkerIdentityConflict => "WORKER_IDENTITY_CONFLICT",
            Self::WorkerStateConflict => "WORKER_STATE_CONFLICT",
            Self::InputStateConflict => "INPUT_STATE_CONFLICT",
            Self::WaitStateConflict => "WAIT_STATE_CONFLICT",
            Self::RunStateConflict => "RUN_STATE_CONFLICT",
            Self::SessionStateConflict => "SESSION_STATE_CONFLICT",
            Self::HarnessDisabled => "HARNESS_DISABLED",
            Self::RuntimeConstraintConflict => "RUNTIME_CONSTRAINT_CONFLICT",
            Self::RuntimeConflict => "RUNTIME_CONFLICT",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ErrorInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorInfo {
    /// Keep unknown future codes intact rather than failing to decode the error.
    pub code: String,
    pub message: String,
}

impl ErrorInfo {
    pub fn conflict(&self) -> Option<ConflictCode> {
        serde_json::from_value(serde_json::Value::String(self.code.clone())).ok()
    }
}

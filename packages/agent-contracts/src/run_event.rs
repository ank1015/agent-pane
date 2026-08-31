use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    HARNESS_PROTOCOL_VERSION, RunStatus, WaitResolutionSource,
    validation::{finish, issue, validate_trimmed},
};

pub const HARNESS_RUN_EVENT_SUBJECT_PATTERN: &str = "agent.run.*.event.v1";
pub const RUN_INPUT_SUBJECT_PATTERN: &str = "agent.run.*.>";

#[must_use]
pub fn harness_run_event_subject(run_id: Uuid) -> String {
    format!("agent.run.{run_id}.event.v1")
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunEventSource {
    Agent,
    Harness,
}

impl RunEventSource {
    #[must_use]
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Harness => "harness",
        }
    }

    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "harness" => Some(Self::Harness),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndReason {
    Continued,
    Waiting,
    Completed,
    Failed,
    Aborted,
}

/// A durable, ordered observation from a run.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunEvent {
    pub event_id: Uuid,
    pub run_id: Uuid,
    pub sequence: u64,
    pub turn_number: Option<u32>,
    pub state_version: u64,
    pub run_status: RunStatus,
    pub source: RunEventSource,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    #[serde(flatten)]
    pub event: RunEventData,
}

impl RunEvent {
    #[must_use]
    pub const fn terminal(&self) -> bool {
        matches!(
            self.event,
            RunEventData::RunCompleted { .. }
                | RunEventData::RunFailed { .. }
                | RunEventData::RunAborted { .. }
        )
    }

    #[must_use]
    pub const fn sse_name(&self) -> &'static str {
        self.event.sse_name()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "details", rename_all = "snake_case")]
pub enum RunEventData {
    RunStarted,
    TurnRequested,
    TurnStarted,
    TurnEnded {
        reason: TurnEndReason,
    },
    RunWaiting {
        wait_id: Uuid,
        kind: String,
        expires_at: Option<DateTime<Utc>>,
    },
    RunResumed {
        wait_id: Uuid,
        source: WaitResolutionSource,
    },
    RunCompleted {
        final_message_id: Uuid,
    },
    RunFailed {
        failure: JsonObject,
    },
    RunAborted {
        abort_id: Uuid,
        reason: Option<String>,
    },
    Progress {
        name: String,
        #[serde(default)]
        data: JsonObject,
    },
}

impl RunEventData {
    #[must_use]
    pub const fn sse_name(&self) -> &'static str {
        match self {
            Self::RunStarted => "run.started",
            Self::TurnRequested => "turn.requested",
            Self::TurnStarted => "turn.started",
            Self::TurnEnded { .. } => "turn.ended",
            Self::RunWaiting { .. } => "run.waiting",
            Self::RunResumed { .. } => "run.resumed",
            Self::RunCompleted { .. } => "run.completed",
            Self::RunFailed { .. } => "run.failed",
            Self::RunAborted { .. } => "run.aborted",
            Self::Progress { .. } => "progress",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEventListQuery {
    pub after_sequence: Option<u64>,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunEventPage {
    pub items: Vec<RunEvent>,
    pub next_after_sequence: Option<u64>,
}

/// A non-authoritative observation published by the harness handling a turn.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessRunEvent {
    pub protocol_version: u32,
    pub event_id: Uuid,
    pub emitted_at: DateTime<Utc>,
    pub run_id: Uuid,
    pub harness_slug: String,
    pub turn_number: u32,
    pub expected_state_version: u64,
    #[serde(flatten)]
    pub event: HarnessRunEventData,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "details", rename_all = "snake_case")]
pub enum HarnessRunEventData {
    TurnStarted,
    Progress {
        name: String,
        #[serde(default)]
        data: JsonObject,
    },
}

impl Validate for HarnessRunEvent {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.protocol_version != HARNESS_PROTOCOL_VERSION {
            issue(
                &mut issues,
                "protocol_version",
                format!("must equal {HARNESS_PROTOCOL_VERSION}"),
            );
        }
        if self.event_id.is_nil() {
            issue(&mut issues, "event_id", "must not be nil");
        }
        if self.run_id.is_nil() {
            issue(&mut issues, "run_id", "must not be nil");
        }
        validate_trimmed(&mut issues, "harness_slug", &self.harness_slug, 1, 64);
        if self.turn_number == 0 {
            issue(&mut issues, "turn_number", "must be greater than zero");
        }
        if self.expected_state_version == 0 {
            issue(
                &mut issues,
                "expected_state_version",
                "must be greater than zero",
            );
        }
        if let HarnessRunEventData::Progress { name, .. } = &self.event {
            validate_trimmed(&mut issues, "details.name", name, 1, 128);
        }
        finish(issues)
    }
}

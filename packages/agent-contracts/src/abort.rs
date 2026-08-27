use chrono::{DateTime, Utc};
use llm_contracts::JsonObject;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Durable record of an immediate control-plane cancellation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunAbort {
    pub abort_id: Uuid,
    pub run_id: Uuid,
    pub turn_number: u32,
    pub reason: Option<String>,
    pub payload: JsonObject,
    pub requested_at: DateTime<Utc>,
}

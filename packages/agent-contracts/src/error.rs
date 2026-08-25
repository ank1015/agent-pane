use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AgentErrorResponse {
    pub error: AgentApiError,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AgentApiError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

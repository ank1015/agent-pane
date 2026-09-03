//! Stable public resource and Host Daemon contracts for the execution gateway.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use execution_core::ExecutionHostDescriptor;
use execution_wire::{RequestEnvelope, ResponseEnvelope};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum E2bAccountStatus {
    Active,
    Invalid,
    Disabled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct E2bAccount {
    pub id: Uuid,
    pub name: String,
    pub credential_fingerprint: String,
    pub is_default: bool,
    pub status: E2bAccountStatus,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateE2bAccountRequest {
    pub name: String,
    pub api_key: String,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateE2bAccountRequest {
    pub name: Option<String>,
    pub is_default: Option<bool>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceE2bCredentialRequest {
    pub api_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionHostKind {
    E2b,
    Registered,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredHostState {
    Ready,
    Paused,
    Deleted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionHostState {
    Provisioning,
    Ready,
    Pausing,
    Paused,
    Resuming,
    Unavailable,
    Deleting,
    Deleted,
    Failed,
    Lost,
}

/// The only supported E2B creation sources.
///
/// `base` always means the private, gateway-selected execution template. A
/// caller cannot provide an arbitrary E2B template ID. `snapshot` accepts only
/// a logical snapshot ID previously created and stored by this gateway.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum E2bHostSource {
    Base {
        #[serde(default)]
        e2b_account_id: Option<Uuid>,
    },
    Snapshot {
        snapshot_id: Uuid,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateExecutionHostRequest {
    pub name: Option<String>,
    pub source: E2bHostSource,
    pub timeout_seconds: Option<u64>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRegisteredHostRequest {
    pub name: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredHostRegistration {
    pub host: ExecutionHost,
    pub registration_token: String,
    pub registration_token_expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenewRegisteredHostTokenResponse {
    pub host_id: Uuid,
    pub registration_token: String,
    pub registration_token_expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRegisteredHostRequest {
    pub registration_token: String,
    pub installation_id: Uuid,
    pub daemon_version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRegisteredHostResponse {
    pub host_id: Uuid,
    pub credential: String,
    pub websocket_url: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateExecutionHostRequest {
    pub name: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct E2bHostBinding {
    pub e2b_account_id: Uuid,
    pub e2b_sandbox_id: Option<String>,
    pub source: E2bHostSource,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredHostBinding {
    pub installation_id: Option<Uuid>,
    pub daemon_version: Option<String>,
    pub protocol_version: Option<u32>,
    pub registered_at: Option<DateTime<Utc>>,
    pub last_connected_at: Option<DateTime<Utc>>,
    pub last_disconnected_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionHost {
    pub id: Uuid,
    pub kind: ExecutionHostKind,
    pub name: Option<String>,
    pub desired_state: DesiredHostState,
    pub state: ExecutionHostState,
    pub status_code: Option<String>,
    pub status_message: Option<String>,
    pub status_retryable: bool,
    pub descriptor: Option<ExecutionHostDescriptor>,
    pub metadata: Value,
    pub e2b: Option<E2bHostBinding>,
    pub registered: Option<RegisteredHostBinding>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Messages sent by a Host Daemon over its authenticated WebSocket.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum HostDaemonMessage {
    Hello {
        protocol_name: String,
        protocol_version: u32,
        daemon_version: String,
        daemon_instance_id: Uuid,
        descriptor: Box<ExecutionHostDescriptor>,
    },
    Response {
        response: Box<ResponseEnvelope>,
    },
}

/// Messages sent by the Execution Gateway to a connected Host Daemon.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum GatewayHostMessage {
    Welcome { heartbeat_interval_ms: u64 },
    Request { request: Box<RequestEnvelope> },
    Disconnect { code: String, message: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredSnapshotState {
    Ready,
    Deleted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotState {
    Creating,
    Ready,
    Deleting,
    Deleted,
    Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct E2bSnapshot {
    pub id: Uuid,
    pub e2b_account_id: Uuid,
    pub e2b_snapshot_id: Option<String>,
    pub source_host_id: Option<Uuid>,
    pub name: Option<String>,
    pub desired_state: DesiredSnapshotState,
    pub state: SnapshotState,
    pub status_code: Option<String>,
    pub status_message: Option<String>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSnapshotRequest {
    pub name: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSnapshotRequest {
    pub name: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiErrorBody {
    pub error: ApiError,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VersionResponse {
    pub program: String,
    pub version: String,
    pub execution_protocol: String,
    pub execution_protocol_version: u32,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_source_does_not_accept_a_template_id() {
        let error = serde_json::from_value::<E2bHostSource>(serde_json::json!({
            "type": "base",
            "template_id": "arbitrary"
        }))
        .expect_err("arbitrary template IDs must be rejected");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn snapshot_source_contains_only_a_gateway_snapshot_id() {
        let snapshot_id = Uuid::now_v7();
        let source = serde_json::from_value::<E2bHostSource>(serde_json::json!({
            "type": "snapshot",
            "snapshot_id": snapshot_id
        }))
        .expect("snapshot source");
        assert_eq!(source, E2bHostSource::Snapshot { snapshot_id });
    }
}

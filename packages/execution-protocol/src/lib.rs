//! Transport-independent envelopes shared by execution gateways, daemons, and clients.

use execution_contracts::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROTOCOL_NAME: &str = "agent-pane.machine.v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Request {
        request_id: String,
        operation: Box<Operation>,
    },
    Cancel {
        request_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready {
        protocol: String,
        descriptor: MachineDescriptor,
    },
    Response {
        request_id: String,
        response: Response,
    },
    StreamItem {
        request_id: String,
        item: StreamItem,
    },
    StreamEnd {
        request_id: String,
    },
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        error: ExecutionError,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub enum Operation {
    Describe,
    WorkspaceInspect(InspectRequest),
    WorkspaceInspectMany(InspectManyRequest),
    WorkspaceRead(ReadRequest),
    WorkspaceList(ListRequest),
    WorkspaceSearch(SearchRequest),
    MutationPrepare(PrepareMutationRequest),
    MutationCommit(CommitMutationRequest),
    MutationAbort(AbortMutationRequest),
    MutationApply(ApplyMutationRequest),
    ProcessStart(StartExecutionRequest),
    ProcessInspect(InspectExecutionRequest),
    ProcessAttach(AttachExecutionRequest),
    ProcessWriteInput(WriteProcessInputRequest),
    ProcessResizePty(ResizePtyRequest),
    ProcessSignal(SignalExecutionRequest),
    ProcessTerminate(TerminateExecutionRequest),
    ArtifactMetadata(GetArtifactMetadataRequest),
    ArtifactOpen(OpenArtifactRequest),
    FilesystemInspect(InspectRequest),
    FilesystemReadBytes(ReadBytesRequest),
    FilesystemWriteBytes(WriteBytesRequest),
    FilesystemCreateDirectory(CreateDirectoryRequest),
    FilesystemRemove(RemovePathRequest),
    FilesystemMove(MovePathRequest),
    FilesystemCopy(CopyPathRequest),
    FilesystemListRaw(ListRequest),
    FilesystemWalk(WalkRequest),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum Response {
    MachineDescriptor(MachineDescriptor),
    FileMetadata(FileMetadata),
    InspectMany(InspectManyResult),
    Read(ReadResult),
    List(ListResult),
    Search(SearchResult),
    PreparedMutation(PreparedMutation),
    Mutation(MutationResult),
    ExecutionHandle(ExecutionHandle),
    ExecutionStatus(ExecutionStatus),
    ProcessInput(WriteProcessInputResult),
    Terminated(TerminateExecutionResult),
    ArtifactMetadata(ArtifactMetadata),
    ReadBytes(ReadBytesResult),
    WriteBytes(WriteBytesResult),
    Walk(WalkResult),
    Unit,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "item", content = "value", rename_all = "snake_case")]
pub enum StreamItem {
    ProcessEvent(ProcessEvent),
    ArtifactChunk(ArtifactChunk),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorKind {
    MachineDaemon,
    Sandbox,
    E2b,
    Ssh,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateRegistrationRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_seconds: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RegistrationCreated {
    pub registration_id: String,
    pub registration_token: String,
    pub expires_at: TimestampMs,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClaimMachineRequest {
    pub registration_token: String,
    pub descriptor: MachineDescriptor,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClaimMachineResponse {
    pub machine_id: MachineId,
    pub credential: String,
    pub websocket_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MachineSummary {
    pub machine_id: MachineId,
    pub name: String,
    pub connector: ConnectorKind,
    pub online: bool,
    pub descriptor: MachineDescriptor,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<TimestampMs>,
}

/// Minimal sandbox-account metadata exposed to execution API clients.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxAccountSummary {
    pub account_id: String,
    pub account_name: String,
    pub provider_name: String,
}

/// Creates a provider sandbox, optionally from a saved snapshot.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSandboxMachineRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<Uuid>,
}

/// A sandbox machine created by the execution gateway.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SandboxMachineCreated {
    pub machine: MachineSummary,
}

/// A snapshot created from a sandbox machine.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxSnapshotCreated {
    pub snapshot_id: Uuid,
}

/// Creates a reusable sandbox environment template for a project.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSandboxTemplateEnvironmentRequest {
    pub project_id: Uuid,
    pub snapshot_id: Uuid,
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub creation_script: String,
}

/// Updates a reusable sandbox environment template for a project.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSandboxTemplateEnvironmentRequest {
    pub project_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_script: Option<String>,
}

/// A saved working location on a registered machine.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Environment {
    pub environment_id: EnvironmentId,
    pub project_id: Uuid,
    pub machine_id: MachineId,
    pub name: String,
    pub workspace_root_id: WorkspaceRootId,
    pub path: String,
    pub created_at: TimestampMs,
}

/// Creates an immutable saved working location.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateEnvironmentRequest {
    pub project_id: Uuid,
    pub machine_id: MachineId,
    pub name: String,
    pub workspace_root_id: WorkspaceRootId,
    pub path: String,
}

/// Updates a saved working location while preserving its project ownership.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateEnvironmentRequest {
    pub project_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root_id: Option<WorkspaceRootId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// A configured machine environment or sandbox template owned by a project.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectEnvironment {
    pub id: String,
    pub name: String,
    pub host_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root_id: Option<WorkspaceRootId>,
    pub path: String,
    #[serde(rename = "type")]
    pub environment_type: ProjectEnvironmentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_script: Option<String>,
    pub created_at: TimestampMs,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectEnvironmentType {
    Env,
    Template,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateOperationRequest {
    pub operation: Box<Operation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OperationRecord {
    pub operation_id: String,
    pub machine_id: MachineId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<EnvironmentId>,
    pub status: OperationStatus,
    pub operation: Box<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Response>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ExecutionError>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OperationEvent {
    pub sequence: u64,
    pub item: StreamItem,
    pub created_at: TimestampMs,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_envelope_has_a_stable_tagged_shape() {
        let message = ClientMessage::Request {
            request_id: "request-1".to_owned(),
            operation: Box::new(Operation::Describe),
        };
        let value = serde_json::to_value(message).expect("serialize request");
        assert_eq!(value["type"], "request");
        assert_eq!(value["operation"]["operation"], "describe");
    }

    #[test]
    fn project_machine_environment_omits_snapshot_id() {
        let environment = ProjectEnvironment {
            id: "environment-1".to_owned(),
            name: "Local project".to_owned(),
            host_name: "Workstation".to_owned(),
            machine_id: Some(MachineId::new("machine-1").unwrap()),
            workspace_root_id: Some(WorkspaceRootId::new("workspace").unwrap()),
            path: "projects/example".to_owned(),
            environment_type: ProjectEnvironmentType::Env,
            snapshot_id: None,
            setup_script: None,
            created_at: TimestampMs(1),
        };

        let value = serde_json::to_value(environment).expect("serialize project environment");
        assert_eq!(value["type"], "env");
        assert_eq!(value["host_name"], "Workstation");
        assert!(value.get("snapshot_id").is_none());
    }

    #[test]
    fn base_sandbox_creation_omits_snapshot_id() {
        let value = serde_json::to_value(CreateSandboxMachineRequest::default())
            .expect("serialize sandbox creation request");

        assert_eq!(value, serde_json::json!({}));
    }

    #[test]
    fn sandbox_template_creation_omits_an_empty_setup_script() {
        let value = serde_json::to_value(CreateSandboxTemplateEnvironmentRequest {
            project_id: Uuid::nil(),
            snapshot_id: Uuid::nil(),
            name: "Development".to_owned(),
            path: "project".to_owned(),
            creation_script: String::new(),
        })
        .expect("serialize sandbox template creation request");

        assert!(value.get("creation_script").is_none());
    }
}

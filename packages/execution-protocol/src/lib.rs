//! Transport-independent envelopes shared by execution gateways, daemons, and clients.

use execution_contracts::*;
use serde::{Deserialize, Serialize};

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

/// A saved working location on a registered machine.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Environment {
    pub environment_id: EnvironmentId,
    pub machine_id: MachineId,
    pub workspace_root_id: WorkspaceRootId,
    pub path: String,
    pub created_at: TimestampMs,
}

/// Creates an immutable saved working location.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateEnvironmentRequest {
    pub machine_id: MachineId,
    pub workspace_root_id: WorkspaceRootId,
    pub path: String,
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
}

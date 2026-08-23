use std::collections::BTreeMap;

use execution_contracts::{
    ApplyMutationRequest, Capability, CommandSpec, EnvironmentDescriptor, EnvironmentId,
    EnvironmentInheritance, EnvironmentVariables, ExecutionId, ExecutionPersistence,
    ExecutionPolicy, MachineId, MutationAtomicity, MutationOperation, MutationPlan,
    MutationPostActions, NetworkMode, OperatingSystem, OperationId, PathConvention, PathSpec,
    ProcessOutputPolicy, ProtocolVersion, ReadMode, ReadRequest, SandboxMode,
    StartExecutionRequest, StdinMode, TextPageRequest, Validate, WorkspaceRoot, WorkspaceRootId,
    parse_json,
};
use schemars::schema_for;
use serde_json::{Value, json};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid test identifier")
}

fn workspace_path(path: &str) -> PathSpec {
    PathSpec::workspace(id::<WorkspaceRootId>("root-1"), path)
}

#[test]
fn environment_manifest_round_trips_and_admits_additive_fields() {
    let descriptor = EnvironmentDescriptor {
        protocol_version: ProtocolVersion::V1,
        machine_id: id::<MachineId>("machine-1"),
        environment_id: id::<EnvironmentId>("local"),
        name: "MacBook workspace".to_owned(),
        operating_system: OperatingSystem::Macos,
        architecture: "aarch64".to_owned(),
        path_convention: PathConvention::Posix,
        default_shell: None,
        workspace_roots: vec![WorkspaceRoot {
            id: id::<WorkspaceRootId>("root-1"),
            name: "agent-pane".to_owned(),
            uri: "file:///workspace/agent-pane".to_owned(),
            read_only: false,
        }],
        capabilities: vec![Capability::v1("workspace.query").expect("valid capability")],
    };

    descriptor.validate().expect("valid descriptor");
    let mut value = serde_json::to_value(&descriptor).expect("serialize descriptor");
    value
        .as_object_mut()
        .expect("descriptor object")
        .insert("future_transport_hint".to_owned(), json!("quic"));

    let decoded: EnvironmentDescriptor =
        serde_json::from_value(value).expect("additive fields remain compatible");
    assert_eq!(decoded, descriptor);

    let schema = serde_json::to_value(schema_for!(EnvironmentDescriptor)).expect("schema");
    assert!(schema["properties"]["capabilities"].is_object());
}

#[test]
fn environment_validation_rejects_invalid_versions_and_duplicates() {
    let duplicate = Capability::v1("workspace.query").expect("valid capability");
    let descriptor = EnvironmentDescriptor {
        protocol_version: ProtocolVersion { major: 0, minor: 1 },
        machine_id: id::<MachineId>("machine-1"),
        environment_id: id::<EnvironmentId>("local"),
        name: "Local".to_owned(),
        operating_system: OperatingSystem::Linux,
        architecture: "x86_64".to_owned(),
        path_convention: PathConvention::Posix,
        default_shell: None,
        workspace_roots: Vec::new(),
        capabilities: vec![duplicate.clone(), duplicate],
    };

    let error = descriptor.validate().expect_err("descriptor is invalid");
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.path.ends_with("protocol_version.major"))
    );
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.message.contains("duplicated"))
    );
}

#[test]
fn workspace_paths_cannot_escape_the_advertised_root() {
    let path = workspace_path("src/../../secrets");
    let error = path.validate().expect_err("traversal must be rejected");
    assert!(error.issues[0].message.contains("traverse"));
}

#[test]
fn validated_json_rejects_ambiguous_read_pagination() {
    let request = ReadRequest {
        path: workspace_path("src/lib.rs"),
        mode: ReadMode::Text,
        page: Some(TextPageRequest {
            start_line: Some(1),
            cursor: Some(id("opaque-next-page")),
            ..TextPageRequest::default()
        }),
    };

    let input = serde_json::to_string(&request).expect("serialize request");
    let error = parse_json::<ReadRequest>(&input).expect_err("pagination is ambiguous");
    assert!(error.to_string().contains("mutually exclusive"));
}

#[test]
fn mutation_apply_round_trips_and_validates_operations() {
    let request = ApplyMutationRequest {
        operation_id: id::<OperationId>("mutation-1"),
        plan: MutationPlan {
            operations: vec![MutationOperation::CreateFile {
                path: workspace_path("src/new.rs"),
                content: execution_contracts::ContentSource::Text {
                    content: "fn main() {}\n".to_owned(),
                },
                create_parents: true,
            }],
            atomicity: MutationAtomicity::AtomicIfSupported,
            post_actions: MutationPostActions::default(),
        },
    };

    request.validate().expect("valid mutation");
    let value = serde_json::to_value(&request).expect("serialize mutation");
    assert_eq!(value["plan"]["operations"][0]["type"], "create_file");
    assert_eq!(
        serde_json::from_value::<ApplyMutationRequest>(value).expect("deserialize mutation"),
        request
    );
}

#[test]
fn execution_request_has_stable_tagged_wire_shape() {
    let request = StartExecutionRequest {
        operation_id: id::<OperationId>("start-1"),
        execution_id: id::<ExecutionId>("exec-1"),
        command: CommandSpec::Argv {
            program: "cargo".to_owned(),
            arguments: vec!["test".to_owned()],
        },
        cwd: workspace_path("."),
        environment: EnvironmentVariables {
            inherit: EnvironmentInheritance::AllowList,
            allow: vec!["PATH".to_owned()],
            remove: Vec::new(),
            set: BTreeMap::from([("CI".to_owned(), "true".to_owned())]),
        },
        stdin: StdinMode::Pty {
            columns: 120,
            rows: 40,
        },
        timeout_ms: Some(60_000),
        persistence: ExecutionPersistence::KeepUntilExit,
        policy: ExecutionPolicy {
            sandbox: SandboxMode::BestEffort,
            network: NetworkMode::Denied,
            profile: None,
            resource_limits: None,
        },
        output: ProcessOutputPolicy {
            persist_full_output: true,
            max_inline_bytes: 64 * 1_024,
            max_chunk_bytes: 16 * 1_024,
        },
    };

    request.validate().expect("valid execution request");
    let value = serde_json::to_value(&request).expect("serialize request");
    assert_eq!(value["command"]["type"], "argv");
    assert_eq!(value["stdin"]["type"], "pty");
    assert_eq!(value["policy"]["network"], "denied");
}

#[test]
fn execution_validation_rejects_zero_sized_pty() {
    let value: Value = json!({
        "operation_id": "start-1",
        "execution_id": "exec-1",
        "command": { "type": "shell", "command": "pwd" },
        "cwd": { "type": "workspace", "root_id": "root-1", "path": "." },
        "stdin": { "type": "pty", "columns": 0, "rows": 40 },
        "persistence": { "type": "kill_on_disconnect" },
        "policy": { "sandbox": "best_effort", "network": "denied" },
        "output": {
            "persist_full_output": true,
            "max_inline_bytes": 1024,
            "max_chunk_bytes": 1024
        }
    });

    let error = execution_contracts::from_json_value::<StartExecutionRequest>(value)
        .expect_err("zero-sized PTY is invalid");
    assert!(error.to_string().contains("PTY columns and rows"));
}

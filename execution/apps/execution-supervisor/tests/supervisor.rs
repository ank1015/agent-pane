use std::{path::PathBuf, process::Stdio, time::Duration};

use execution_core::{
    BinaryData, CommandSpec, EnvironmentVariables, ExecutionHostId, ExecutionId, ExecutionPath,
    ExecutionState, OperationContext, OperationId, ReadExecutionRequest, ReadFileRequest, RootId,
    StartExecutionRequest, StdinMode, WriteCondition, WriteFileRequest,
};
use execution_supervisor::{ServeConfig, call, probe, serve};
use execution_supervisor_core::{SupervisorConfig, SupervisorLimits, SupervisorRoot};
use execution_wire::{
    NdjsonCodec, Operation, OperationResult, RequestEnvelope, RequestId, ResponseEnvelope,
};
use tempfile::TempDir;
use tokio::task::JoinHandle;

struct RunningSupervisor {
    _temporary: TempDir,
    root_id: RootId,
    socket_path: PathBuf,
    state_directory: PathBuf,
    root_directory: PathBuf,
    shutdown: OperationContext,
    task: JoinHandle<anyhow::Result<()>>,
}

impl RunningSupervisor {
    async fn start() -> Self {
        let temporary = tempfile::tempdir().expect("create temporary directory");
        let root_directory = temporary.path().join("workspace");
        tokio::fs::create_dir(&root_directory)
            .await
            .expect("create execution root");
        let socket_directory = temporary.path().join("run");
        let socket_path = socket_directory.join("supervisor.sock");
        let state_directory = temporary.path().join("state");
        let root_id = RootId::new("workspace").expect("valid root ID");
        let shutdown = OperationContext::new();
        let task = tokio::spawn(serve(
            serve_config(
                socket_path.clone(),
                state_directory.clone(),
                root_directory.clone(),
                root_id.clone(),
            ),
            shutdown.clone(),
        ));

        let started = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if probe(&socket_path).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if started.is_err() {
            shutdown.cancel();
            let result = task.await.expect("join failed supervisor start");
            panic!("supervisor did not become ready: {result:?}");
        }

        Self {
            _temporary: temporary,
            root_id,
            socket_path,
            state_directory,
            root_directory,
            shutdown,
            task,
        }
    }

    fn path(&self, path: &str) -> ExecutionPath {
        ExecutionPath::new(self.root_id.clone(), path).expect("valid execution path")
    }

    async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("supervisor shutdown timed out")
            .expect("join supervisor")
            .expect("clean supervisor shutdown");
    }
}

fn serve_config(
    socket_path: PathBuf,
    state_directory: PathBuf,
    root_directory: PathBuf,
    root_id: RootId,
) -> ServeConfig {
    ServeConfig {
        socket_path,
        supervisor: SupervisorConfig {
            host_id: ExecutionHostId::generate(),
            state_directory,
            roots: vec![SupervisorRoot {
                id: root_id,
                name: "Workspace".to_string(),
                path: root_directory,
                read_only: false,
            }],
            limits: SupervisorLimits::default(),
        },
    }
}

fn request(operation: Operation) -> RequestEnvelope {
    RequestEnvelope::new(RequestId::generate(), operation)
}

async fn successful_call(socket_path: &std::path::Path, operation: Operation) -> OperationResult {
    match call(socket_path, &request(operation))
        .await
        .expect("call supervisor")
    {
        ResponseEnvelope::Success { result, .. } => result,
        ResponseEnvelope::Error { error, .. } => panic!("operation failed: {error}"),
    }
}

#[tokio::test]
async fn socket_endpoint_dispatches_filesystem_and_process_operations() {
    let supervisor = RunningSupervisor::start().await;
    let descriptor = probe(&supervisor.socket_path)
        .await
        .expect("probe supervisor");

    let result = successful_call(
        &supervisor.socket_path,
        Operation::FilesystemWrite(WriteFileRequest {
            expected_generation: None,
            strategy: execution_core::WriteStrategy::AtomicReplace,
            operation_id: OperationId::generate(),
            path: supervisor.path("message.txt"),
            data: BinaryData::new(b"hello".to_vec()),
            condition: WriteCondition::MustNotExist,
            create_parents: true,
            follow_symlinks: true,
        }),
    )
    .await;
    assert!(matches!(result, OperationResult::WriteFile(_)));

    let result = successful_call(
        &supervisor.socket_path,
        Operation::FilesystemRead(ReadFileRequest {
            path: supervisor.path("message.txt"),
            offset: 0,
            max_bytes: 1024,
            follow_symlinks: true,
        }),
    )
    .await;
    let OperationResult::ReadFile(read) = result else {
        panic!("expected read-file result");
    };
    assert_eq!(read.data.as_slice(), b"hello");

    let execution_id = ExecutionId::generate();
    let result = successful_call(
        &supervisor.socket_path,
        Operation::ProcessStart(StartExecutionRequest {
            expected_generation: None,
            output_drain_timeout_ms: None,
            operation_id: OperationId::generate(),
            execution_id: execution_id.clone(),
            command: CommandSpec::Shell {
                command: "printf 'supervised'".to_string(),
                shell: Some("/bin/sh".to_string()),
                login: false,
            },
            cwd: ExecutionPath::root(supervisor.root_id.clone()),
            environment: EnvironmentVariables::default(),
            stdin: StdinMode::Closed,
            timeout_ms: None,
        }),
    )
    .await;
    assert!(matches!(result, OperationResult::ExecutionHandle(_)));

    let mut after_sequence = 0;
    let mut output = Vec::new();
    let terminal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let result = successful_call(
                &supervisor.socket_path,
                Operation::ProcessRead(ReadExecutionRequest {
                    execution_id: execution_id.clone(),
                    supervisor_generation_id: descriptor.supervisor_generation_id.clone(),
                    after_sequence,
                    max_bytes: 1024,
                    wait_ms: Some(500),
                }),
            )
            .await;
            let OperationResult::ReadExecution(read) = result else {
                panic!("expected process-read result");
            };
            after_sequence = read.next_sequence;
            for event in read.events {
                if let execution_core::ProcessEventKind::Output { data, .. } = event.event {
                    output.extend_from_slice(data.as_slice());
                }
            }
            if matches!(
                read.state,
                ExecutionState::Exited
                    | ExecutionState::Failed
                    | ExecutionState::Cancelled
                    | ExecutionState::Lost
            ) {
                break read.state;
            }
        }
    })
    .await
    .expect("process did not finish");
    assert_eq!(terminal, ExecutionState::Exited);
    assert_eq!(output, b"supervised");

    supervisor.stop().await;
}

#[tokio::test]
async fn state_and_socket_locks_reject_competing_supervisors() {
    let supervisor = RunningSupervisor::start().await;
    let second_shutdown = OperationContext::new();
    let second_socket = supervisor
        .socket_path
        .parent()
        .expect("socket parent")
        .join("second.sock");
    let error = serve(
        serve_config(
            second_socket,
            supervisor.state_directory.clone(),
            supervisor.root_directory.clone(),
            supervisor.root_id.clone(),
        ),
        second_shutdown,
    )
    .await
    .expect_err("second supervisor must not share the state directory");
    assert!(error.to_string().contains("another supervisor"));
    supervisor.stop().await;
}

#[tokio::test]
async fn rpc_and_health_commands_keep_stdout_protocol_safe() {
    let supervisor = RunningSupervisor::start().await;
    let executable = env!("CARGO_BIN_EXE_execution-supervisor");
    let describe = request(Operation::Describe);
    let mut child = tokio::process::Command::new(executable)
        .arg("rpc")
        .arg("--socket")
        .arg(&supervisor.socket_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start rpc command");
    let mut input = child.stdin.take().expect("rpc stdin");
    let codec = NdjsonCodec::default();
    codec
        .write_request(&mut input, &describe)
        .await
        .expect("write rpc request");
    drop(input);
    let output = child.wait_with_output().await.expect("wait for rpc");
    assert!(output.status.success(), "rpc stderr: {:?}", output.stderr);
    let response: ResponseEnvelope =
        serde_json::from_slice(&output.stdout).expect("parse rpc stdout");
    assert_eq!(response.request_id(), &describe.request_id);
    assert!(matches!(
        response,
        ResponseEnvelope::Success {
            result: OperationResult::HostDescriptor(_),
            ..
        }
    ));

    let output = tokio::process::Command::new(executable)
        .arg("health")
        .arg("--socket")
        .arg(&supervisor.socket_path)
        .output()
        .await
        .expect("run health command");
    assert!(
        output.status.success(),
        "health stderr: {:?}",
        output.stderr
    );
    let health: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse health output");
    assert_eq!(health["status"], "ok");
    assert_eq!(
        health["descriptor"]["host_id"],
        descriptor_host(&supervisor).await
    );

    let output = tokio::process::Command::new(executable)
        .arg("version")
        .output()
        .await
        .expect("run version command");
    assert!(output.status.success());
    let version: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse version output");
    assert_eq!(version["program"], "execution-supervisor");
    assert_eq!(version["protocol"], execution_wire::PROTOCOL_NAME);
    assert_eq!(
        version["protocol_version"],
        execution_wire::PROTOCOL_VERSION
    );

    supervisor.stop().await;
}

async fn descriptor_host(supervisor: &RunningSupervisor) -> String {
    probe(&supervisor.socket_path)
        .await
        .expect("probe supervisor")
        .host_id
        .into_inner()
}

#[cfg(unix)]
#[tokio::test]
async fn socket_start_checks_generation_before_spawning_and_carries_drain_options() {
    let supervisor = RunningSupervisor::start().await;
    let descriptor = probe(&supervisor.socket_path).await.unwrap();
    let mut start = StartExecutionRequest {
        expected_generation: Some(execution_core::SupervisorGenerationId::generate()),
        output_drain_timeout_ms: Some(50),
        operation_id: OperationId::generate(),
        execution_id: ExecutionId::generate(),
        command: CommandSpec::ShellScript {
            command: "printf once > fenced-start; sleep 2 & exit 7".into(),
            shell: Some("/bin/sh".into()),
            login: false,
        },
        cwd: ExecutionPath::root(supervisor.root_id.clone()),
        environment: EnvironmentVariables::default(),
        stdin: StdinMode::Closed,
        timeout_ms: None,
    };
    let response = call(
        &supervisor.socket_path,
        &request(Operation::ProcessStart(start.clone())),
    )
    .await
    .unwrap();
    assert!(
        matches!(response, ResponseEnvelope::Error { error, .. } if error.code == execution_core::ExecutionErrorCode::ExecutionLost)
    );
    assert!(!supervisor.root_directory.join("fenced-start").exists());
    start.expected_generation = Some(descriptor.supervisor_generation_id.clone());
    let began = std::time::Instant::now();
    let OperationResult::ExecutionHandle(handle) =
        successful_call(&supervisor.socket_path, Operation::ProcessStart(start)).await
    else {
        panic!("start handle")
    };
    let mut cursor = 0;
    loop {
        let OperationResult::ReadExecution(page) = successful_call(
            &supervisor.socket_path,
            Operation::ProcessRead(ReadExecutionRequest {
                execution_id: handle.execution_id.clone(),
                supervisor_generation_id: handle.supervisor_generation_id.clone(),
                after_sequence: cursor,
                max_bytes: 1024,
                wait_ms: Some(100),
            }),
        )
        .await
        else {
            panic!("read page")
        };
        cursor = page.next_sequence;
        if page
            .events
            .iter()
            .any(|event| matches!(event.event, execution_core::ProcessEventKind::Closed))
        {
            assert_eq!(page.exit_code, Some(7));
            break;
        }
        assert!(began.elapsed() < Duration::from_secs(1));
    }
    assert!(began.elapsed() < Duration::from_secs(1));
    assert_eq!(
        std::fs::read_to_string(supervisor.root_directory.join("fenced-start")).unwrap(),
        "once"
    );
    supervisor.stop().await;
}

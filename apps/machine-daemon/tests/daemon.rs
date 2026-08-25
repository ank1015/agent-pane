use std::sync::Arc;

use execution_contracts::{MachineId, WorkspaceRootId};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_runtime::ExecutionRuntime;
use machine_daemon::{
    config::DaemonConfig,
    dispatch::Dispatcher,
    doctor, identity,
    protocol::{ClientMessage, Operation, Response, ServerMessage},
    session,
};
use tempfile::TempDir;
use tokio::sync::mpsc;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid test identifier")
}

async fn runtime() -> (TempDir, Arc<LocalExecutionRuntime>) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let workspace = directory.path().join("workspace");
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("create workspace");
    let runtime = LocalExecutionRuntime::new(LocalRuntimeConfig {
        machine_id: id::<MachineId>("machine-1"),
        name: "Daemon fixture".to_owned(),
        state_directory: directory.path().join("state"),
        workspace_roots: vec![LocalWorkspaceRoot {
            id: id::<WorkspaceRootId>("root-1"),
            name: "fixture".to_owned(),
            path: workspace,
            read_only: false,
        }],
        native_grants: Vec::new(),
    })
    .await
    .expect("local runtime");
    (directory, Arc::new(runtime))
}

#[tokio::test]
async fn generated_identity_is_stable() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let first = identity::load_or_create(directory.path(), None)
        .await
        .expect("first identity");
    let second = identity::load_or_create(directory.path(), None)
        .await
        .expect("second identity");
    assert_eq!(first, second);
}

#[tokio::test]
async fn session_announces_descriptor_and_dispatches_requests() {
    let (_directory, runtime) = runtime().await;
    let descriptor = runtime.descriptor().clone();
    let dispatcher = Dispatcher::new(runtime);
    let (incoming_tx, incoming_rx) = mpsc::channel(4);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(4);
    let task = tokio::spawn(session::run(
        descriptor.clone(),
        dispatcher,
        incoming_rx,
        outgoing_tx,
    ));

    let ready = outgoing_rx.recv().await.expect("ready message");
    assert!(matches!(
        ready,
        ServerMessage::Ready {
            descriptor: value,
            ..
        } if value == descriptor
    ));
    incoming_tx
        .send(ClientMessage::Request {
            request_id: "describe-1".to_owned(),
            operation: Box::new(Operation::Describe),
        })
        .await
        .expect("send request");
    let response = outgoing_rx.recv().await.expect("describe response");
    assert!(matches!(
        response,
        ServerMessage::Response {
            request_id,
            response: Response::MachineDescriptor(value),
        } if request_id == "describe-1" && value == descriptor
    ));
    drop(incoming_tx);
    task.await.expect("session task");
}

#[tokio::test]
async fn config_paths_are_resolved_relative_to_the_config_file() {
    let directory = tempfile::tempdir().expect("temporary directory");
    tokio::fs::create_dir_all(directory.path().join("workspace"))
        .await
        .expect("create workspace");
    let config_path = directory.path().join("machine-daemon.json");
    tokio::fs::write(
        &config_path,
        br#"{
            "name": "Fixture",
            "state_directory": "state",
            "workspace_roots": [
                {"id": "root-1", "name": "Fixture", "path": "workspace"}
            ]
        }"#,
    )
    .await
    .expect("write config");

    let config = DaemonConfig::load(&config_path).await.expect("load config");
    assert_eq!(config.state_directory, directory.path().join("state"));
    assert_eq!(
        config.workspace_roots[0].path,
        directory.path().join("workspace")
    );
    assert_eq!(config.auth.token_env, "MACHINE_DAEMON_TOKEN");
}

#[tokio::test]
async fn doctor_verifies_the_real_local_runtime() {
    let (directory, runtime) = runtime().await;
    let report = doctor::run(runtime, &directory.path().join("state"), None).await;
    assert!(report.healthy, "doctor checks: {:?}", report.checks);
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "process_runtime" && check.ok)
    );
}

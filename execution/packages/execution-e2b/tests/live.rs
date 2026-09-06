use std::time::Duration;

use execution_core::{
    BinaryData, CommandSpec, EnvironmentVariables, ExecutionHostId, ExecutionId, ExecutionPath,
    ExecutionRuntime, ExecutionState, OperationContext, OperationId, ProcessEventKind,
    ReadExecutionRequest, ReadFileRequest, StartExecutionRequest, StdinMode, WriteCondition,
    WriteFileRequest,
};
use execution_e2b::{
    E2bConfig, E2bControlClient, E2bEnvdClient, E2bExecutionRuntime, E2bResult, E2bRuntimeConfig,
    EnvdProcessRequest, SupervisorConfig,
};

/// Creates real E2B resources. Run explicitly with:
/// `cargo test -p execution-e2b --test live -- --ignored --nocapture`
#[tokio::test]
#[ignore = "requires E2B_API_KEY and creates temporary E2B resources"]
async fn real_e2b_lifecycle_and_envd_smoke_test() -> E2bResult<()> {
    let mut config = E2bConfig::from_env()?;
    config.request_timeout = Duration::from_secs(180);
    config.sandbox_timeout_seconds = 300;
    let control = E2bControlClient::new(config.clone())?;

    let base_id = control.create_base_sandbox(None, None).await?;
    let mut clone_id = None;
    let mut snapshot_id = None;

    let result: E2bResult<()> = async {
        let connection = control.ensure_connected(&base_id).await?;
        let envd = E2bEnvdClient::new(&config, &connection)?;
        envd.health().await?;
        envd.upload_file(
            "/home/user/execution-e2b-live-smoke.txt",
            b"snapshot-content".to_vec(),
        )
        .await?;

        let mut python = EnvdProcessRequest::new("python3");
        python.arguments = vec![
            "-c".to_owned(),
            "print(open('/home/user/execution-e2b-live-smoke.txt').read())".to_owned(),
        ];
        let output = envd.run_process(python).await?;
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, b"snapshot-content\n");

        control.pause_sandbox(&base_id).await?;
        let paused = control.get_sandbox(&base_id).await?;
        assert_eq!(paused.state, execution_e2b::SandboxState::Paused);
        let resumed_connection = control.ensure_connected(&base_id).await?;
        let resumed = control.get_sandbox(&base_id).await?;
        assert_eq!(resumed.state, execution_e2b::SandboxState::Running);
        E2bEnvdClient::new(&config, &resumed_connection)?
            .health()
            .await?;

        verify_supervisor_runtime(&config, &base_id).await?;

        let created_snapshot = control.snapshot_sandbox(&base_id).await?;
        snapshot_id = Some(created_snapshot.clone());
        let created_clone = control
            .create_snapshot_sandbox(&created_snapshot, None)
            .await?;
        clone_id = Some(created_clone.clone());

        let clone_connection = control.ensure_connected(&created_clone).await?;
        let clone_envd = E2bEnvdClient::new(&config, &clone_connection)?;
        clone_envd.health().await?;
        let mut verify = EnvdProcessRequest::new("python3");
        verify.arguments = vec![
            "-c".to_owned(),
            "print(open('/home/user/execution-e2b-live-smoke.txt').read())".to_owned(),
        ];
        let output = clone_envd.run_process(verify).await?;
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, b"snapshot-content\n");
        Ok(())
    }
    .await;

    let clone_cleanup = if let Some(id) = clone_id {
        control.delete_sandbox(&id).await
    } else {
        Ok(())
    };
    let base_cleanup = control.delete_sandbox(&base_id).await;
    let snapshot_cleanup = if let Some(id) = snapshot_id {
        control.delete_snapshot(&id).await
    } else {
        Ok(())
    };

    result?;
    clone_cleanup?;
    base_cleanup?;
    snapshot_cleanup?;
    Ok(())
}

async fn verify_supervisor_runtime(e2b: &E2bConfig, sandbox_id: &str) -> E2bResult<()> {
    let runtime = E2bExecutionRuntime::connect(E2bRuntimeConfig {
        e2b: e2b.clone(),
        sandbox_id: sandbox_id.to_owned(),
        host_id: ExecutionHostId::new("live-e2b-host").unwrap(),
        supervisor: SupervisorConfig::default(),
    })
    .await?;
    let context = OperationContext::with_timeout(Duration::from_secs(60));
    let root_id = runtime.descriptor().roots[0].id.clone();
    let path = ExecutionPath::new(root_id.clone(), "runtime-smoke.txt").unwrap();

    runtime
        .filesystem()
        .write(
            &context,
            WriteFileRequest {
                expected_generation: None,
                strategy: execution_core::WriteStrategy::AtomicReplace,
                operation_id: OperationId::generate(),
                path: path.clone(),
                data: BinaryData::new(b"runtime-content".to_vec()),
                condition: WriteCondition::Any,
                create_parents: true,
                follow_symlinks: true,
            },
        )
        .await
        .map_err(execution_e2b::E2bError::from)?;
    let read = runtime
        .filesystem()
        .read(
            &context,
            ReadFileRequest {
                path,
                offset: 0,
                max_bytes: 1024,
                follow_symlinks: true,
            },
        )
        .await
        .map_err(execution_e2b::E2bError::from)?;
    assert_eq!(read.data.as_slice(), b"runtime-content");

    let execution_id = ExecutionId::generate();
    runtime
        .processes()
        .start(
            &context,
            StartExecutionRequest {
                expected_generation: None,
                output_drain_timeout_ms: None,
                operation_id: OperationId::generate(),
                execution_id: execution_id.clone(),
                command: CommandSpec::Shell {
                    command: "printf 'runtime-process'".to_owned(),
                    shell: Some("/bin/sh".to_owned()),
                    login: false,
                },
                cwd: ExecutionPath::root(root_id),
                environment: EnvironmentVariables::default(),
                stdin: StdinMode::Closed,
                timeout_ms: Some(10_000),
            },
        )
        .await
        .map_err(execution_e2b::E2bError::from)?;

    let mut after_sequence = 0;
    let mut bytes = Vec::new();
    loop {
        let read = runtime
            .processes()
            .read(
                &context,
                ReadExecutionRequest {
                    execution_id: execution_id.clone(),
                    supervisor_generation_id: runtime.descriptor().supervisor_generation_id.clone(),
                    after_sequence,
                    max_bytes: 1024,
                    wait_ms: Some(1_000),
                },
            )
            .await
            .map_err(execution_e2b::E2bError::from)?;
        after_sequence = read.next_sequence;
        for event in read.events {
            if let ProcessEventKind::Output { data, .. } = event.event {
                bytes.extend_from_slice(data.as_slice());
            }
        }
        if matches!(
            read.state,
            ExecutionState::Exited
                | ExecutionState::Failed
                | ExecutionState::Cancelled
                | ExecutionState::Lost
        ) {
            assert_eq!(read.state, ExecutionState::Exited);
            break;
        }
    }
    assert_eq!(bytes, b"runtime-process");
    Ok(())
}

use std::{path::Path, sync::Arc, time::Duration};

use execution_contracts::{
    AttachExecutionRequest, CommandSpec, EnvironmentInheritance, EnvironmentVariables, ExecutionId,
    ExecutionPersistence, ExecutionPolicy, InspectRequest, NetworkMode, OperationId, PathSpec,
    ProcessEventKind, ProcessOutputPolicy, SandboxMode, StartExecutionRequest, StdinMode,
};
use execution_local::LocalExecutionRuntime;
use execution_runtime::{ExecutionRuntime, OperationContext};
use futures_util::StreamExt;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub message: String,
}

pub async fn run(
    runtime: Arc<LocalExecutionRuntime>,
    state_directory: &Path,
    gateway: Option<&str>,
) -> DoctorReport {
    let mut checks = Vec::new();
    checks.push(match runtime.validate_capabilities() {
        Ok(()) => passed("capabilities", "descriptor and runtime capabilities agree"),
        Err(source) => failed("capabilities", source.to_string()),
    });

    for root in &runtime.descriptor().workspace_roots {
        let result = runtime
            .workspace_query()
            .inspect(
                &OperationContext::new(),
                InspectRequest {
                    path: PathSpec::workspace(root.id.clone(), "."),
                    follow_symlinks: true,
                },
            )
            .await;
        checks.push(match result {
            Ok(_) => passed(
                format!("workspace_root:{}", root.id),
                format!("{} is accessible", root.uri),
            ),
            Err(source) => failed(format!("workspace_root:{}", root.id), source.to_string()),
        });
    }

    let probe = state_directory.join(format!("doctor-{}.tmp", Uuid::now_v7()));
    checks.push(
        match tokio::fs::write(&probe, b"machine-daemon-doctor").await {
            Ok(()) => {
                let _ = tokio::fs::remove_file(&probe).await;
                passed("state_directory", "state directory is writable")
            }
            Err(source) => failed("state_directory", source.to_string()),
        },
    );

    checks.push(process_check(runtime.as_ref()).await);
    if let Some(gateway) = gateway {
        checks.push(match url::Url::parse(gateway) {
            Ok(url) if matches!(url.scheme(), "ws" | "wss") => {
                passed("gateway_url", "gateway WebSocket URL is valid")
            }
            Ok(_) => failed("gateway_url", "gateway URL must use ws:// or wss://"),
            Err(source) => failed("gateway_url", source.to_string()),
        });
    }

    DoctorReport {
        healthy: checks.iter().all(|check| check.ok),
        checks,
    }
}

async fn process_check(runtime: &LocalExecutionRuntime) -> DoctorCheck {
    let Some(root) = runtime.descriptor().workspace_roots.first() else {
        return failed("process_runtime", "no workspace root is configured");
    };
    let Some(process_runtime) = runtime.process_runtime() else {
        return failed("process_runtime", "process capability is missing");
    };
    let execution_id =
        ExecutionId::new(Uuid::now_v7().to_string()).expect("UUID execution identifier is valid");
    let start = process_runtime
        .start(
            &OperationContext::new(),
            StartExecutionRequest {
                operation_id: OperationId::new(Uuid::now_v7().to_string())
                    .expect("UUID operation identifier is valid"),
                execution_id: execution_id.clone(),
                command: CommandSpec::Shell {
                    command: doctor_command().to_owned(),
                    shell: None,
                    login: false,
                },
                cwd: PathSpec::workspace(root.id.clone(), "."),
                environment: EnvironmentVariables {
                    inherit: EnvironmentInheritance::All,
                    allow: Vec::new(),
                    remove: Vec::new(),
                    set: Default::default(),
                },
                stdin: StdinMode::Closed,
                timeout_ms: Some(5_000),
                persistence: ExecutionPersistence::KeepUntilExit,
                policy: ExecutionPolicy {
                    sandbox: SandboxMode::Disabled,
                    network: NetworkMode::Inherit,
                    profile: None,
                    resource_limits: None,
                },
                output: ProcessOutputPolicy {
                    persist_full_output: false,
                    max_inline_bytes: 1024,
                    max_chunk_bytes: 1024,
                },
            },
        )
        .await;
    if let Err(source) = start {
        return failed("process_runtime", source.to_string());
    }
    let stream = process_runtime
        .attach(
            &OperationContext::new(),
            AttachExecutionRequest {
                execution_id,
                after_sequence: None,
            },
        )
        .await;
    let Ok(mut stream) = stream else {
        return failed("process_runtime", "could not attach to doctor process");
    };
    let result = tokio::time::timeout(Duration::from_secs(6), async {
        while let Some(event) = stream.next().await {
            match event?.event {
                ProcessEventKind::Exited { exit_code: 0, .. } => {
                    return Ok::<bool, execution_contracts::ExecutionError>(true);
                }
                ProcessEventKind::Exited { .. } | ProcessEventKind::Failed { .. } => {
                    return Ok(false);
                }
                ProcessEventKind::Started
                | ProcessEventKind::Output { .. }
                | ProcessEventKind::Closed => {}
            }
        }
        Ok(false)
    })
    .await;
    match result {
        Ok(Ok(true)) => passed("process_runtime", "shell process execution succeeded"),
        Ok(Ok(false)) => failed("process_runtime", "doctor process exited unsuccessfully"),
        Ok(Err(source)) => failed("process_runtime", source.to_string()),
        Err(_) => failed("process_runtime", "doctor process timed out"),
    }
}

#[cfg(unix)]
const fn doctor_command() -> &'static str {
    "exit 0"
}

#[cfg(windows)]
const fn doctor_command() -> &'static str {
    "exit /B 0"
}

fn passed(name: impl Into<String>, message: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        ok: true,
        message: message.into(),
    }
}

fn failed(name: impl Into<String>, message: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        ok: false,
        message: message.into(),
    }
}

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use execution_contracts::{
    ExecutionId, ExecutionState, InspectExecutionRequest, MachineId, TerminateExecutionRequest,
    WorkspaceRootId,
};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_runtime::{ExecutionRuntime, OperationContext};
use llm_contracts::{ContentPart, ToolArguments, Validate as _};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tool_codex_unified_exec::{
    CodexExecOptions, CodexExecSession, CodexExecSessionStore, CodexExecSessionStoreError,
    CodexExecToolContext, ExecCommandArguments, WriteStdinArguments, definitions,
    execute_exec_command, execute_exec_command_tool, execute_write_stdin,
};

#[derive(Default)]
struct HarnessSessionStore {
    sessions: Mutex<HashMap<i32, Arc<CodexExecSession>>>,
    next_id: Mutex<i32>,
}

#[async_trait]
impl CodexExecSessionStore for HarnessSessionStore {
    async fn insert(
        &self,
        execution_id: ExecutionId,
        tty: bool,
    ) -> Result<Arc<CodexExecSession>, CodexExecSessionStoreError> {
        let mut next_id = self.next_id.lock().await;
        let session_id = if *next_id == 0 { 1_000 } else { *next_id };
        *next_id = session_id + 1;
        drop(next_id);
        let session = Arc::new(CodexExecSession::new(session_id, execution_id, tty)?);
        self.sessions
            .lock()
            .await
            .insert(session_id, Arc::clone(&session));
        Ok(session)
    }

    async fn get(
        &self,
        session_id: i32,
    ) -> Result<Option<Arc<CodexExecSession>>, CodexExecSessionStoreError> {
        Ok(self.sessions.lock().await.get(&session_id).cloned())
    }

    async fn update_last_sequence(
        &self,
        _session_id: i32,
        _execution_id: &ExecutionId,
        _last_sequence: u64,
    ) -> Result<(), CodexExecSessionStoreError> {
        Ok(())
    }

    async fn remove(
        &self,
        session_id: i32,
        execution_id: &ExecutionId,
    ) -> Result<(), CodexExecSessionStoreError> {
        let mut sessions = self.sessions.lock().await;
        if sessions
            .get(&session_id)
            .is_some_and(|session| session.execution_id() == execution_id)
        {
            sessions.remove(&session_id);
        }
        Ok(())
    }
}

struct Fixture {
    _directory: TempDir,
    workspace: std::path::PathBuf,
    runtime: LocalExecutionRuntime,
    root_id: WorkspaceRootId,
    sessions: HarnessSessionStore,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        tokio::fs::create_dir_all(workspace.join("project/subdir"))
            .await
            .expect("create workspace");
        let workspace = tokio::fs::canonicalize(workspace)
            .await
            .expect("canonical workspace path");
        let root_id = id::<WorkspaceRootId>("root");
        let runtime = LocalExecutionRuntime::new(LocalRuntimeConfig {
            machine_id: id::<MachineId>("machine"),
            name: "Codex unified exec tests".to_owned(),
            state_directory: directory.path().join("state"),
            workspace_roots: vec![LocalWorkspaceRoot {
                id: root_id.clone(),
                name: "workspace".to_owned(),
                path: workspace.clone(),
                read_only: false,
            }],
            native_grants: Vec::new(),
        })
        .await
        .expect("local execution runtime");
        Self {
            _directory: directory,
            workspace,
            runtime,
            root_id,
            sessions: HarnessSessionStore::default(),
        }
    }

    fn context<'a>(
        &'a self,
        operation: &'a OperationContext,
        call_id: &str,
    ) -> CodexExecToolContext<'a> {
        CodexExecToolContext::new(
            &self.runtime,
            operation,
            &self.sessions,
            self.root_id.clone(),
            "project",
            call_id,
        )
        .expect("unified exec context")
        .with_options(CodexExecOptions {
            allow_login_shell: false,
            ..CodexExecOptions::default()
        })
    }
}

#[test]
fn exports_two_valid_codex_definitions() {
    let definitions = definitions();
    assert_eq!(definitions.len(), 2);
    assert_eq!(definitions[0].name(), "exec_command");
    assert_eq!(definitions[1].name(), "write_stdin");
    for definition in &definitions {
        let llm_contracts::ToolDefinition::Function(tool) = definition else {
            panic!("unified exec tools must be function tools")
        };
        assert_eq!(tool.strict, Some(false));
        assert!(
            !tool.parameters["properties"]
                .as_object()
                .unwrap()
                .contains_key("sandbox_permissions")
        );
    }
    for definition in definitions {
        definition.validate().expect("valid tool definition");
    }
}

#[test]
fn exports_the_codex_unified_exec_output_schema() {
    let definitions = definitions();
    for definition in definitions {
        let llm_contracts::ToolDefinition::Function(tool) = definition else {
            panic!("unified exec tools must be function tools")
        };
        assert_eq!(
            Value::Object(tool.output_schema.expect("unified exec output schema")),
            json!({
                "type": "object",
                "properties": {
                    "chunk_id": {
                        "type": "string",
                        "description": "Chunk identifier included when the response reports one."
                    },
                    "wall_time_seconds": {
                        "type": "number",
                        "description": "Elapsed wall time spent waiting for output in seconds."
                    },
                    "exit_code": {
                        "type": "number",
                        "description": "Process exit code when the command finished during this call."
                    },
                    "session_id": {
                        "type": "number",
                        "description": "Session identifier to pass to write_stdin when the process is still running."
                    },
                    "original_token_count": {
                        "type": "number",
                        "description": "Approximate token count before output truncation."
                    },
                    "output": {
                        "type": "string",
                        "description": "Command output text, possibly truncated."
                    }
                },
                "required": ["wall_time_seconds", "output"],
                "additionalProperties": false
            })
        );
    }
}

#[tokio::test]
async fn completed_nonzero_command_is_a_successful_tool_output() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let output = execute_exec_command(
        ExecCommandArguments {
            cmd: "printf stdout; printf stderr >&2; exit 7".to_owned(),
            workdir: None,
            tty: false,
            yield_time_ms: 10_000,
            max_output_tokens: None,
            shell: None,
            login: Some(false),
        },
        &fixture.context(&operation, "short-command"),
    )
    .await
    .expect("nonzero exits are returned, not rejected");

    let content = text(&output.content);
    assert!(content.contains("Process exited with code 7"));
    assert!(content.contains("stdout"));
    assert!(content.contains("stderr"));
    let details = output.details.as_ref().unwrap();
    assert_eq!(details["exit_code"], 7);
    assert!(details.get("session_id").is_none());
}

#[tokio::test]
async fn running_command_returns_session_and_poll_does_not_replay_output() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let first = execute_exec_command(
        ExecCommandArguments {
            cmd: "printf first; sleep 0.5; printf second".to_owned(),
            workdir: None,
            tty: false,
            yield_time_ms: 250,
            max_output_tokens: None,
            shell: None,
            login: Some(false),
        },
        &fixture.context(&operation, "background-command"),
    )
    .await
    .expect("initial command yield");
    let session_id = first.details.as_ref().unwrap()["session_id"]
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .expect("running session ID");
    assert_eq!(first.details.as_ref().unwrap()["output"], "first");

    let second = execute_write_stdin(
        WriteStdinArguments {
            session_id,
            chars: String::new(),
            yield_time_ms: 5_000,
            max_output_tokens: None,
        },
        &fixture.context(&operation, "background-poll"),
    )
    .await
    .expect("final poll");
    assert_eq!(second.details.as_ref().unwrap()["output"], "second");
    assert_eq!(second.details.as_ref().unwrap()["exit_code"], 0);
    assert!(second.details.as_ref().unwrap().get("session_id").is_none());
    assert!(
        fixture
            .sessions
            .get(session_id)
            .await
            .expect("session lookup")
            .is_none()
    );
}

#[tokio::test]
async fn cancelling_initial_collection_preserves_the_started_background_process() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let context = fixture.context(&operation, "cancelled-command");
    let execution = execute_exec_command(
        ExecCommandArguments {
            cmd: "sleep 30".to_owned(),
            workdir: None,
            tty: false,
            yield_time_ms: 30_000,
            max_output_tokens: None,
            shell: None,
            login: Some(false),
        },
        &context,
    );
    tokio::pin!(execution);
    tokio::select! {
        () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
            operation.cancel();
        }
        result = &mut execution => panic!("command unexpectedly completed: {result:?}"),
    }
    let error = execution.await.expect_err("cancelled command");
    assert_eq!(error.name(), "cancelled");
    let session = fixture
        .sessions
        .sessions
        .lock()
        .await
        .values()
        .next()
        .cloned()
        .expect("started process must remain in the session store");
    let cleanup = OperationContext::new();
    let runtime = fixture
        .runtime
        .process_runtime()
        .expect("local process runtime");
    let status = runtime
        .inspect(
            &cleanup,
            InspectExecutionRequest {
                execution_id: session.execution_id().clone(),
            },
        )
        .await
        .expect("inspect preserved process");
    assert!(matches!(
        status.state,
        ExecutionState::Queued | ExecutionState::Starting | ExecutionState::Running
    ));
    runtime
        .terminate(
            &cleanup,
            TerminateExecutionRequest {
                execution_id: session.execution_id().clone(),
            },
        )
        .await
        .expect("clean up preserved test process");
}

#[tokio::test]
async fn tty_session_accepts_input_and_finishes() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let first = execute_exec_command(
        ExecCommandArguments {
            cmd: "read line; printf 'got:%s\\n' \"$line\"".to_owned(),
            workdir: None,
            tty: true,
            yield_time_ms: 250,
            max_output_tokens: None,
            shell: None,
            login: Some(false),
        },
        &fixture.context(&operation, "tty-command"),
    )
    .await
    .expect("interactive command");
    let session_id = first.details.as_ref().unwrap()["session_id"]
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .expect("TTY session ID");

    let output = execute_write_stdin(
        WriteStdinArguments {
            session_id,
            chars: "hello\n".to_owned(),
            yield_time_ms: 5_000,
            max_output_tokens: None,
        },
        &fixture.context(&operation, "tty-input"),
    )
    .await
    .expect("TTY input");
    let text = output.details.as_ref().unwrap()["output"]
        .as_str()
        .expect("output string");
    assert!(text.contains("got:hello"));
    assert_eq!(output.details.as_ref().unwrap()["exit_code"], 0);
}

#[tokio::test]
async fn write_after_process_exit_returns_final_output_instead_of_stdin_closed() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let first = execute_exec_command(
        ExecCommandArguments {
            cmd: "sleep 0.35; printf final".to_owned(),
            workdir: None,
            tty: false,
            yield_time_ms: 250,
            max_output_tokens: None,
            shell: None,
            login: Some(false),
        },
        &fixture.context(&operation, "late-write-command"),
    )
    .await
    .expect("background command");
    let session_id = first.details.as_ref().unwrap()["session_id"]
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .expect("running session ID");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let output = execute_write_stdin(
        WriteStdinArguments {
            session_id,
            chars: "too late".to_owned(),
            yield_time_ms: 250,
            max_output_tokens: None,
        },
        &fixture.context(&operation, "late-write"),
    )
    .await
    .expect("final output remains observable");
    assert_eq!(output.details.as_ref().unwrap()["output"], "final");
    assert_eq!(output.details.as_ref().unwrap()["exit_code"], 0);
}

#[tokio::test]
async fn function_arguments_and_relative_workdir_are_supported() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let Value::Object(arguments) = serde_json::json!({
        "cmd": "pwd",
        "workdir": "subdir",
        "login": false
    }) else {
        unreachable!()
    };
    let output = execute_exec_command_tool(
        &ToolArguments::Object(arguments),
        &fixture.context(&operation, "workdir-command"),
    )
    .await
    .expect("function tool execution");
    let expected = fixture.workspace.join("project/subdir");
    assert!(
        output.details.as_ref().unwrap()["output"]
            .as_str()
            .unwrap()
            .contains(expected.to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn unknown_session_is_reported_without_touching_the_runtime() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let error = execute_write_stdin(
        WriteStdinArguments {
            session_id: 999_999,
            chars: String::new(),
            yield_time_ms: 250,
            max_output_tokens: None,
        },
        &fixture.context(&operation, "unknown-session"),
    )
    .await
    .expect_err("unknown session must fail");
    assert_eq!(error.name(), "process_error");
    assert!(error.message().contains("unknown session ID"));
}

fn text(content: &[ContentPart]) -> &str {
    let ContentPart::Text(text) = &content[0] else {
        panic!("first content part must be text")
    };
    &text.content
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid identifier")
}

use serde_json::{Value, json};

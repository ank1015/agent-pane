#![cfg(unix)]

use std::{sync::Arc, time::Duration};

use execution_core::*;
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use llm_contracts::{ToolArguments, ToolDefinition, Validate as _};
use serde_json::json;
use tool_unified_exec::*;

struct Host {
    temp: tempfile::TempDir,
    runtime: Arc<SupervisorRuntime>,
}

impl Host {
    async fn new(max_process_read_bytes: u64) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(root.join("project/sub dir")).unwrap();
        let runtime = Arc::new(
            SupervisorRuntime::new(SupervisorConfig {
                host_id: ExecutionHostId::generate(),
                state_directory: temp.path().join("state"),
                roots: vec![SupervisorRoot {
                    id: RootId::new("work").unwrap(),
                    name: "Work".into(),
                    path: root,
                    read_only: false,
                }],
                limits: SupervisorLimits {
                    max_process_read_bytes,
                    termination_grace_period: Duration::from_millis(100),
                    ..Default::default()
                },
            })
            .await
            .unwrap(),
        );
        Self { temp, runtime }
    }

    fn cwd(&self) -> ExecutionPath {
        ExecutionPath::new(RootId::new("work").unwrap(), "project").unwrap()
    }

    fn tool(&self) -> UnifiedExecTool<'_> {
        UnifiedExecTool::new(
            self.runtime.as_ref(),
            self.cwd(),
            UnifiedExecConfig {
                shell: Some("/bin/sh".into()),
                default_login: false,
                max_poll_wait_ms: 50,
                ..Default::default()
            },
        )
        .unwrap()
    }
}

fn context() -> OperationContext {
    OperationContext::with_timeout(Duration::from_secs(10))
}

fn exec_ids(session_id: i32) -> ExecCommandIds {
    ExecCommandIds {
        session_id,
        operation_id: OperationId::generate(),
        execution_id: ExecutionId::generate(),
        terminate_operation_id: OperationId::generate(),
    }
}

fn write_ids() -> WriteStdinIds {
    WriteStdinIds {
        write_id: WriteId::generate(),
        interrupt_operation_id: OperationId::generate(),
    }
}

fn exec(cmd: &str, tty: bool) -> ExecCommandInput {
    ExecCommandInput {
        cmd: cmd.into(),
        workdir: None,
        shell: None,
        login: None,
        tty,
        yield_time_ms: MIN_YIELD_TIME_MS,
        max_output_tokens: None,
    }
}

#[test]
fn definitions_match_codex_function_contracts() {
    let definitions = definitions();
    assert_eq!(definitions.len(), 2);
    assert_eq!(definitions[0].name(), "exec_command");
    assert_eq!(definitions[1].name(), "write_stdin");
    for definition in &definitions {
        definition.validate().unwrap();
        let ToolDefinition::Function(function) = definition else {
            panic!("unified exec tools must be function tools")
        };
        assert_eq!(function.strict, Some(false));
        assert_eq!(
            function.output_schema.as_ref().unwrap()["required"],
            json!(["wall_time_seconds", "output"])
        );
    }

    let exec_schema = exec_command_input_schema();
    let exec_validator = jsonschema::validator_for(&exec_schema).unwrap();
    assert!(exec_validator.is_valid(&json!({"cmd":"pwd"})));
    assert!(exec_validator.is_valid(&json!({
        "cmd":"pwd", "workdir":"/tmp", "tty":true, "yield_time_ms":250,
        "max_output_tokens":1000, "shell":"/bin/sh", "login":false
    })));
    assert!(!exec_validator.is_valid(&json!({})));
    assert!(!exec_validator.is_valid(&json!({"cmd":"pwd", "unknown":true})));

    let stdin_schema = write_stdin_input_schema();
    let stdin_validator = jsonschema::validator_for(&stdin_schema).unwrap();
    assert!(stdin_validator.is_valid(&json!({"session_id":1234})));
    assert!(!stdin_validator.is_valid(&json!({"chars":"hello"})));
}

#[test]
fn parses_object_and_json_string_arguments_with_codex_defaults() {
    let exec = parse_exec_command_arguments(&ToolArguments::Object(
        json!({"cmd":"pwd"}).as_object().unwrap().clone(),
    ))
    .unwrap();
    assert!(!exec.tty);
    assert_eq!(exec.yield_time_ms, 10_000);

    let stdin =
        parse_write_stdin_arguments(&ToolArguments::String(r#"{"session_id":42}"#.into())).unwrap();
    assert_eq!(stdin.chars, "");
    assert_eq!(stdin.yield_time_ms, 250);
    assert!(
        parse_exec_command_arguments(&ToolArguments::String(
            r#"{"cmd":"pwd","extra":true}"#.into()
        ))
        .is_ok()
    );
}

#[tokio::test]
async fn executes_to_completion_and_drains_all_small_pages() {
    let host = Host::new(7).await;
    let tool = host.tool();
    let expected = "aé🙂0123456789".repeat(30);
    let prepared = tool
        .prepare_exec_command(
            exec(&format!("printf '%s' '{expected}'"), false),
            exec_ids(1001),
        )
        .unwrap();
    let mut running = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    let result = tool
        .wait_exec_command(&context(), &mut running)
        .await
        .unwrap();
    assert_eq!(result.output.output, expected);
    assert_eq!(result.output.exit_code, Some(0));
    assert_eq!(result.output.session_id, None);
    assert!(result.session.is_none());
    assert!(
        result
            .output
            .to_text()
            .contains("Process exited with code 0")
    );
}

#[tokio::test]
async fn serialized_running_state_resumes_without_duplicate_output() {
    let host = Host::new(7).await;
    let tool = host.tool();
    let expected = "0123456789".repeat(40);
    let prepared = tool
        .prepare_exec_command(
            exec(&format!("printf '%s' '{expected}'"), false),
            exec_ids(1010),
        )
        .unwrap();
    let mut running = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    tool.poll_exec_command(&context(), &mut running)
        .await
        .unwrap();
    let cursor = running.session().unwrap().cursor;
    assert!(cursor > 0);
    let saved = serde_json::to_vec(&running).unwrap();
    let mut recovered: RunningExecCommand = serde_json::from_slice(&saved).unwrap();
    assert_eq!(recovered.session().unwrap().cursor, cursor);
    let result = tool
        .wait_exec_command(&context(), &mut recovered)
        .await
        .unwrap();
    assert_eq!(result.output.output, expected);
}

#[tokio::test]
async fn returns_a_durable_session_then_writes_to_its_pty() {
    let host = Host::new(64 * 1024).await;
    let tool = host.tool();
    let prepared = tool
        .prepare_exec_command(
            exec(
                "IFS= read -r value; printf 'received:%s\\n' \"$value\"",
                true,
            ),
            exec_ids(2048),
        )
        .unwrap();
    let mut initial = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    let initial = tool
        .wait_exec_command(&context(), &mut initial)
        .await
        .unwrap();
    assert_eq!(initial.output.session_id, Some(2048));
    let session = initial.session.unwrap();

    let input = WriteStdinInput {
        session_id: 2048,
        chars: "hello world\n".into(),
        yield_time_ms: 1_000,
        max_output_tokens: None,
    };
    let prepared = tool
        .prepare_write_stdin(input, session, write_ids())
        .unwrap();
    let serialized = serde_json::to_vec(&prepared).unwrap();
    let prepared: PreparedWriteStdin = serde_json::from_slice(&serialized).unwrap();
    let mut running = tool.start_write_stdin(&context(), &prepared).await.unwrap();
    assert_eq!(running.input_status(), Some(ProcessInputStatus::Accepted));
    let result = tool
        .wait_write_stdin(&context(), &mut running)
        .await
        .unwrap();
    assert!(result.output.output.contains("received:hello world"));
    assert_eq!(result.output.exit_code, Some(0));
    assert!(result.session.is_none());
}

#[tokio::test]
async fn replaying_prepared_start_and_write_does_not_repeat_side_effects() {
    let host = Host::new(64 * 1024).await;
    let tool = host.tool();
    let marker = host.temp.path().join("workspace/project/start-count");
    let prepared = tool
        .prepare_exec_command(
            exec(
                "printf x >> start-count; IFS= read -r value; printf 'received:%s\\n' \"$value\"",
                true,
            ),
            exec_ids(3003),
        )
        .unwrap();
    let _lost_response = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    let mut recovered = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    let recovered = tool
        .wait_exec_command(&context(), &mut recovered)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "x");
    let session = recovered.session.unwrap();

    let prepared_write = tool
        .prepare_write_stdin(
            WriteStdinInput {
                session_id: 3003,
                chars: "once\n".into(),
                yield_time_ms: 1_000,
                max_output_tokens: None,
            },
            session,
            write_ids(),
        )
        .unwrap();
    let _lost_response = tool
        .start_write_stdin(&context(), &prepared_write)
        .await
        .unwrap();
    let mut recovered_write = tool
        .start_write_stdin(&context(), &prepared_write)
        .await
        .unwrap();
    assert_eq!(
        recovered_write.input_status(),
        Some(ProcessInputStatus::AlreadyAccepted)
    );
    let result = tool
        .wait_write_stdin(&context(), &mut recovered_write)
        .await
        .unwrap();
    assert_eq!(result.output.output.matches("received:once").count(), 1);
}

#[tokio::test]
async fn non_tty_rejects_text_but_accepts_interrupt() {
    let host = Host::new(64 * 1024).await;
    let tool = host.tool();
    let prepared = tool
        .prepare_exec_command(exec("sleep 30", false), exec_ids(4004))
        .unwrap();
    let mut running = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    let result = tool
        .wait_exec_command(&context(), &mut running)
        .await
        .unwrap();
    let session = result.session.unwrap();

    let error = tool
        .prepare_write_stdin(
            WriteStdinInput {
                session_id: 4004,
                chars: "hello".into(),
                yield_time_ms: 250,
                max_output_tokens: None,
            },
            session.clone(),
            write_ids(),
        )
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::StdinClosed);

    let interrupt = tool
        .prepare_write_stdin(
            WriteStdinInput {
                session_id: 4004,
                chars: "\u{3}".into(),
                yield_time_ms: 1_000,
                max_output_tokens: None,
            },
            session,
            write_ids(),
        )
        .unwrap();
    assert!(interrupt.interrupt_request().is_some());
    let mut running = tool
        .start_write_stdin(&context(), &interrupt)
        .await
        .unwrap();
    let result = tool
        .wait_write_stdin(&context(), &mut running)
        .await
        .unwrap();
    assert!(result.session.is_none());
}

#[tokio::test]
async fn clamps_yields_resolves_workdir_and_caps_output_tokens() {
    let host = Host::new(64 * 1024).await;
    let tool = UnifiedExecTool::new(
        host.runtime.as_ref(),
        host.cwd(),
        UnifiedExecConfig {
            shell: Some("/bin/sh".into()),
            default_login: false,
            default_max_output_tokens: 2,
            max_output_tokens: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let prepared = tool
        .prepare_exec_command(
            ExecCommandInput {
                workdir: Some("sub dir".into()),
                yield_time_ms: 1,
                max_output_tokens: Some(1_000),
                ..exec("pwd; printf 0123456789abcdef", false)
            },
            exec_ids(5005),
        )
        .unwrap();
    assert_eq!(prepared.effective_yield_time_ms(), 250);
    let mut running = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    let result = tool
        .wait_exec_command(&context(), &mut running)
        .await
        .unwrap();
    assert!(result.output.to_text().contains("tokens truncated"));
    assert!(
        !result.output.code_mode_result()["output"]
            .as_str()
            .unwrap()
            .contains("tokens truncated")
    );

    let prepared = tool
        .prepare_exec_command(exec("sleep 30", true), exec_ids(5006))
        .unwrap();
    let mut running = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    let session = tool
        .wait_exec_command(&context(), &mut running)
        .await
        .unwrap()
        .session
        .unwrap();
    let poll = tool
        .prepare_write_stdin(
            WriteStdinInput {
                session_id: 5006,
                chars: String::new(),
                yield_time_ms: 1,
                max_output_tokens: None,
            },
            session.clone(),
            write_ids(),
        )
        .unwrap();
    assert_eq!(poll.effective_yield_time_ms(), 5_000);
    tool.terminate_session(&context(), &session).await.unwrap();
}

#[tokio::test]
async fn hard_output_cap_preserves_head_tail_and_an_explicit_marker() {
    let host = Host::new(64 * 1024).await;
    let tool = UnifiedExecTool::new(
        host.runtime.as_ref(),
        host.cwd(),
        UnifiedExecConfig {
            shell: Some("/bin/sh".into()),
            default_login: false,
            max_output_bytes: 10,
            ..Default::default()
        },
    )
    .unwrap();
    let prepared = tool
        .prepare_exec_command(exec("printf 0123456789abcdef", false), exec_ids(6006))
        .unwrap();
    let mut running = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    let output = tool
        .wait_exec_command(&context(), &mut running)
        .await
        .unwrap()
        .output;
    assert!(output.output.starts_with("01234"));
    assert!(output.output.ends_with("bcdef"));
    assert!(output.output.contains("6 bytes omitted"));
    assert_eq!(output.original_token_count, Some(4));
}

#[tokio::test]
async fn disabled_login_empty_script_and_normalized_workdir() {
    let host = Host::new(64 * 1024).await;
    let tool = UnifiedExecTool::new(
        host.runtime.as_ref(),
        host.cwd(),
        UnifiedExecConfig {
            shell: Some("/bin/sh".into()),
            allow_login_shell: false,
            allow_shell_override: false,
            ..Default::default()
        },
    )
    .unwrap();
    let prepared = tool
        .prepare_exec_command(
            ExecCommandInput {
                workdir: Some("sub dir/..".into()),
                ..exec("", false)
            },
            exec_ids(7001),
        )
        .unwrap();
    assert_eq!(prepared.request().cwd, host.cwd());
    assert!(matches!(
        prepared.request().command,
        CommandSpec::ShellScript { login: false, .. }
    ));
    let ToolDefinition::Function(definition) = &tool.definitions()[0] else {
        panic!()
    };
    assert!(definition.parameters["properties"].get("login").is_none());
    assert!(definition.parameters["properties"].get("shell").is_none());
    assert!(
        tool.prepare_exec_command(
            ExecCommandInput {
                login: Some(true),
                ..exec("", false)
            },
            exec_ids(7002)
        )
        .is_err()
    );
    let mut running = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    assert_eq!(
        tool.wait_exec_command(&context(), &mut running)
            .await
            .unwrap()
            .output
            .exit_code,
        Some(0)
    );
}

#[tokio::test]
async fn reports_child_exit_without_waiting_for_descendant_pipes_or_pty() {
    let host = Host::new(64 * 1024).await;
    let tool = host.tool();
    for tty in [false, true] {
        let prepared = tool
            .prepare_exec_command(
                exec("trap '' HUP; sleep 2 & printf done; exit 7", tty),
                exec_ids(if tty { 7004 } else { 7003 }),
            )
            .unwrap();
        let start = std::time::Instant::now();
        let mut running = tool
            .start_exec_command(&context(), &prepared)
            .await
            .unwrap();
        let result = tool
            .wait_exec_command(&context(), &mut running)
            .await
            .unwrap();
        assert_eq!(result.output.exit_code, Some(7), "{result:?}");
        assert!(result.session.is_none());
        assert!(result.output.output.contains("done"));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}

#[tokio::test]
async fn stdin_wait_begins_after_delayed_checkpoint_and_dispatch() {
    let host = Host::new(64 * 1024).await;
    let tool = host.tool();
    let prepared = tool
        .prepare_exec_command(
            exec(
                "read value; sleep 0.12; printf 'received:%s' \"$value\"; sleep 1",
                true,
            ),
            exec_ids(7005),
        )
        .unwrap();
    let mut running = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    let session = tool
        .wait_exec_command(&context(), &mut running)
        .await
        .unwrap()
        .session
        .unwrap();
    let prepared = tool
        .prepare_write_stdin(
            WriteStdinInput {
                session_id: 7005,
                chars: "hello\n".into(),
                yield_time_ms: 250,
                max_output_tokens: None,
            },
            session,
            write_ids(),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let prepared = serde_json::from_slice(&serde_json::to_vec(&prepared).unwrap()).unwrap();
    let mut running = tool.start_write_stdin(&context(), &prepared).await.unwrap();
    let result = tool
        .wait_write_stdin(&context(), &mut running)
        .await
        .unwrap();
    assert!(
        result.output.output.contains("received:hello"),
        "{result:?}"
    );
    assert!(result.output.wall_time_seconds >= 0.24);
    assert!(result.output.wall_time_seconds < 0.6);
    tool.terminate_session(&context(), result.session.as_ref().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn code_mode_preserves_omitted_budget_while_history_is_limited() {
    let host = Host::new(64 * 1024).await;
    let tool = UnifiedExecTool::new(
        host.runtime.as_ref(),
        host.cwd(),
        UnifiedExecConfig {
            shell: Some("/bin/sh".into()),
            default_login: false,
            default_max_output_tokens: 2,
            ..Default::default()
        },
    )
    .unwrap();
    for budget in [None, Some(2)] {
        let prepared = tool
            .prepare_exec_command(
                ExecCommandInput {
                    max_output_tokens: budget,
                    ..exec("printf 'start🙂middle🙂finish'", false)
                },
                exec_ids(7006),
            )
            .unwrap();
        let mut running = tool
            .start_exec_command(&context(), &prepared)
            .await
            .unwrap();
        let result = tool
            .wait_exec_command(&context(), &mut running)
            .await
            .unwrap();
        assert!(
            result
                .output
                .to_text()
                .contains("Warning: truncated output")
        );
        let code = result.output.code_mode_result();
        let output = code["output"].as_str().unwrap();
        assert_eq!(output.contains("tokens truncated"), budget.is_some());
        assert!(code.get("model_output_tokens").is_none());
        assert!(code.get("max_output_tokens").is_none());
        if budget.is_none() {
            assert_eq!(output, "start🙂middle🙂finish");
        }
    }
}

#[tokio::test]
async fn intercepted_patch_is_checkpointed_and_replay_does_not_overwrite_later_edits() {
    let host = Host::new(64 * 1024).await;
    let tool = host.tool();
    let prepared = tool.prepare_exec_command(exec("cd 'sub dir' && apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: created\n+once\n*** End Patch\nPATCH", false), exec_ids(7007)).unwrap();
    let mut running = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap();
    assert!(running.session().is_none());
    let path = host.temp.path().join("workspace/project/sub dir/created");
    assert!(!path.exists());
    // First poll prepares the exact write. Persist it before dispatch.
    tool.poll_exec_command(&context(), &mut running)
        .await
        .unwrap();
    assert!(!path.exists());
    let checkpoint = serde_json::to_vec(&running).unwrap();
    tool.poll_exec_command(&context(), &mut running)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "once\n");
    std::fs::write(&path, "later\n").unwrap();
    let mut recovered: RunningExecCommand = serde_json::from_slice(&checkpoint).unwrap();
    let result = tool
        .wait_exec_command(&context(), &mut recovered)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "later\n");
    assert!(result.output.output.contains("A created"));
    assert!(result.output.exit_code.is_none());
    assert!(result.output.chunk_id.is_none());
    assert!(result.session.is_none());
}

#[tokio::test]
async fn stale_cached_descriptor_cannot_spawn_on_restarted_supervisor() {
    struct CachedRuntime<'a> {
        descriptor: ExecutionHostDescriptor,
        actual: &'a SupervisorRuntime,
    }
    impl ExecutionRuntime for CachedRuntime<'_> {
        fn descriptor(&self) -> &ExecutionHostDescriptor {
            &self.descriptor
        }
        fn filesystem(&self) -> &dyn FileSystem {
            self.actual.filesystem()
        }
        fn processes(&self) -> &dyn ProcessRuntime {
            self.actual.processes()
        }
    }
    let host = Host::new(64 * 1024).await;
    let mut stale = host.runtime.descriptor().clone();
    stale.supervisor_generation_id = SupervisorGenerationId::generate();
    let runtime = CachedRuntime {
        descriptor: stale,
        actual: &host.runtime,
    };
    let tool = UnifiedExecTool::new(&runtime, host.cwd(), UnifiedExecConfig::default()).unwrap();
    let prepared = tool
        .prepare_exec_command(exec("printf bad > should-not-exist", false), exec_ids(7008))
        .unwrap();
    let failure = tool
        .start_exec_command(&context(), &prepared)
        .await
        .unwrap_err();
    assert_eq!(failure.code, ExecutionErrorCode::ExecutionLost);
    assert!(
        !host
            .temp
            .path()
            .join("workspace/project/should-not-exist")
            .exists()
    );
}

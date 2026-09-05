#![cfg(unix)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use execution_client::{ExecutionClient, ExecutionClientConfig, GatewayHostRuntime};
use execution_core::*;
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use execution_wire::{Operation, RequestEnvelope, ResponseEnvelope, dispatch_request};
use tool_bash_minimal::*;

struct Host {
    temp: tempfile::TempDir,
    runtime: GatewayHostRuntime,
    starts: Arc<AtomicUsize>,
    terminations: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Host {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Host {
    async fn new(mode: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(root.join("project/sub dir")).unwrap();
        let supervisor = Arc::new(
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
                    max_process_read_bytes: 7,
                    termination_grace_period: Duration::from_millis(50),
                    ..Default::default()
                },
            })
            .await
            .unwrap(),
        );
        let starts = Arc::new(AtomicUsize::new(0));
        let terminations = Arc::new(AtomicUsize::new(0));
        let start_count = starts.clone();
        let terminate_count = terminations.clone();
        let target = supervisor.clone();
        let lose_start = Arc::new(AtomicBool::new(mode == "lost-start"));
        let lose_read = Arc::new(AtomicBool::new(mode == "lost-read"));
        let bad_cursor = mode == "bad-cursor";
        let app = Router::new().route(
            "/v1/hosts/{host}/operations",
            post(
                move |Path(id): Path<String>,
                      headers: HeaderMap,
                      Json(request): Json<RequestEnvelope>| {
                    let target = target.clone();
                    let starts = start_count.clone();
                    let terminations = terminate_count.clone();
                    let lose_start = lose_start.clone();
                    let lose_read = lose_read.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer bash-test-token");
                        assert_eq!(id, target.descriptor().host_id.as_str());
                        let start = matches!(request.operation, Operation::ProcessStart(_));
                        let read = matches!(request.operation, Operation::ProcessRead(_));
                        if start {
                            starts.fetch_add(1, Ordering::SeqCst);
                        }
                        if matches!(request.operation, Operation::ProcessTerminate(_)) {
                            terminations.fetch_add(1, Ordering::SeqCst);
                        }
                        let result =
                            dispatch_request(target.as_ref(), &OperationContext::new(), request)
                                .await;
                        if matches!(result, ResponseEnvelope::Success { .. })
                            && ((start && lose_start.swap(false, Ordering::SeqCst))
                                || (read && lose_read.swap(false, Ordering::SeqCst)))
                        {
                            return (StatusCode::BAD_GATEWAY, "response lost").into_response();
                        }
                        if read && bad_cursor {
                            let mut value = serde_json::to_value(result).unwrap();
                            // Use the serialized wire result, without replacing the transport.
                            fn corrupt(value: &mut serde_json::Value) {
                                if let Some(object) = value.as_object_mut() {
                                    if let Some(cursor) = object.get_mut("next_sequence") {
                                        *cursor = serde_json::json!(999999);
                                    }
                                    for value in object.values_mut() {
                                        corrupt(value);
                                    }
                                }
                            }
                            corrupt(&mut value);
                            return Json(value).into_response();
                        }
                        Json(result).into_response()
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = ExecutionClientConfig::new(url.parse().unwrap(), "bash-test-token");
        config.allow_insecure_http = true;
        let runtime = ExecutionClient::new(config)
            .unwrap()
            .connect_host(&context(), supervisor.descriptor().host_id.clone())
            .await
            .unwrap();
        Self {
            temp,
            runtime,
            starts,
            terminations,
            server,
        }
    }
    fn cwd(&self) -> ExecutionPath {
        ExecutionPath::new(RootId::new("work").unwrap(), "project").unwrap()
    }
    fn tool(&self) -> BashTool<'_> {
        self.configured(BashConfig::default())
    }
    fn configured(&self, config: BashConfig) -> BashTool<'_> {
        BashTool::new(
            &self.runtime,
            self.cwd(),
            BashConfig {
                shell: Some("/bin/sh".into()),
                ..config
            },
        )
        .unwrap()
    }
    async fn run(&self, input: BashInput) -> BashOutput {
        let tool = self.tool();
        let prepared = tool.prepare(input, ids()).unwrap();
        let mut running = tool.start(&context(), &prepared).await.unwrap();
        tool.wait(&context(), &mut running).await.unwrap()
    }
}

fn context() -> OperationContext {
    OperationContext::with_timeout(Duration::from_secs(10))
}
fn ids() -> BashIds {
    BashIds {
        operation_id: OperationId::generate(),
        execution_id: ExecutionId::generate(),
        terminate_operation_id: OperationId::generate(),
    }
}
fn input(command: &str) -> BashInput {
    BashInput {
        command: command.into(),
        workdir: None,
        timeout: None,
    }
}

#[test]
fn schema_has_only_command_timeout_workdir() {
    let schema = input_schema();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert_eq!(schema["properties"].as_object().unwrap().len(), 3);
    assert_eq!(schema["required"], serde_json::json!(["command"]));
    assert!(validator.is_valid(&serde_json::json!({"command":"pwd"})));
    assert!(validator.is_valid(&serde_json::json!({"command":"pwd","timeout":1_800_000})));
    assert_eq!(MAX_TIMEOUT_MS, 30 * 60 * 1000);
    for invalid in [
        serde_json::json!({}),
        serde_json::json!({"command":""}),
        serde_json::json!({"command":"pwd","timeout":0}),
        serde_json::json!({"command":"pwd","timeout":1800001}),
        serde_json::json!({"command":"pwd","timeout":1.2}),
        serde_json::json!({"command":"pwd","run_in_background":true}),
    ] {
        assert!(!validator.is_valid(&invalid));
    }
    assert!(
        serde_json::from_value::<BashInput>(serde_json::json!({"command":"pwd","description":"x"}))
            .is_err()
    );
}

#[tokio::test]
async fn executes_real_shell_and_preserves_both_streams_and_nonzero_status() {
    let host = Host::new("").await;
    let result = host
        .run(input("printf 'stdout'; printf 'stderr' >&2; exit 7"))
        .await;
    assert!(result.output.contains("stdout"));
    assert!(result.output.contains("stderr"));
    assert_eq!(result.exit_code, Some(7));
    assert!(result.is_error());
    assert!(result.to_text().contains("exit code: 7"));
}

#[tokio::test]
async fn resolves_workdir_without_persisting_shell_state() {
    let host = Host::new("").await;
    let result = host
        .run(BashInput {
            workdir: Some("sub dir".into()),
            ..input("pwd; export BASH_TOOL_TEST_VAR=secret; cd /")
        })
        .await;
    assert!(result.output.trim().ends_with("/project/sub dir"));
    let next = host
        .run(input("pwd; printf '%s' \"${BASH_TOOL_TEST_VAR-unset}\""))
        .await;
    assert!(next.output.contains("/project\nunset"));
    let absolute =
        std::fs::canonicalize(host.temp.path().join("workspace/project/sub dir")).unwrap();
    let result = host
        .run(BashInput {
            workdir: Some(absolute.to_str().unwrap().into()),
            ..input("pwd")
        })
        .await;
    assert_eq!(result.output.trim(), absolute.to_str().unwrap());
}

#[tokio::test]
async fn closed_stdin_and_empty_success() {
    let host = Host::new("").await;
    let result = host.run(input("cat; true")).await;
    assert_eq!(result.output, "");
    assert!(!result.is_error());
}

#[tokio::test]
async fn thirty_minute_timeout_is_accepted_by_the_execution_runtime() {
    let host = Host::new("").await;
    let result = host
        .run(BashInput {
            timeout: Some(1_800_000),
            ..input("printf accepted")
        })
        .await;
    assert!(!result.is_error());
    assert_eq!(result.output, "accepted");
}

#[tokio::test]
async fn validates_before_dispatch() {
    let host = Host::new("").await;
    for invalid in [
        input(""),
        input("a\0b"),
        BashInput {
            timeout: Some(0),
            ..input("pwd")
        },
        BashInput {
            timeout: Some(1800001),
            ..input("pwd")
        },
        BashInput {
            workdir: Some("../".into()),
            ..input("pwd")
        },
        BashInput {
            workdir: Some("/outside-workspace".into()),
            ..input("pwd")
        },
    ] {
        assert!(host.tool().prepare(invalid, ids()).is_err());
    }
    let prepared = host.tool().prepare(input("true"), ids()).unwrap();
    assert_eq!(prepared.request().timeout_ms, Some(120000));
    let longest = host
        .tool()
        .prepare(
            BashInput {
                timeout: Some(MAX_TIMEOUT_MS),
                ..input("true")
            },
            ids(),
        )
        .unwrap();
    assert_eq!(longest.request().timeout_ms, Some(1_800_000));
    let cancelled = context();
    cancelled.cancel();
    assert!(host.tool().start(&cancelled, &prepared).await.is_err());
    assert_eq!(host.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn drains_terminal_output_across_many_small_pages() {
    let host = Host::new("").await;
    let text = "aé🙂0123456789".repeat(30);
    let tool = host.tool();
    let prepared = tool
        .prepare(input(&format!("printf '%s' '{text}'")), ids())
        .unwrap();
    let mut running = tool.start(&context(), &prepared).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let result = tool.wait(&context(), &mut running).await.unwrap();
    assert_eq!(result.output, text);
    assert!(!result.truncated);
    assert!(running.cursor() > text.len() as u64);
}

#[tokio::test]
async fn truncates_tail_but_poll_exposes_full_output() {
    let host = Host::new("").await;
    let tool = host.configured(BashConfig {
        max_output_bytes: 10,
        max_output_lines: 2,
        ..Default::default()
    });
    let prepared = tool
        .prepare(input("printf 'one\ntwo\nthree\nfour\n'"), ids())
        .unwrap();
    let mut running = tool.start(&context(), &prepared).await.unwrap();
    let mut full = Vec::new();
    loop {
        let page = tool.poll(&context(), &mut running).await.unwrap();
        for event in page.events {
            if let ProcessEventKind::Output { data, .. } = event.event {
                full.extend_from_slice(data.as_slice());
            }
        }
        if page.complete {
            break;
        }
    }
    let result = running.output();
    assert_eq!(full, b"one\ntwo\nthree\nfour\n");
    assert!(result.truncated);
    assert!(result.output.ends_with("four\n"));
    assert!(result.output.len() <= 10);
    assert!(result.to_text().contains("truncated"));
}

#[tokio::test]
async fn resumes_serialized_state_without_duplicate_output_or_start() {
    let host = Host::new("").await;
    let tool = host.tool();
    let prepared = tool
        .prepare(input("printf 'abcdefghijklmno'"), ids())
        .unwrap();
    let mut running = tool.start(&context(), &prepared).await.unwrap();
    tool.poll(&context(), &mut running).await.unwrap();
    let saved = serde_json::to_vec(&running).unwrap();
    let mut restored: RunningBash = serde_json::from_slice(&saved).unwrap();
    let result = tool.wait(&context(), &mut restored).await.unwrap();
    assert_eq!(result.output, "abcdefghijklmno");
    assert_eq!(host.starts.load(Ordering::SeqCst), 1);
    assert_eq!(
        tool.wait(&context(), &mut restored).await.unwrap().output,
        result.output
    );
}

#[tokio::test]
async fn lost_start_response_is_not_retried_automatically() {
    let host = Host::new("lost-start").await;
    let tool = host.tool();
    let prepared = tool
        .prepare(input("printf x >> count; printf done"), ids())
        .unwrap();
    assert!(tool.start(&context(), &prepared).await.is_err());
    assert_eq!(host.starts.load(Ordering::SeqCst), 1);
    let restored: PreparedBash =
        serde_json::from_slice(&serde_json::to_vec(&prepared).unwrap()).unwrap();
    let mut running = tool.start(&context(), &restored).await.unwrap();
    assert_eq!(
        tool.wait(&context(), &mut running).await.unwrap().output,
        "done"
    );
    assert_eq!(
        std::fs::read(host.temp.path().join("workspace/project/count")).unwrap(),
        b"x"
    );
}

#[tokio::test]
async fn failed_read_preserves_cursor_and_can_reconnect() {
    let host = Host::new("lost-read").await;
    let tool = host.tool();
    let prepared = tool.prepare(input("printf recovery"), ids()).unwrap();
    let mut running = tool.start(&context(), &prepared).await.unwrap();
    assert!(tool.wait(&context(), &mut running).await.is_err());
    assert_eq!(running.cursor(), 0);
    assert_eq!(host.terminations.load(Ordering::SeqCst), 0);
    assert_eq!(
        tool.wait(&context(), &mut running).await.unwrap().output,
        "recovery"
    );
}

#[tokio::test]
async fn malformed_cursor_does_not_commit_partial_state() {
    let host = Host::new("bad-cursor").await;
    let tool = host.tool();
    let prepared = tool.prepare(input("printf text"), ids()).unwrap();
    let mut running = tool.start(&context(), &prepared).await.unwrap();
    assert!(tool.poll(&context(), &mut running).await.is_err());
    assert_eq!(running.cursor(), 0);
    assert_eq!(running.output().output, "");
}

#[tokio::test]
async fn remote_timeout_ends_command_and_retains_partial_output() {
    let host = Host::new("").await;
    let result = host
        .run(BashInput {
            timeout: Some(150),
            ..input("printf before; sleep 10; printf after")
        })
        .await;
    assert_eq!(result.output, "before");
    assert_eq!(result.handle.state, ExecutionState::Failed);
    assert!(result.is_error());
}

#[tokio::test]
async fn cancellation_terminates_remote_execution() {
    let host = Host::new("").await;
    let tool = host.tool();
    let prepared = tool
        .prepare(input("printf ready; sleep 10; printf after"), ids())
        .unwrap();
    let mut running = tool.start(&context(), &prepared).await.unwrap();
    let cancelled = context();
    let signal = cancelled.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        signal.cancel();
    });
    let error = tool.wait(&cancelled, &mut running).await.unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::Cancelled);
    assert_eq!(host.terminations.load(Ordering::SeqCst), 1);
    let result = tool.wait(&context(), &mut running).await.unwrap();
    assert_eq!(result.handle.state, ExecutionState::Cancelled);
    assert!(!result.output.contains("after"));
}

#[tokio::test]
async fn caller_deadline_terminates_remote_execution() {
    let host = Host::new("").await;
    let tool = host.tool();
    let prepared = tool.prepare(input("sleep 10"), ids()).unwrap();
    let mut running = tool.start(&context(), &prepared).await.unwrap();
    let deadline = OperationContext::with_timeout(Duration::from_millis(100));
    assert_eq!(
        tool.wait(&deadline, &mut running).await.unwrap_err().code,
        ExecutionErrorCode::DeadlineExceeded
    );
    assert_eq!(host.terminations.load(Ordering::SeqCst), 1);
    assert_eq!(
        tool.wait(&context(), &mut running)
            .await
            .unwrap()
            .handle
            .state,
        ExecutionState::Cancelled
    );
}

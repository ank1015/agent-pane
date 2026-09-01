use std::{
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    routing::{get, patch, post},
};
use environment_harness::{
    clients::ExecutionClient,
    harness::tools::{
        ToolExecutionContext as GatewayToolExecutionContext, execute_tool_call,
        execute_tool_call_with_runtime,
    },
};
use execution_contracts::{MachineId, WorkspaceRootId};
use execution_gateway_client::{ExecutionGatewayClientError, ExecutionGatewayConfig};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_runtime::OperationContext;
use llm_contracts::{
    AssistantContent, ContentPart, ImageSource, ToolArguments, ToolCallId, ToolResultMessage,
    ToolResultOutcome,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use url::Url;
use uuid::Uuid;

const PROJECT_ID: &str = "019d2aa0-0000-7000-8000-000000000010";

struct Fixture {
    _directory: TempDir,
    workspace: std::path::PathBuf,
    runtime: LocalExecutionRuntime,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_root_count(1).await
    }

    async fn with_root_count(root_count: usize) -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        tokio::fs::create_dir_all(&workspace)
            .await
            .expect("create workspace");
        let workspace = tokio::fs::canonicalize(workspace)
            .await
            .expect("canonical workspace path");
        let mut workspace_roots = Vec::new();
        for index in 0..root_count {
            let path = if index == 0 {
                workspace.clone()
            } else {
                let path = directory.path().join(format!("workspace-{index}"));
                tokio::fs::create_dir_all(&path)
                    .await
                    .expect("create additional workspace");
                tokio::fs::canonicalize(path)
                    .await
                    .expect("canonical additional workspace")
            };
            workspace_roots.push(LocalWorkspaceRoot {
                id: id::<WorkspaceRootId>(&format!("root-{index}")),
                name: format!("workspace-{index}"),
                path,
                read_only: false,
            });
        }
        let runtime = LocalExecutionRuntime::new(LocalRuntimeConfig {
            machine_id: id::<MachineId>("machine"),
            name: "Environment tool tests".to_owned(),
            state_directory: directory.path().join("state"),
            workspace_roots,
            native_grants: Vec::new(),
        })
        .await
        .expect("local execution runtime");
        Self {
            _directory: directory,
            workspace,
            runtime,
        }
    }
}

struct ToolExecutionContext<'a> {
    runtime: &'a LocalExecutionRuntime,
    operation: &'a OperationContext,
}

#[tokio::test]
async fn lists_only_connected_online_tunneled_machines() {
    let app = Router::new().route(
        "/v1/machines",
        get(|| async {
            Json(json!([
                machine_summary("tunnel-online", "Developer laptop", "machine_daemon", true),
                machine_summary("tunnel-offline", "Offline laptop", "machine_daemon", false),
                machine_summary("sandbox-online", "Sandbox", "sandbox", true)
            ]))
        }),
    );
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_tool_call(
        &AssistantContent::ToolCall {
            name: "get_tunnel_machines_list".to_owned(),
            arguments: ToolArguments::Object(serde_json::Map::new()),
            tool_call_id: ToolCallId::new("list-machines-call").expect("tool call id"),
        },
        &GatewayToolExecutionContext {
            execution: &execution,
            operation: &operation,
            project_id: project_id(),
            search: test_search_context(),
            scrape: test_scrape_context(),
        },
    )
    .await
    .expect("assistant tool call");

    assert_success(&result);
    assert_eq!(
        serde_json::from_str::<Value>(text(&result)).expect("JSON tool content"),
        json!([{
            "name": "Developer laptop",
            "machineId": "tunnel-online",
            "workspaceRoots": [{
                "id": "root",
                "name": "Project",
                "uri": "file:///workspace/project",
                "readOnly": false
            }]
        }])
    );
    assert_eq!(result.details, serde_json::from_str(text(&result)).ok());
}

#[tokio::test]
async fn lists_configured_sandbox_accounts() {
    let app = Router::new().route(
        "/v1/sandbox-accounts",
        get(|| async {
            Json(json!([
                {
                    "account_id": "019d2aa0-0000-7000-8000-000000000001",
                    "account_name": "E2B main",
                    "provider_name": "e2b"
                },
                {
                    "account_id": "019d2aa0-0000-7000-8000-000000000002",
                    "account_name": "Daytona team",
                    "provider_name": "daytona"
                }
            ]))
        }),
    );
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_tool_call(
        &AssistantContent::ToolCall {
            name: "get_sandbox_accounts_list".to_owned(),
            arguments: ToolArguments::Object(serde_json::Map::new()),
            tool_call_id: ToolCallId::new("list-accounts-call").expect("tool call id"),
        },
        &GatewayToolExecutionContext {
            execution: &execution,
            operation: &operation,
            project_id: project_id(),
            search: test_search_context(),
            scrape: test_scrape_context(),
        },
    )
    .await
    .expect("assistant tool call");

    assert_success(&result);
    assert_eq!(
        serde_json::from_str::<Value>(text(&result)).expect("JSON tool content"),
        json!([
            {
                "accountName": "E2B main",
                "providerName": "e2b",
                "accountId": "019d2aa0-0000-7000-8000-000000000001"
            },
            {
                "accountName": "Daytona team",
                "providerName": "daytona",
                "accountId": "019d2aa0-0000-7000-8000-000000000002"
            }
        ])
    );
    assert_eq!(result.details, serde_json::from_str(text(&result)).ok());
}

#[tokio::test]
async fn lists_environments_for_the_configured_project() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/v1/projects/{project_id}/environments",
            get(list_project_environments_response),
        )
        .with_state(captured.clone());
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(&execution, &operation, "list_environments", json!({})).await;

    assert_success(&result);
    assert_eq!(
        serde_json::from_str::<Value>(text(&result)).expect("JSON tool content"),
        json!([
            {
                "id": "environment-1",
                "name": "Local development",
                "host_name": "Developer laptop",
                "machine_id": "tunnel-online",
                "path": "projects/example",
                "type": "env",
                "created_at": 3
            },
            {
                "id": "019d2aa0-0000-7000-8000-000000000021",
                "name": "Sandbox development",
                "host_name": "E2B main",
                "path": "project",
                "type": "template",
                "snapshot_id": "019d2aa0-0000-7000-8000-000000000020",
                "setup_script": "pnpm install",
                "created_at": 4
            }
        ])
    );
    assert_eq!(result.details, serde_json::from_str(text(&result)).ok());
    assert_eq!(
        captured
            .lock()
            .expect("environment list request capture")
            .as_slice(),
        [json!({
            "project_id": PROJECT_ID,
            "authorization": "Bearer api-token"
        })]
    );
}

#[tokio::test]
async fn discovery_tools_reject_arguments_without_calling_the_gateway() {
    let execution =
        execution_client("http://127.0.0.1:1".parse().expect("URL")).expect("execution client");
    let operation = OperationContext::new();
    for name in [
        "get_tunnel_machines_list",
        "get_sandbox_accounts_list",
        "list_environments",
    ] {
        let result = execute_tool_call(
            &AssistantContent::ToolCall {
                name: name.to_owned(),
                arguments: ToolArguments::Object(
                    [("machineId".to_owned(), json!("unnecessary"))]
                        .into_iter()
                        .collect(),
                ),
                tool_call_id: ToolCallId::new(format!("{name}-call")).expect("tool call id"),
            },
            &GatewayToolExecutionContext {
                execution: &execution,
                operation: &operation,
                project_id: project_id(),
                search: test_search_context(),
                scrape: test_scrape_context(),
            },
        )
        .await
        .expect("assistant tool call");

        assert_error(&result, "invalid_arguments");
        assert!(text(&result).contains("does not accept arguments"));
    }
}

#[tokio::test]
async fn executes_web_tools_without_a_machine_target() {
    let app = Router::new()
        .route(
            "/v2/search",
            post(|| async {
                Json(json!({
                    "success": true,
                    "data": {
                        "web": [{
                            "title": "Environment documentation",
                            "description": "Relevant documentation",
                            "url": "https://example.com/docs"
                        }]
                    },
                    "id": "search-1",
                    "creditsUsed": 1
                }))
            }),
        )
        .route(
            "/v2/scrape",
            post(|| async {
                Json(json!({
                    "success": true,
                    "data": {
                        "markdown": "# Environment documentation\n\nUseful content.",
                        "metadata": {
                            "title": "Environment documentation",
                            "sourceURL": "https://example.com/docs",
                            "statusCode": 200,
                            "contentType": "text/html"
                        }
                    }
                }))
            }),
        );
    let base_url = serve(app).await;
    let search = tool_firecrawl_search::FirecrawlSearchToolContext::with_client(
        "test-key",
        base_url.join("/v2/search").expect("search URL"),
        reqwest::Client::new(),
    )
    .expect("search context");
    let scrape = tool_firecrawl_scrape::FirecrawlScrapeToolContext::with_client(
        "test-key",
        base_url.join("/v2/scrape").expect("scrape URL"),
        reqwest::Client::new(),
    )
    .expect("scrape context");
    let execution =
        execution_client("http://127.0.0.1:1".parse().expect("URL")).expect("execution client");
    let operation = OperationContext::new();
    let context = GatewayToolExecutionContext {
        execution: &execution,
        operation: &operation,
        project_id: project_id(),
        search: &search,
        scrape: &scrape,
    };

    let search_result = execute_tool_call(
        &AssistantContent::ToolCall {
            name: "search".to_owned(),
            arguments: ToolArguments::Object(
                [("query".to_owned(), json!("environment documentation"))]
                    .into_iter()
                    .collect(),
            ),
            tool_call_id: ToolCallId::new("search-call").expect("tool call id"),
        },
        &context,
    )
    .await
    .expect("search tool call");
    assert_success(&search_result);
    assert!(text(&search_result).contains("Environment documentation"));

    let scrape_result = execute_tool_call(
        &AssistantContent::ToolCall {
            name: "scrape".to_owned(),
            arguments: ToolArguments::Object(
                [("url".to_owned(), json!("https://example.com/docs"))]
                    .into_iter()
                    .collect(),
            ),
            tool_call_id: ToolCallId::new("scrape-call").expect("tool call id"),
        },
        &context,
    )
    .await
    .expect("scrape tool call");
    assert_success(&scrape_result);
    assert!(text(&scrape_result).contains("# Environment documentation"));
}

#[tokio::test]
async fn creates_base_and_snapshot_sandboxes() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/v1/sandbox-accounts/{account_id}/sandboxes",
            post(create_sandbox_response),
        )
        .with_state(captured.clone());
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let account_id = "019d2aa0-0000-7000-8000-000000000001";
    let snapshot_id = "019d2aa0-0000-7000-8000-000000000002";

    let base = execute_gateway_tool(
        &execution,
        &operation,
        "create_sandbox",
        json!({"account_id": account_id}),
    )
    .await;
    let restored = execute_gateway_tool(
        &execution,
        &operation,
        "create_sandbox",
        json!({"account_id": account_id, "snapshot_id": snapshot_id}),
    )
    .await;

    for result in [&base, &restored] {
        assert_success(result);
        assert_eq!(
            serde_json::from_str::<Value>(text(result)).expect("JSON tool content"),
            json!({
                "status": "successful",
                "machineId": "sandbox-created",
                "workspaceRoot": {
                    "id": "root",
                    "name": "Project",
                    "uri": "file:///workspace/project",
                    "readOnly": false
                }
            })
        );
    }
    let captured = captured.lock().expect("sandbox request capture");
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0]["account_id"], account_id);
    assert_eq!(captured[0]["authorization"], "Bearer api-token");
    assert_eq!(captured[0]["body"], json!({}));
    assert_eq!(captured[1]["body"], json!({"snapshot_id": snapshot_id}));
}

#[tokio::test]
async fn returns_sandbox_creation_failures_as_tool_errors() {
    let app = Router::new().route(
        "/v1/sandbox-accounts/{account_id}/sandboxes",
        post(|| async {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "code": "disconnected",
                    "message": "sandbox provider is unavailable",
                    "retryable": true
                })),
            )
        }),
    );
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "create_sandbox",
        json!({"account_id": "019d2aa0-0000-7000-8000-000000000001"}),
    )
    .await;

    assert_error(&result, "sandbox_creation_failed");
    assert!(text(&result).contains("sandbox provider is unavailable"));
}

#[tokio::test]
async fn create_sandbox_rejects_invalid_account_ids_before_calling_the_gateway() {
    let execution =
        execution_client("http://127.0.0.1:1".parse().expect("URL")).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "create_sandbox",
        json!({"account_id": "not-a-uuid"}),
    )
    .await;

    assert_error(&result, "invalid_arguments");
    assert!(text(&result).contains("Invalid arguments for create_sandbox"));
}

#[tokio::test]
async fn snapshots_a_sandbox_machine() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/v1/machines/{machine_id}/snapshots",
            post(snapshot_sandbox_response),
        )
        .with_state(captured.clone());
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let snapshot_id = "019d2aa0-0000-7000-8000-000000000003";
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "snapshot_sandbox",
        json!({"machine_id": "sandbox-created"}),
    )
    .await;

    assert_success(&result);
    assert_eq!(
        serde_json::from_str::<Value>(text(&result)).expect("JSON tool content"),
        json!({
            "status": "successful",
            "snapshotId": snapshot_id
        })
    );
    assert_eq!(
        captured
            .lock()
            .expect("snapshot request capture")
            .as_slice(),
        [json!({
            "machine_id": "sandbox-created",
            "authorization": "Bearer api-token"
        })]
    );
}

#[tokio::test]
async fn returns_sandbox_snapshot_failures_as_tool_errors() {
    let app = Router::new().route(
        "/v1/machines/{machine_id}/snapshots",
        post(|| async {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "code": "disconnected",
                    "message": "sandbox provider could not create a snapshot",
                    "retryable": true
                })),
            )
        }),
    );
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "snapshot_sandbox",
        json!({"machine_id": "sandbox-created"}),
    )
    .await;

    assert_error(&result, "sandbox_snapshot_failed");
    assert!(text(&result).contains("sandbox provider could not create a snapshot"));
}

#[tokio::test]
async fn snapshot_sandbox_rejects_invalid_machine_ids_before_calling_the_gateway() {
    let execution =
        execution_client("http://127.0.0.1:1".parse().expect("URL")).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "snapshot_sandbox",
        json!({"machine_id": ""}),
    )
    .await;

    assert_error(&result, "invalid_arguments");
    assert!(text(&result).contains("Invalid arguments for snapshot_sandbox"));
}

#[tokio::test]
async fn creates_a_tunnel_machine_environment_for_the_configured_project() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/v1/machines/{machine_id}",
            get(|| async {
                Json(machine_summary(
                    "tunnel-online",
                    "Developer laptop",
                    "machine_daemon",
                    true,
                ))
            }),
        )
        .route("/v1/environments", post(create_environment_response))
        .with_state(captured.clone());
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "create_tunnel_machine_environment",
        json!({
            "name": "Development",
            "machine_id": "tunnel-online",
            "path": "projects/example"
        }),
    )
    .await;

    assert_success(&result);
    assert_eq!(
        serde_json::from_str::<Value>(text(&result)).expect("JSON tool content"),
        json!({
            "status": "successful",
            "envId": "environment-created",
            "name": "Development",
            "path": "projects/example",
            "hostName": "Developer laptop",
            "createdAt": 3
        })
    );
    assert_eq!(
        captured
            .lock()
            .expect("environment request capture")
            .as_slice(),
        [json!({
            "authorization": "Bearer api-token",
            "body": {
                "project_id": PROJECT_ID,
                "machine_id": "tunnel-online",
                "name": "Development",
                "workspace_root_id": "root",
                "path": "projects/example"
            }
        })]
    );
}

#[tokio::test]
async fn returns_tunnel_environment_creation_failures_as_tool_errors() {
    let app = Router::new()
        .route(
            "/v1/machines/{machine_id}",
            get(|| async {
                Json(machine_summary(
                    "tunnel-online",
                    "Developer laptop",
                    "machine_daemon",
                    true,
                ))
            }),
        )
        .route(
            "/v1/environments",
            post(|| async {
                (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "code": "already_exists",
                        "message": "an active environment already exists at this machine location",
                        "retryable": false
                    })),
                )
            }),
        );
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "create_tunnel_machine_environment",
        json!({
            "name": "Development",
            "machine_id": "tunnel-online",
            "path": "."
        }),
    )
    .await;

    assert_error(&result, "environment_creation_failed");
    assert!(text(&result).contains("an active environment already exists"));
}

#[tokio::test]
async fn creates_a_sandbox_template_environment_for_the_configured_project() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/v1/sandbox-environment-templates",
            post(create_sandbox_template_environment_response),
        )
        .with_state(captured.clone());
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let snapshot_id = "019d2aa0-0000-7000-8000-000000000020";
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "create_sandbox_template_environment",
        json!({
            "snapshot_id": snapshot_id,
            "name": "Sandbox development",
            "path": "project",
            "setup_script": "pnpm install"
        }),
    )
    .await;

    assert_success(&result);
    assert_eq!(
        serde_json::from_str::<Value>(text(&result)).expect("JSON tool content"),
        json!({
            "status": "successful",
            "envId": "019d2aa0-0000-7000-8000-000000000021",
            "name": "Sandbox development",
            "path": "project",
            "hostName": "E2B main",
            "createdAt": 4,
            "snapshotId": snapshot_id
        })
    );
    assert_eq!(
        captured
            .lock()
            .expect("sandbox template request capture")
            .as_slice(),
        [json!({
            "authorization": "Bearer api-token",
            "body": {
                "project_id": PROJECT_ID,
                "snapshot_id": snapshot_id,
                "name": "Sandbox development",
                "path": "project",
                "creation_script": "pnpm install"
            }
        })]
    );
}

#[tokio::test]
async fn updates_a_tunnel_machine_environment() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/v1/projects/{project_id}/environments",
            get(list_project_environments_response),
        )
        .route(
            "/v1/environments/{environment_id}",
            patch(update_tunnel_environment_response),
        )
        .with_state(captured.clone());
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "update_environment",
        json!({
            "environment_id": "environment-1",
            "name": "Renamed local",
            "path": "projects/renamed"
        }),
    )
    .await;

    assert_success(&result);
    assert_eq!(
        serde_json::from_str::<Value>(text(&result)).expect("JSON tool content"),
        json!({
            "status": "successful",
            "environment": {
                "id": "environment-1",
                "name": "Renamed local",
                "host_name": "Developer laptop",
                "machine_id": "tunnel-online",
                "path": "projects/renamed",
                "type": "env",
                "created_at": 3
            }
        })
    );
    let captured = captured.lock().expect("environment update capture");
    assert_eq!(captured[1]["environment_id"], "environment-1");
    assert_eq!(captured[1]["body"]["project_id"], PROJECT_ID);
    assert_eq!(captured[1]["body"]["name"], "Renamed local");
    assert_eq!(captured[1]["body"]["path"], "projects/renamed");
}

#[tokio::test]
async fn updates_a_sandbox_template_environment() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/v1/projects/{project_id}/environments",
            get(list_project_environments_response),
        )
        .route(
            "/v1/sandbox-environment-templates/{template_id}",
            patch(update_template_environment_response),
        )
        .with_state(captured.clone());
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "update_environment",
        json!({
            "environment_id": "019d2aa0-0000-7000-8000-000000000021",
            "path": "apps/api",
            "setup_script": "pnpm install --frozen-lockfile"
        }),
    )
    .await;

    assert_success(&result);
    assert_eq!(
        serde_json::from_str::<Value>(text(&result)).expect("JSON tool content"),
        json!({
            "status": "successful",
            "environment": {
                "id": "019d2aa0-0000-7000-8000-000000000021",
                "name": "Sandbox development",
                "host_name": "E2B main",
                "path": "apps/api",
                "type": "template",
                "snapshot_id": "019d2aa0-0000-7000-8000-000000000020",
                "setup_script": "pnpm install --frozen-lockfile",
                "created_at": 4
            }
        })
    );
    let captured = captured.lock().expect("template update capture");
    assert_eq!(
        captured[1]["template_id"],
        "019d2aa0-0000-7000-8000-000000000021"
    );
    assert_eq!(captured[1]["body"]["project_id"], PROJECT_ID);
    assert_eq!(captured[1]["body"]["path"], "apps/api");
}

#[tokio::test]
async fn returns_sandbox_template_creation_failures_as_tool_errors() {
    let app = Router::new().route(
        "/v1/sandbox-environment-templates",
        post(|| async {
            (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "code": "not_found",
                    "message": "resource not found",
                    "retryable": false
                })),
            )
        }),
    );
    let execution = execution_client(serve(app).await).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "create_sandbox_template_environment",
        json!({
            "snapshot_id": "019d2aa0-0000-7000-8000-000000000020",
            "name": "Sandbox development",
            "path": "project"
        }),
    )
    .await;

    assert_error(&result, "environment_creation_failed");
    assert!(text(&result).contains("resource not found"));
}

#[tokio::test]
async fn sandbox_template_creation_rejects_invalid_snapshot_ids() {
    let execution =
        execution_client("http://127.0.0.1:1".parse().expect("URL")).expect("execution client");
    let operation = OperationContext::new();
    let result = execute_gateway_tool(
        &execution,
        &operation,
        "create_sandbox_template_environment",
        json!({
            "snapshot_id": "not-a-uuid",
            "name": "Sandbox development",
            "path": "project"
        }),
    )
    .await;

    assert_error(&result, "invalid_arguments");
    assert!(text(&result).contains("Invalid arguments for create_sandbox_template_environment"));
}

#[tokio::test]
async fn executes_write_read_edit_and_bash() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let context = ToolExecutionContext {
        runtime: &fixture.runtime,
        operation: &operation,
    };

    let write = execute(
        &context,
        "write",
        json!({
            "path": "src/content.txt",
            "content": "\u{feff}alpha\r\nbeta\r\ngamma\r\n"
        }),
    )
    .await;
    assert_success(&write);
    assert!(text(&write).contains("Successfully wrote"));

    let read = execute(
        &context,
        "read",
        json!({
            "path": "src/content.txt",
            "offset": 2,
            "limit": 1
        }),
    )
    .await;
    assert_success(&read);
    assert!(text(&read).starts_with("beta"));
    assert!(text(&read).contains("more lines in file"));

    let edit = execute(
        &context,
        "edit",
        json!({
            "path": "src/content.txt",
            "edits": [
                { "oldText": "alpha", "newText": "ALPHA" },
                { "oldText": "gamma", "newText": "GAMMA" }
            ]
        }),
    )
    .await;
    assert_success(&edit);
    assert_eq!(
        tokio::fs::read(fixture.workspace.join("src/content.txt"))
            .await
            .expect("edited file"),
        "\u{feff}ALPHA\r\nbeta\r\nGAMMA\r\n".as_bytes()
    );

    let bash = execute(
        &context,
        "bash",
        json!({ "command": "test -f src/content.txt && printf 'shell output'" }),
    )
    .await;
    assert_success(&bash);
    assert_eq!(text(&bash), "shell output");
}

#[tokio::test]
async fn read_returns_images_as_base64_content() {
    let fixture = Fixture::new().await;
    let image_bytes = b"not-a-decoded-image-but-valid-tool-bytes";
    tokio::fs::write(fixture.workspace.join("image.png"), image_bytes.as_slice())
        .await
        .expect("image fixture");
    let operation = OperationContext::new();
    let context = ToolExecutionContext {
        runtime: &fixture.runtime,
        operation: &operation,
    };

    let result = execute(&context, "read", json!({ "path": "image.png" })).await;
    assert_success(&result);
    let ContentPart::Image(image) = &result.content[1] else {
        panic!("second content part must be an image");
    };
    let ImageSource::Base64(source) = &image.source else {
        panic!("image must be returned inline");
    };
    assert_eq!(source.mime_type, "image/png");
    assert!(!source.data.is_empty());
}

#[tokio::test]
async fn tool_failures_are_returned_to_the_model() {
    let fixture = Fixture::new().await;
    tokio::fs::write(fixture.workspace.join("duplicate.txt"), "same\nsame\n")
        .await
        .expect("duplicate fixture");
    let operation = OperationContext::new();
    let context = ToolExecutionContext {
        runtime: &fixture.runtime,
        operation: &operation,
    };

    let edit = execute(
        &context,
        "edit",
        json!({
            "path": "duplicate.txt",
            "edits": [{ "oldText": "same", "newText": "changed" }]
        }),
    )
    .await;
    assert_error(&edit, "edit_failed");
    assert!(text(&edit).contains("must be unique"));

    let bash = execute(
        &context,
        "bash",
        json!({
            "command": "printf 'before failure'; exit 7"
        }),
    )
    .await;
    assert_error(&bash, "command_failed");
    assert!(text(&bash).contains("before failure"));
    assert!(text(&bash).contains("Command exited with code 7"));

    let malformed = execute(&context, "read", json!({ "offset": 1 })).await;
    assert_error(&malformed, "invalid_arguments");
}

#[tokio::test]
async fn machine_targeting_failures_are_returned_to_the_model() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let context = ToolExecutionContext {
        runtime: &fixture.runtime,
        operation: &operation,
    };

    let missing = execute_raw(&context, "read", json!({"path": "file.txt"})).await;
    assert_error(&missing, "invalid_arguments");
    assert!(text(&missing).contains("machineId"));

    let mismatch = execute_raw(
        &context,
        "read",
        json!({"machineId": "another-machine", "path": "file.txt"}),
    )
    .await;
    assert_error(&mismatch, "machine_mismatch");
}

#[tokio::test]
async fn rejects_a_machine_with_multiple_workspace_roots() {
    let fixture = Fixture::with_root_count(2).await;
    let operation = OperationContext::new();
    let context = ToolExecutionContext {
        runtime: &fixture.runtime,
        operation: &operation,
    };

    let result = execute(&context, "read", json!({"path": "file.txt"})).await;
    assert_error(&result, "ambiguous_workspace_root");
}

#[tokio::test]
async fn resolves_absolute_paths_only_inside_the_active_root() {
    let fixture = Fixture::new().await;
    let file = fixture.workspace.join("absolute.txt");
    tokio::fs::write(&file, "absolute")
        .await
        .expect("absolute fixture");
    let operation = OperationContext::new();
    let context = ToolExecutionContext {
        runtime: &fixture.runtime,
        operation: &operation,
    };

    let result = execute(&context, "read", json!({ "path": file.to_string_lossy() })).await;
    assert_success(&result);
    assert_eq!(text(&result), "absolute");

    let outside = Path::new("/outside-the-active-workspace/file.txt");
    let result = execute(
        &context,
        "read",
        json!({ "path": outside.to_string_lossy() }),
    )
    .await;
    assert_error(&result, "invalid_path");
}

async fn execute(
    context: &ToolExecutionContext<'_>,
    name: &str,
    arguments: Value,
) -> ToolResultMessage {
    let Value::Object(arguments) = arguments else {
        panic!("test arguments must be an object");
    };
    let mut arguments = arguments;
    arguments.insert("machineId".to_owned(), json!("machine"));
    execute_raw(context, name, Value::Object(arguments)).await
}

async fn execute_raw(
    context: &ToolExecutionContext<'_>,
    name: &str,
    arguments: Value,
) -> ToolResultMessage {
    let Value::Object(arguments) = arguments else {
        panic!("test arguments must be an object");
    };
    execute_tool_call_with_runtime(
        &AssistantContent::ToolCall {
            name: name.to_owned(),
            arguments: ToolArguments::Object(arguments),
            tool_call_id: ToolCallId::new(format!("{name}-call")).expect("tool call id"),
        },
        context.runtime,
        context.operation,
    )
    .await
    .expect("assistant tool call")
}

async fn execute_gateway_tool(
    execution: &ExecutionClient,
    operation: &OperationContext,
    name: &str,
    arguments: Value,
) -> ToolResultMessage {
    execute_gateway_tool_with_project(execution, operation, project_id(), name, arguments).await
}

async fn execute_gateway_tool_with_project(
    execution: &ExecutionClient,
    operation: &OperationContext,
    project_id: Uuid,
    name: &str,
    arguments: Value,
) -> ToolResultMessage {
    let Value::Object(arguments) = arguments else {
        panic!("test arguments must be an object");
    };
    execute_tool_call(
        &AssistantContent::ToolCall {
            name: name.to_owned(),
            arguments: ToolArguments::Object(arguments),
            tool_call_id: ToolCallId::new(format!("{name}-gateway-call")).expect("tool call id"),
        },
        &GatewayToolExecutionContext {
            execution,
            operation,
            project_id,
            search: test_search_context(),
            scrape: test_scrape_context(),
        },
    )
    .await
    .expect("assistant tool call")
}

fn project_id() -> Uuid {
    PROJECT_ID.parse().expect("valid project ID")
}

fn test_search_context() -> &'static tool_firecrawl_search::FirecrawlSearchToolContext {
    static CONTEXT: OnceLock<tool_firecrawl_search::FirecrawlSearchToolContext> = OnceLock::new();
    CONTEXT.get_or_init(|| {
        tool_firecrawl_search::FirecrawlSearchToolContext::new("test-key")
            .expect("test search context")
    })
}

fn test_scrape_context() -> &'static tool_firecrawl_scrape::FirecrawlScrapeToolContext {
    static CONTEXT: OnceLock<tool_firecrawl_scrape::FirecrawlScrapeToolContext> = OnceLock::new();
    CONTEXT.get_or_init(|| {
        tool_firecrawl_scrape::FirecrawlScrapeToolContext::new("test-key")
            .expect("test scrape context")
    })
}

fn text(message: &ToolResultMessage) -> &str {
    let ContentPart::Text(content) = &message.content[0] else {
        panic!("first content part must be text");
    };
    &content.content
}

fn assert_success(message: &ToolResultMessage) {
    assert_eq!(message.outcome, ToolResultOutcome::Success);
}

fn assert_error(message: &ToolResultMessage, expected_name: &str) {
    let ToolResultOutcome::Error { error } = &message.outcome else {
        panic!("tool result must be an error");
    };
    assert_eq!(error.name.as_deref(), Some(expected_name));
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid identifier")
}

fn machine_summary(machine_id: &str, name: &str, connector: &str, online: bool) -> Value {
    json!({
        "machine_id": machine_id,
        "name": name,
        "connector": connector,
        "online": online,
        "descriptor": {
            "protocol_version": {"major": 1, "minor": 0},
            "machine_id": machine_id,
            "name": name,
            "operating_system": {"type": "linux"},
            "architecture": "aarch64",
            "path_convention": "posix",
            "workspace_roots": [{
                "id": "root",
                "name": "Project",
                "uri": "file:///workspace/project",
                "read_only": false
            }],
            "capabilities": []
        },
        "created_at": 1,
        "updated_at": 2,
        "last_seen_at": 2
    })
}

async fn create_sandbox_response(
    State(captured): State<Arc<Mutex<Vec<Value>>>>,
    AxumPath(account_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    captured
        .lock()
        .expect("sandbox request capture")
        .push(json!({
            "account_id": account_id,
            "authorization": headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            "body": body
        }));
    (
        StatusCode::CREATED,
        Json(json!({
            "machine": machine_summary(
                "sandbox-created",
                "Created sandbox",
                "sandbox",
                true
            )
        })),
    )
}

async fn list_project_environments_response(
    State(captured): State<Arc<Mutex<Vec<Value>>>>,
    AxumPath(project_id): AxumPath<String>,
    headers: HeaderMap,
) -> Json<Value> {
    captured
        .lock()
        .expect("environment list request capture")
        .push(json!({
            "project_id": project_id,
            "authorization": headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
        }));
    Json(json!([
        {
            "id": "environment-1",
            "name": "Local development",
            "host_name": "Developer laptop",
            "machine_id": "tunnel-online",
            "path": "projects/example",
            "type": "env",
            "created_at": 3
        },
        {
            "id": "019d2aa0-0000-7000-8000-000000000021",
            "name": "Sandbox development",
            "host_name": "E2B main",
            "path": "project",
            "type": "template",
            "snapshot_id": "019d2aa0-0000-7000-8000-000000000020",
            "setup_script": "pnpm install",
            "created_at": 4
        }
    ]))
}

async fn snapshot_sandbox_response(
    State(captured): State<Arc<Mutex<Vec<Value>>>>,
    AxumPath(machine_id): AxumPath<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    captured
        .lock()
        .expect("snapshot request capture")
        .push(json!({
            "machine_id": machine_id,
            "authorization": headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
        }));
    (
        StatusCode::CREATED,
        Json(json!({
            "snapshot_id": "019d2aa0-0000-7000-8000-000000000003"
        })),
    )
}

async fn create_environment_response(
    State(captured): State<Arc<Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    captured
        .lock()
        .expect("environment request capture")
        .push(json!({
            "authorization": headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            "body": body
        }));
    (
        StatusCode::CREATED,
        Json(json!({
            "environment_id": "environment-created",
            "project_id": PROJECT_ID,
            "machine_id": "tunnel-online",
            "name": "Development",
            "workspace_root_id": "root",
            "path": "projects/example",
            "created_at": 3
        })),
    )
}

async fn create_sandbox_template_environment_response(
    State(captured): State<Arc<Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    captured
        .lock()
        .expect("sandbox template request capture")
        .push(json!({
            "authorization": headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            "body": body
        }));
    (
        StatusCode::CREATED,
        Json(json!({
            "id": "019d2aa0-0000-7000-8000-000000000021",
            "name": "Sandbox development",
            "host_name": "E2B main",
            "path": "project",
            "type": "template",
            "snapshot_id": "019d2aa0-0000-7000-8000-000000000020",
            "setup_script": "pnpm install",
            "created_at": 4
        })),
    )
}

async fn update_tunnel_environment_response(
    State(captured): State<Arc<Mutex<Vec<Value>>>>,
    AxumPath(environment_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    captured
        .lock()
        .expect("environment update request capture")
        .push(json!({
            "environment_id": environment_id,
            "authorization": headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            "body": body
        }));
    Json(json!({
        "id": "environment-1",
        "name": "Renamed local",
        "host_name": "Developer laptop",
        "machine_id": "tunnel-online",
        "path": "projects/renamed",
        "type": "env",
        "created_at": 3
    }))
}

async fn update_template_environment_response(
    State(captured): State<Arc<Mutex<Vec<Value>>>>,
    AxumPath(template_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    captured
        .lock()
        .expect("template update request capture")
        .push(json!({
            "template_id": template_id,
            "authorization": headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            "body": body
        }));
    Json(json!({
        "id": "019d2aa0-0000-7000-8000-000000000021",
        "name": "Sandbox development",
        "host_name": "E2B main",
        "path": "apps/api",
        "type": "template",
        "snapshot_id": "019d2aa0-0000-7000-8000-000000000020",
        "setup_script": "pnpm install --frozen-lockfile",
        "created_at": 4
    }))
}

fn execution_client(base_url: Url) -> Result<ExecutionClient, ExecutionGatewayClientError> {
    let mut config = ExecutionGatewayConfig::new(base_url, "api-token");
    config.request_timeout = Duration::from_secs(2);
    ExecutionClient::new(config)
}

async fn serve(app: Router) -> Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let address = listener.local_addr().expect("test server address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve test app");
    });
    format!("http://{address}").parse().expect("server URL")
}

use super::*;
use llm_contracts::{ToolArguments, ToolDefinition};
use serde_json::{Value, json};
#[test]
fn public_site_rejection_details_are_explicit_without_changing_transport_logs() {
    let error = platform_runtime_client::ServerError {
        status: 409,
        error: Some(platform_runtime_client::types::ErrorInfo {
            code: "RUNTIME_CONFLICT".into(),
            message:
                "Site provisioning is still pending; retry the same operation with the same key."
                    .into(),
        }),
    };
    assert!(!error.to_string().contains("provisioning"));
    let rejected = tools::site_rejection(&error);
    assert!(rejected.message.contains("provisioning"));
    assert!(rejected.message.contains("same key"));
}
#[test]
fn configuration_is_site_bound_without_environment_requirements() {
    let config = json!({"model":{"provider":"openai","id":"gpt-5.6-terra"},"reasoning_level":"high","siteId":null});
    assert!(Config::parse(config.as_object().unwrap()).is_ok());
    for field in ["environment", "workspace", "expectedVersion"] {
        let mut invalid = config.clone();
        invalid[field] = json!("not supported");
        assert!(Config::parse(invalid.as_object().unwrap()).is_err());
    }
    let mut invalid = config.clone();
    invalid["siteId"] = json!("not-a-uuid");
    assert!(Config::parse(invalid.as_object().unwrap()).is_err());
    invalid = config;
    invalid["model"]["id"] = json!("unknown-model");
    assert!(Config::parse(invalid.as_object().unwrap()).is_err());
}
#[test]
fn prompt_uses_one_instructions_file_with_optional_session_append() {
    let main = include_str!("system_prompt.md");
    assert_eq!(prompt::generate(None), main);
    assert_eq!(
        prompt::generate(Some("Use the project style guide.")),
        format!("{main}\nAdditional session instructions:\nUse the project style guide.")
    );
}
#[test]
fn main_prompt_explains_the_role_and_site_scope() {
    let main = include_str!("system_prompt.md");
    assert!(main.starts_with("## Your role and what Sites are for\n"));
    assert_eq!(
        main.lines().filter(|line| line.starts_with("## ")).count(),
        10
    );
    for concept in [
        "custom web applications",
        "not every Site needs them",
        "Your authoring tools automatically target it.",
        "`tools.metadata()` inside `exec`",
        "project's allowed environments and harnesses",
        "Build a functioning application, not just a visual mockup.",
        "## The Platform capabilities your application can use",
        "`ctx.platform.execution.listResources()`",
        "`sandbox_accounts`",
        "execution access enabled",
        "## What a Site consists of and where its code runs",
        "### Three separate JavaScript contexts",
        "each must stay within 48 KiB",
        "The database is independent of code releases.",
        "## Build the frontend",
        "Native form submission is disabled in the Site frame.",
        "A `submit` handler with `preventDefault()` is not sufficient here.",
        "clean, minimal interface with clear hierarchy and concise text",
        "avoid redundant descriptions, filler copy",
        "Verify the frontend with `tools.browser` inside `exec`",
        "a screenshot alone does not verify that an interaction works",
        "## Build the backend and persistent data model",
        "ctx.db.transaction<T>",
        "Returning an error response does not automatically roll them back.",
        "## Use Platform operations from the backend",
        "networkAccess?: boolean | null",
        "ctx.platform.sandboxes.get(",
        "The SDK has no manual sandbox termination method.",
        "outside the harness-managed workflow",
        "## Design long-running work, continuation, and recovery",
        "A simple CRUD application does not need a workflow engine.",
        "Both initial submission and recovery must load that record through the same path",
        "Keep UI/status projections separate from the record used to retry work.",
        "async function submitSavedSession(ctx, operationId)",
        "dispatching a synthetic `submit` event does not verify",
        "wait for a relevant observable condition with a bounded deadline",
        "Task creation not confirmed: ",
        "does not prove interrupted-submission recovery",
        "ctx.invocation.eventId !== event.id",
        "A duplicate delivery must also reconcile any unfinished work",
        "After 12 unsuccessful attempts, delivery is marked failed.",
        "It does not resume a suspended JavaScript stack.",
        "## Use exec and wait to build the Site",
        "declare function store(key: string, value: Json): void;",
        "declare function load(key: string): Json | undefined;",
        "max_output_tokens?: number;",
        "max_tokens?: number;",
        "Do not resubmit the original JavaScript to collect its result.",
        "## The six authoring tools",
        "### metadata: inspect the current Site",
        "### read: read one source file",
        "### apply_patch: edit and activate source",
        "### sql: inspect and modify persistent data",
        "### invoke: test a backend endpoint",
        "### browser: inspect and exercise the frontend",
        "Success returns exactly `{}`.",
        "`response.status` is the application's HTTP-style response status.",
        "After applying frontend or backend changes, explicitly reload",
        "## Build, verify, and hand off",
        "preserve unrelated user changes",
        "Calling an endpoint directly does not verify that the interface calls it correctly.",
        "a simple CRUD Site does not need orchestration tests.",
        "Distinguish tested behavior from assumptions.",
    ] {
        assert!(main.contains(concept), "Missing {concept}");
    }
    assert!(!main.contains("ctx.platform.sandboxes.terminate"));
}
#[test]
fn tool_plans_validate_raw_exec_and_cell_wait() {
    let args = |v: Value| ToolArguments::Object(v.as_object().unwrap().clone());
    let plan = tools::prepare("exec", &ToolArguments::String("text(1)".into())).unwrap();
    let encoded = serde_json::to_value(plan).unwrap();
    let decoded: tools::Plan = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
    assert!(
        tools::prepare(
            "exec",
            &args(json!({"source":"return 1","id":"model-selected"}))
        )
        .is_err()
    );
    assert!(tools::prepare("wait", &args(json!({"seconds":0}))).is_err());
    assert!(tools::prepare("wait", &args(json!({"seconds":3601}))).is_err());
    let plan = tools::prepare(
        "wait",
        &args(json!({"cell_id":"live-cell","terminate":true})),
    )
    .unwrap();
    assert!(
        matches!(plan,tools::Plan::Wait {input} if input.cell_id == "live-cell" && input.terminate && input.yield_time_ms == 10_000)
    );
    assert!(tools::prepare("inspect_cell", &args(json!({"id":"invalid"}))).is_err());
    assert!(tools::prepare("code_mode", &args(json!({"source":"text(1)"}))).is_err());
    assert!(tools::prepare("reconcile_call", &args(json!({}))).is_err());
}
#[test]
fn registry_validates_the_six_tool_inputs() {
    let mut registry = tool_code_mode::Registry::default();
    tools::register(&mut registry).unwrap();
    let names: Vec<_> = registry.tools().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "apply_patch",
            "browser",
            "invoke",
            "metadata",
            "read",
            "sql"
        ]
    );
    for (name, input) in [
        ("metadata", json!({})),
        (
            "read",
            json!({"file_path":"backend.js","offset":2,"limit":3}),
        ),
        (
            "apply_patch",
            json!("*** Begin Patch\n*** Update File: index.html\n@@\n-a\n+b\n*** End Patch"),
        ),
        (
            "invoke",
            json!({"method":"POST","path":"/items","body":{"name":"example"}}),
        ),
        (
            "browser",
            json!({"action":"evaluate","code":"return document.title"}),
        ),
        ("browser", json!({"action":"screenshot"})),
        ("browser", json!({"action":"reload"})),
        ("sql", json!({"sql":"select ?","params":[7]})),
    ] {
        assert!(
            jsonschema::draft202012::is_valid(&registry.get(name).unwrap().input_schema, &input),
            "{name}"
        );
    }
    for (name, input) in [
        ("read", json!({"file_path":"other.js"})),
        ("read", json!({"file_path":"index.html","offset":0})),
        ("apply_patch", json!({"patch":"not raw"})),
        ("metadata", json!({"site_id":"another"})),
        ("browser", json!({"code":"return 1"})),
        ("browser", json!({"action":"evaluate"})),
        ("browser", json!({"action":"evaluate","code":""})),
        ("browser", json!({"action":"screenshot","code":"return 1"})),
        (
            "browser",
            json!({"action":"reload","url":"https://example.com"}),
        ),
        ("browser", json!({"action":"click"})),
        ("sql", json!({"sql":"select ?","params":[true]})),
    ] {
        assert!(
            !jsonschema::draft202012::is_valid(&registry.get(name).unwrap().input_schema, &input),
            "{name}"
        );
    }
}

#[test]
fn read_returns_only_requested_source_window_and_content_revision() {
    let source = json!({"files":{"frontend":"a\r\nb\r\nc", "backend":"private backend"}});
    let input = tool_read::ReadInput {
        file_path: "index.html".into(),
        offset: Some(2),
        limit: Some(1),
    };
    let result = tools::read_source(&source, &input).unwrap();
    assert_eq!(result["content"], "b\r\n");
    assert_eq!(result["start_line"], 2);
    assert_eq!(result["end_line"], 2);
    assert_eq!(result["next_offset"], 3);
    assert_eq!(result["eof"], false);
    assert_eq!(result["truncation"], "line_limit");
    assert!(!result.to_string().contains("private backend"));
    let mut changed = source;
    changed["files"]["frontend"] = json!("a\r\nb\r\nd");
    assert_ne!(
        tools::read_source(&changed, &input).unwrap()["revision"],
        result["revision"]
    );
}

#[test]
fn invocation_preserves_http_errors_and_normalizes_diagnostics() {
    let result = tools::invocation_result(json!({
        "id":"invocation", "status":"succeeded", "response":{"status":400,"body":{"error":"bad input"}},
        "error_code":null,"logs":[],"responseTruncated":false
    }));
    assert_eq!(
        result,
        json!({
            "invocation_id":"invocation","status":"succeeded","response":{"status":400,"body":{"error":"bad input"}},
            "error":null,"logs":[],"response_truncated":false,"logs_truncated":false
        })
    );
}

#[test]
fn outside_tools_have_only_raw_exec_and_wait_contracts() {
    let definitions = tools::definitions();
    assert_eq!(definitions.len(), 2);
    let ToolDefinition::Custom(exec) = &definitions[0] else {
        panic!("exec must be raw, not a JSON function")
    };
    assert_eq!(exec.name, "exec");
    assert!(matches!(
        exec.format.syntax,
        llm_contracts::GrammarSyntax::Lark
    ));
    assert_eq!(
        exec.format.definition,
        tool_code_mode::live::protocol::EXEC_GRAMMAR
    );
    let ToolDefinition::Function(wait) = &definitions[1] else {
        panic!("wait must be a JSON function")
    };
    assert_eq!(wait.name, "wait");
    assert_eq!(wait.parameters["required"], json!(["cell_id"]));
    assert_eq!(wait.parameters["additionalProperties"], false);
    let properties = wait.parameters["properties"].as_object().unwrap();
    assert_eq!(properties.len(), 4);
    for (name, kind) in [
        ("cell_id", "string"),
        ("yield_time_ms", "number"),
        ("max_tokens", "number"),
        ("terminate", "boolean"),
    ] {
        assert_eq!(properties[name]["type"], kind);
    }
    assert!(wait.output_schema.is_none());
    assert!(!supported_models().contains_key("fireworks"));
}

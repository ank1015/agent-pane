use super::*;
use llm_contracts::{ToolArguments, ToolDefinition};
use serde_json::{Value, json};
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
fn prompt_explains_every_shared_capability_and_optional_tools_honestly() {
    let prompt = prompt::generate(None, false, false);
    for method in platform_javascript_sdk::methods() {
        let name = method.name.split('.').next_back().unwrap();
        assert!(prompt.contains(name), "Missing {}", method.name);
    }
    for concept in [
        "Environments and execution",
        "Harnesses and accounts",
        "Sessions",
        "Runs and waiting",
        "Sites and your authoring workflow",
        "no expectedVersion",
        "never undo database",
        "never reruns the JavaScript",
        "window.callBackend",
        "Backend handler SDK reference",
    ] {
        assert!(prompt.contains(concept), "Missing {concept}");
    }
    assert!(prompt.contains("Browser verification is not configured"));
    let enabled = prompt::generate(Some("Use the project style guide."), true, true);
    assert!(enabled.contains("tools['web.scrape']"));
    assert!(enabled.ends_with("Use the project style guide."));
    assert!(
        enabled.len() < 256 * 1024,
        "Frozen prompt must fit bounded checkpoint"
    );
}
#[test]
fn tool_plans_validate_and_preserve_cell_identity_and_absolute_wait_time() {
    let args = |v: Value| ToolArguments::Object(v.as_object().unwrap().clone());
    let plan = tools::prepare("code_mode", &args(json!({"source":"return 1"}))).unwrap();
    let encoded = serde_json::to_value(plan).unwrap();
    let decoded: tools::Plan = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
    assert!(
        tools::prepare(
            "code_mode",
            &args(json!({"source":"return 1","id":"model-selected"}))
        )
        .is_err()
    );
    assert!(tools::prepare("wait", &args(json!({"seconds":0}))).is_err());
    assert!(tools::prepare("wait", &args(json!({"seconds":3601}))).is_err());
    let plan = tools::prepare("wait", &args(json!({"seconds":20}))).unwrap();
    assert!(matches!(plan,tools::Plan::Wait {wake_at,..} if wake_at>chrono::Utc::now()));
    assert!(tools::prepare("inspect_cell", &args(json!({"id":"invalid"}))).is_err());
}
#[test]
fn optional_registry_tools_are_reads_and_snapshot_tools_are_absent() {
    use platform_agent_code_mode::code_mode::{Effect, Registry};
    let mut registry = Registry::default();
    tools::register(&mut registry, true, true).unwrap();
    assert_eq!(registry.tools().count(), 3);
    assert!(registry.tools().all(|t| t.effect == Effect::Read));
    for tool in tools::definitions() {
        if let ToolDefinition::Function(f) = tool {
            assert!(!f.name.contains("snapshot"));
        }
    }
    let mut absent = Registry::default();
    tools::register(&mut absent, false, false).unwrap();
    assert_eq!(absent.tools().count(), 0);
}

#[test]
fn oversized_trace_retains_reconciliation_identity_and_outcome() {
    let id = uuid::Uuid::now_v7();
    let call = json!({"cell_id":id,"sequence":3,"tool":"sites.execute","operation_key":"saved-receipt","status":"uncertain","result":"x".repeat(200*1024)});
    let output = tools::Output::success(
        json!({"cell":{"id":id,"status":"interrupted"},"calls":{"items":[{"value":call.clone()}],"next_cursor":"next"}}),
    );
    let result: Value = serde_json::from_str(&output.text).unwrap();
    assert_eq!(result["cellId"], json!(id));
    assert_eq!(result["calls"][0]["sequence"], 3);
    assert_eq!(result["calls"][0]["status"], "uncertain");
    assert_eq!(result["next_cursor"], "next");
    let output = tools::Output::success(call);
    let result: Value = serde_json::from_str(&output.text).unwrap();
    assert_eq!(result["call"]["operationKey"], "saved-receipt");
    assert!(output.text.len() < 4096);
}

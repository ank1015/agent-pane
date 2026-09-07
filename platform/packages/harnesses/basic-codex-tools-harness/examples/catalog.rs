fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "name":"Basic Codex Tools Harness",
            "description":"Durable exec_command/write_stdin/apply_patch/view_image coding loop",
            "default_config":{"reasoning_level":"high"},
            "config_schema":basic_codex_tools_harness::config_schema(),
            "supported_models":basic_codex_tools_harness::supported_models(),
            "enabled":true
        }))
        .unwrap()
    );
}

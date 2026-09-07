fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "name":"Unified Exec Only Harness",
            "description":"Durable shell-only coding loop using exec_command and write_stdin",
            "default_config":{"reasoning_level":"high"},
            "config_schema":unified_exec_only_harness::config_schema(),
            "supported_models":unified_exec_only_harness::supported_models(),
            "enabled":true
        }))
        .unwrap()
    );
}

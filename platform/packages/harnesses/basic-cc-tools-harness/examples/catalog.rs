fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "name":"Basic CC Tools Harness",
            "description":"Sequential read/write/edit/bash coding loop without compaction",
            "default_config":{"reasoning_level":"high"},
            "config_schema":basic_cc_tools_harness::config_schema(),
            "enabled":true
        }))
        .unwrap()
    );
}

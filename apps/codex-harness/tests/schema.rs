use std::collections::BTreeSet;

const TABLES: [&str; 2] = ["codex_code_mode_state", "codex_exec_sessions"];

#[test]
fn tool_state_migration_declares_only_required_durable_state() {
    let migration = include_str!("../migrations/20260830000000_create_codex_tool_state.sql");
    let declared = migration
        .lines()
        .filter_map(|line| line.strip_prefix("create table "))
        .filter_map(|line| line.strip_suffix(" ("))
        .collect::<BTreeSet<_>>();
    let expected = TABLES.into_iter().collect::<BTreeSet<_>>();

    assert_eq!(declared, expected);
    for deliberately_absent in [
        "codex_tool_calls",
        "codex_tool_call_journal",
        "codex_code_mode_cells",
        "codex_live_processes",
    ] {
        assert!(!declared.contains(deliberately_absent));
    }
}

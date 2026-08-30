use std::collections::HashMap;

use codex_code_mode_runtime::CodeModeStateStore;
use codex_harness::{
    config::DatabaseConfig,
    persistence::{Database, PostgresCodeModeStateStore, PostgresExecSessionStore},
};
use execution_contracts::{ExecutionId, MachineId};
use tool_codex_unified_exec::CodexExecSessionStore;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires CODEX_HARNESS_TEST_DATABASE_URL pointing to an empty PostgreSQL database"]
async fn postgres_round_trips_only_durable_tool_state() {
    let database_url = std::env::var("CODEX_HARNESS_TEST_DATABASE_URL")
        .expect("CODEX_HARNESS_TEST_DATABASE_URL must be set");
    let database = Database::connect(&DatabaseConfig {
        url: database_url,
        max_connections: 2,
        acquire_timeout: std::time::Duration::from_secs(5),
    })
    .await
    .expect("database");
    database.migrate().await.expect("migrations");

    let agent_session_id = Uuid::now_v7();
    let execution_id =
        ExecutionId::new(format!("execution-{}", Uuid::now_v7())).expect("execution ID");
    let machine_id = MachineId::new("machine-a").expect("machine ID");
    let first_exec = PostgresExecSessionStore::new(
        database.pool().clone(),
        agent_session_id,
        machine_id.clone(),
    );
    let inserted = first_exec
        .insert(execution_id.clone(), true)
        .await
        .expect("insert exec session");
    let public_session_id = inserted.session_id();
    first_exec
        .update_last_sequence(public_session_id, &execution_id, 41)
        .await
        .expect("persist cursor");
    let persisted_cursor: i64 = sqlx::query_scalar(
        "select last_sequence from codex_exec_sessions where public_session_id = $1",
    )
    .bind(public_session_id)
    .fetch_one(database.pool())
    .await
    .expect("persisted cursor");
    assert_eq!(persisted_cursor, 41);

    let reloaded =
        PostgresExecSessionStore::new(database.pool().clone(), agent_session_id, machine_id)
            .get(public_session_id)
            .await
            .expect("load exec session")
            .expect("exec session exists");
    assert_eq!(reloaded.execution_id(), &execution_id);
    assert!(reloaded.tty());
    first_exec
        .remove(public_session_id, &execution_id)
        .await
        .expect("remove exec session");
    assert!(
        first_exec
            .get(public_session_id)
            .await
            .expect("missing exec lookup")
            .is_none()
    );

    let first_code = PostgresCodeModeStateStore::new(database.pool().clone(), agent_session_id);
    assert_eq!(
        first_code
            .allocate_cell_id()
            .await
            .expect("first cell")
            .as_str(),
        "1"
    );
    first_code
        .commit_values(HashMap::from([(
            "answer".to_owned(),
            serde_json::json!({ "value": 42 }),
        )]))
        .await
        .expect("commit values");

    let reloaded_code = PostgresCodeModeStateStore::new(database.pool().clone(), agent_session_id);
    assert_eq!(
        reloaded_code
            .allocate_cell_id()
            .await
            .expect("second cell")
            .as_str(),
        "2"
    );
    assert_eq!(
        reloaded_code.load_values().await.expect("load values")["answer"],
        serde_json::json!({ "value": 42 })
    );

    sqlx::query("delete from codex_code_mode_state where agent_session_id = $1")
        .bind(agent_session_id)
        .execute(database.pool())
        .await
        .expect("delete code state fixture");
    database.close().await;
}

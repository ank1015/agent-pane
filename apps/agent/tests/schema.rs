use std::collections::BTreeSet;

use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const TABLES: [&str; 9] = [
    "broker_inbox",
    "broker_outbox",
    "harness_revisions",
    "harnesses",
    "run_aborts",
    "run_waits",
    "runs",
    "session_messages",
    "sessions",
];

#[test]
fn core_migration_declares_only_the_rethought_tables() {
    let migration = include_str!("../migrations/20260824000000_create_agent_core.sql");
    let declared = migration
        .lines()
        .filter_map(|line| line.strip_prefix("create table "))
        .filter_map(|line| line.strip_suffix(" ("))
        .collect::<BTreeSet<_>>();
    let expected = TABLES.into_iter().collect::<BTreeSet<_>>();

    assert_eq!(declared, expected);
    for removed in [
        "api_idempotency_records",
        "steers",
        "turn_attempts",
        "turns",
    ] {
        assert!(!declared.contains(removed));
    }
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL pointing to an empty PostgreSQL database"]
async fn migration_builds_the_schema_and_enforces_aggregate_boundaries() {
    let database_url =
        std::env::var("AGENT_TEST_DATABASE_URL").expect("AGENT_TEST_DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("test database connection");
    agent::migrate(&pool).await.expect("agent migrations");

    let actual = sqlx::query_scalar::<_, String>(
        "select table_name::text
         from information_schema.tables
         where table_schema = 'public' and table_type = 'BASE TABLE'",
    )
    .fetch_all(&pool)
    .await
    .expect("schema tables")
    .into_iter()
    .filter(|table| TABLES.contains(&table.as_str()))
    .collect::<BTreeSet<_>>();
    let expected = TABLES
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);
    let archived_at_exists: bool = sqlx::query_scalar(
        "select exists(
             select 1 from information_schema.columns
             where table_schema = 'public' and table_name = 'sessions'
               and column_name = 'archived_at'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("sessions archived_at inspection");
    assert!(!archived_at_exists);

    let harness_id = format!("harness-{}", Uuid::now_v7());
    let revision_id = format!("revision-{}", Uuid::now_v7());
    let session_a = Uuid::now_v7();
    let session_b = Uuid::now_v7();
    let trigger_a = Uuid::now_v7();
    let trigger_b = Uuid::now_v7();
    let run_id = Uuid::now_v7();
    let mut transaction = pool.begin().await.expect("fixture transaction");

    sqlx::query(
        "insert into harnesses (harness_id, slug, display_name)
         values ($1, $2, 'Schema fixture')",
    )
    .bind(&harness_id)
    .bind(format!("schema-fixture-{}", Uuid::now_v7()))
    .execute(&mut *transaction)
    .await
    .expect("harness fixture");
    sqlx::query(
        "insert into harness_revisions
             (harness_revision_id, harness_id, revision, contract_version)
         values ($1, $2, '1', 1)",
    )
    .bind(&revision_id)
    .bind(&harness_id)
    .execute(&mut *transaction)
    .await
    .expect("revision fixture");
    sqlx::query("update harnesses set active_revision_id = $2 where harness_id = $1")
        .bind(&harness_id)
        .bind(&revision_id)
        .execute(&mut *transaction)
        .await
        .expect("activate revision fixture");
    sqlx::query(
        "insert into sessions (session_id, current_revision)
         values ($1, 1), ($2, 1)",
    )
    .bind(session_a)
    .bind(session_b)
    .execute(&mut *transaction)
    .await
    .expect("session fixtures");
    sqlx::query(
        "insert into session_messages
             (session_message_id, session_id, revision, message, origin, state, committed_at)
         values
             ($1, $2, 1, '{\"role\":\"user\"}'::jsonb, 'external', 'committed', now()),
             ($3, $4, 1, '{\"role\":\"user\"}'::jsonb, 'external', 'committed', now())",
    )
    .bind(trigger_a)
    .bind(session_a)
    .bind(trigger_b)
    .bind(session_b)
    .execute(&mut *transaction)
    .await
    .expect("trigger message fixtures");
    sqlx::query(
        "insert into runs
             (run_id, session_id, trigger_message_id, harness_revision_id,
              max_turns)
         values ($1, $2, $3, $4, 10)",
    )
    .bind(run_id)
    .bind(session_a)
    .bind(trigger_a)
    .bind(&revision_id)
    .execute(&mut *transaction)
    .await
    .expect("run fixture");

    sqlx::query("savepoint invalid_lineage")
        .execute(&mut *transaction)
        .await
        .expect("lineage savepoint");
    let invalid_lineage = sqlx::query(
        "insert into session_messages
             (session_message_id, session_id, revision, message, origin, state,
              run_id, turn_number, committed_at)
         values
             ($1, $2, 2, '{\"role\":\"assistant\"}'::jsonb, 'harness', 'committed',
              $3, 1, now())",
    )
    .bind(Uuid::now_v7())
    .bind(session_b)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .expect_err("a message cannot claim a run from another session");
    assert_eq!(
        invalid_lineage
            .as_database_error()
            .and_then(|error| error.code()),
        Some(std::borrow::Cow::Borrowed("23503"))
    );
    sqlx::query("rollback to savepoint invalid_lineage")
        .execute(&mut *transaction)
        .await
        .expect("restore after lineage failure");

    let second_trigger = Uuid::now_v7();
    sqlx::query(
        "insert into session_messages
             (session_message_id, session_id, revision, message, origin, state, committed_at)
         values ($1, $2, 2, '{\"role\":\"user\"}'::jsonb, 'external', 'committed', now())",
    )
    .bind(second_trigger)
    .bind(session_a)
    .execute(&mut *transaction)
    .await
    .expect("second trigger fixture");
    sqlx::query("savepoint second_active_run")
        .execute(&mut *transaction)
        .await
        .expect("active run savepoint");
    let second_active_run = sqlx::query(
        "insert into runs
             (run_id, session_id, trigger_message_id, harness_revision_id,
              max_turns)
         values ($1, $2, $3, $4, 10)",
    )
    .bind(Uuid::now_v7())
    .bind(session_a)
    .bind(second_trigger)
    .bind(&revision_id)
    .execute(&mut *transaction)
    .await
    .expect_err("a session cannot have two active runs");
    assert_eq!(
        second_active_run
            .as_database_error()
            .and_then(|error| error.code()),
        Some(std::borrow::Cow::Borrowed("23505"))
    );
    sqlx::query("rollback to savepoint second_active_run")
        .execute(&mut *transaction)
        .await
        .expect("restore after active run failure");

    let steer_id = Uuid::now_v7();
    sqlx::query(
        "insert into session_messages
             (session_message_id, session_id, message, origin, delivery, state,
              run_id, queued_during_turn, queue_sequence)
         values
             ($1, $2, '{\"role\":\"user\"}'::jsonb, 'external', 'next_turn', 'pending',
              $3, 1, 1)",
    )
    .bind(steer_id)
    .bind(session_a)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .expect("pending steer fixture");
    let canonical: bool = sqlx::query_scalar(
        "select exists(
            select 1 from session_messages
            where session_message_id = $1 and revision is not null
         )",
    )
    .bind(steer_id)
    .fetch_one(&mut *transaction)
    .await
    .expect("canonical visibility query");
    assert!(!canonical);

    sqlx::query("savepoint invalid_pending_message")
        .execute(&mut *transaction)
        .await
        .expect("pending message savepoint");
    let invalid_pending = sqlx::query(
        "insert into session_messages
             (session_message_id, session_id, revision, message, origin, delivery, state,
              run_id, queued_during_turn, queue_sequence)
         values
             ($1, $2, 3, '{\"role\":\"user\"}'::jsonb, 'external', 'next_turn', 'pending',
              $3, 1, 2)",
    )
    .bind(Uuid::now_v7())
    .bind(session_a)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .expect_err("a pending steer cannot enter the transcript");
    assert_eq!(
        invalid_pending
            .as_database_error()
            .and_then(|error| error.code()),
        Some(std::borrow::Cow::Borrowed("23514"))
    );
    sqlx::query("rollback to savepoint invalid_pending_message")
        .execute(&mut *transaction)
        .await
        .expect("restore after pending message failure");

    transaction.rollback().await.expect("fixture rollback");
}

//! Storage-contract tests. Every test migrates its own disposable SQLx database.
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

struct Fixture {
    project: Uuid,
    session: Uuid,
    run: Uuid,
    worker: Uuid,
}

async fn fixture(pool: &PgPool) -> Fixture {
    fixture_version(pool, true).await
}
async fn fixture_version(pool: &PgPool, has_session_config: bool) -> Fixture {
    let f = Fixture {
        project: Uuid::new_v4(),
        session: Uuid::new_v4(),
        run: Uuid::new_v4(),
        worker: Uuid::new_v4(),
    };
    sqlx::query("insert into projects (project_id, name) values ($1, 'Runtime test')")
        .bind(f.project)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("insert into harnesses (id, name) values ('test', 'Test') on conflict do nothing")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("insert into workers (id, build_id, supported_harnesses, capacity) values ($1, 'test-build', array['test'], 4)")
        .bind(f.worker).execute(pool).await.unwrap();
    sqlx::query(if has_session_config {
        "insert into sessions (id, project_id, harness_id,config) values ($1, $2, 'test','{}')"
    } else {
        "insert into sessions (id, project_id, harness_id) values ($1, $2, 'test')"
    })
    .bind(f.session)
    .bind(f.project)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("insert into runs (id, project_id, session_id) values ($1, $2, $3)")
        .bind(f.run)
        .bind(f.project)
        .bind(f.session)
        .execute(pool)
        .await
        .unwrap();
    f
}

fn rejected<T: std::fmt::Debug>(result: Result<T, sqlx::Error>, code: &str) {
    let error = result.expect_err("invalid write unexpectedly succeeded");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some(code),
        "{error}"
    );
}

async fn claim(pool: &PgPool, f: &Fixture) {
    sqlx::query("update runs set status='running', available_at=null, worker_id=$2, lease_epoch=lease_epoch+1, version=version+1, lease_expires_at=clock_timestamp()+interval '1 hour', started_at=coalesce(started_at,clock_timestamp()) where id=$1")
        .bind(f.run).bind(f.worker).execute(pool).await.unwrap();
}

async fn message(pool: &PgPool, f: &Fixture, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "insert into messages (id, project_id, origin_run_id, message) values ($1,$2,$3,$4::jsonb)",
    )
    .bind(id)
    .bind(f.project)
    .bind(f.run)
    .bind(body)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("insert into session_messages (project_id,session_id,message_id,run_id) values ($1,$2,$3,$4)")
        .bind(f.project).bind(f.session).bind(id).bind(f.run).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    id
}

async fn event(tx: &mut Transaction<'_, Postgres>, run: Uuid, kind: &str) {
    sqlx::query("insert into run_events (id,run_id,type,source) values ($1,$2,$3,'runtime')")
        .bind(Uuid::new_v4())
        .bind(run)
        .bind(kind)
        .execute(&mut **tx)
        .await
        .unwrap();
}

async fn fail(pool: &PgPool, run: Uuid) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("update runs set status='failed', available_at=null, worker_id=null, lease_expires_at=null, version=version+1, error='{}', finished_at=clock_timestamp() where id=$1")
        .bind(run).execute(&mut *tx).await.unwrap();
    event(&mut tx, run, "run.failed").await;
    tx.commit().await.unwrap();
}

async fn wait(pool: &PgPool, f: &Fixture, mode: &str, count: i32) -> Uuid {
    let id = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "insert into run_waits (id,project_id,run_id,wait_key,mode) values ($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(f.project)
    .bind(f.run)
    .bind(id.to_string())
    .bind(mode)
    .execute(&mut *tx)
    .await
    .unwrap();
    for index in 0..count {
        sqlx::query("insert into run_wait_dependencies (id,project_id,wait_id,kind,correlation_key) values ($1,$2,$3,'operation',$4)")
            .bind(Uuid::new_v4()).bind(f.project).bind(id).bind(format!("operation-{index}")).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    id
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn creates_runtime_tables_without_replacing_project_data(pool: PgPool) {
    let count: i64 = sqlx::query_scalar("select count(*) from information_schema.tables where table_schema='public' and table_name in ('harnesses','sessions','messages','session_messages','runs','run_checkpoints','run_inputs','run_waits','run_wait_dependencies','run_events','workers','runtime_requests','session_state')")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(count, 13);
    let f = fixture(&pool).await;
    sqlx::query("insert into project_environments (id,project_id,name,type,machine_id,workspace_root,path) values ($1,$2,'Existing environment','machine',$3,'/','.')")
        .bind(Uuid::new_v4()).bind(f.project).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from project_environments")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn prevents_cross_project_links(pool: PgPool) {
    let a = fixture(&pool).await;
    let b = fixture(&pool).await;
    rejected(sqlx::query("insert into messages (id,project_id,origin_run_id,message) values ($1,$2,$3,'{\"role\":\"user\",\"content\":[]}')")
        .bind(Uuid::new_v4()).bind(a.project).bind(b.run).execute(&pool).await, "23503");
    rejected(
        sqlx::query(
            "insert into runs (id,project_id,session_id,parent_run_id) values ($1,$2,$3,$4)",
        )
        .bind(Uuid::new_v4())
        .bind(a.project)
        .bind(a.session)
        .bind(b.run)
        .execute(&pool)
        .await,
        "23503",
    );
    rejected(sqlx::query("insert into run_inputs (id,project_id,run_id,source_run_id,kind,deduplication_key,payload) values ($1,$2,$3,$4,'run_message','cross-project','{}')")
        .bind(Uuid::new_v4()).bind(a.project).bind(a.run).bind(b.run).execute(&pool).await, "23503");
    let w = wait(&pool, &a, "any", 1).await;
    rejected(sqlx::query("insert into run_wait_dependencies (id,project_id,wait_id,kind,target_run_id) values ($1,$2,$3,'run_completion',$4)")
        .bind(Uuid::new_v4()).bind(a.project).bind(w).bind(b.run).execute(&pool).await, "23503");
    let body = message(
        &pool,
        &b,
        "{\"role\":\"custom\",\"content\":{\"tag\":\"state\"}}",
    )
    .await;
    rejected(
        sqlx::query(
            "insert into session_messages (project_id,session_id,message_id) values ($1,$2,$3)",
        )
        .bind(a.project)
        .bind(a.session)
        .bind(body)
        .execute(&pool)
        .await,
        "23503",
    );
    rejected(sqlx::query("insert into run_waits (id,project_id,run_id,wait_key,mode) values ($1,$2,$3,'cross','all')")
        .bind(Uuid::new_v4()).bind(a.project).bind(b.run).execute(&pool).await, "23503");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn forks_share_immutable_content_and_require_a_complete_exact_prefix(pool: PgPool) {
    let f = fixture(&pool).await;
    let first = message(&pool, &f, "{\"role\":\"user\",\"content\":[]}").await;
    let second = message(&pool, &f, "{\"role\":\"assistant\",\"content\":[]}").await;
    let child = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("insert into sessions (id,project_id,harness_id,forked_from_session_id,forked_at_revision,config) values ($1,$2,'test',$3,2,'{}')")
        .bind(child).bind(f.project).bind(f.session).execute(&mut *tx).await.unwrap();
    sqlx::query("insert into session_messages (project_id,session_id,revision,message_id) select project_id,$1,revision,message_id from session_messages where session_id=$2 and revision<=2 order by revision")
        .bind(child).bind(f.session).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from messages")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select current_revision from sessions where id=$1")
            .bind(child)
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>(
            "select message_id from session_messages where session_id=$1 and revision=1"
        )
        .bind(child)
        .fetch_one(&pool)
        .await
        .unwrap(),
        first
    );
    rejected(
        sqlx::query("update messages set message='{}' where id=$1")
            .bind(second)
            .execute(&pool)
            .await,
        "23514",
    );
    rejected(
        sqlx::query("delete from session_messages where session_id=$1")
            .bind(child)
            .execute(&pool)
            .await,
        "23514",
    );
    rejected(sqlx::query("insert into sessions (id,project_id,harness_id,forked_from_session_id,forked_at_revision,config) values ($1,$2,'test',$3,3,'{}')")
        .bind(Uuid::new_v4()).bind(f.project).bind(f.session).execute(&pool).await, "23514");
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("insert into sessions (id,project_id,harness_id,forked_from_session_id,forked_at_revision,config) values ($1,$2,'test',$3,2,'{}')")
        .bind(Uuid::new_v4()).bind(f.project).bind(f.session).execute(&mut *tx).await.unwrap();
    rejected(tx.commit().await, "23514");
    rejected(
        sqlx::query("update sessions set current_revision=99 where id=$1")
            .bind(child)
            .execute(&pool)
            .await,
        "23514",
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn permits_only_one_live_run_per_session_even_under_concurrency(pool: PgPool) {
    let f = fixture(&pool).await;
    fail(&pool, f.run).await;
    let insert = |id| {
        sqlx::query("insert into runs (id,project_id,session_id) values ($1,$2,$3)")
            .bind(id)
            .bind(f.project)
            .bind(f.session)
            .execute(&pool)
    };
    let (left, right) = tokio::join!(insert(Uuid::new_v4()), insert(Uuid::new_v4()));
    assert_ne!(left.is_ok(), right.is_ok());
    rejected(if left.is_err() { left } else { right }, "23505");
    rejected(sqlx::query("update runs set status='ready', available_at=now(), error=null, finished_at=null, version=version+1 where id=$1")
        .bind(f.run).execute(&pool).await, "23514");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn completion_requires_own_final_assistant_and_atomic_terminal_event(pool: PgPool) {
    let f = fixture(&pool).await;
    let user = message(&pool, &f, "{\"role\":\"user\",\"content\":[]}").await;
    let tool = message(
        &pool,
        &f,
        "{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_call\"}]}",
    )
    .await;
    let final_id = message(&pool, &f, "{\"role\":\"assistant\",\"content\":[{\"type\":\"response\",\"response\":{\"text\":\"Done\"}}]}").await;
    for (id, with_event) in [(user, true), (tool, true), (final_id, false)] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("update runs set status='completed', available_at=null, version=version+1, final_message_id=$2, finished_at=clock_timestamp() where id=$1")
            .bind(f.run).bind(id).execute(&mut *tx).await.unwrap();
        if with_event {
            event(&mut tx, f.run, "run.completed").await;
        }
        rejected(tx.commit().await, "23514");
    }
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("update runs set status='completed', available_at=null, version=version+1, final_message_id=$2, finished_at=clock_timestamp() where id=$1")
        .bind(f.run).bind(final_id).execute(&mut *tx).await.unwrap();
    event(&mut tx, f.run, "run.completed").await;
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("select status from runs where id=$1")
            .bind(f.run)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "completed"
    );
    rejected(
        sqlx::query("delete from projects where project_id=$1")
            .bind(f.project)
            .execute(&pool)
            .await,
        "23503",
    );
    rejected(
        sqlx::query("delete from runs where id=$1")
            .bind(f.run)
            .execute(&pool)
            .await,
        "23514",
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn rejects_a_final_message_from_another_run(pool: PgPool) {
    let f = fixture(&pool).await;
    let old = message(&pool, &f, "{\"role\":\"assistant\",\"content\":[]}").await;
    fail(&pool, f.run).await;
    let next = Uuid::new_v4();
    sqlx::query("insert into runs (id,project_id,session_id) values ($1,$2,$3)")
        .bind(next)
        .bind(f.project)
        .bind(f.session)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("update runs set status='completed', available_at=null, version=version+1, final_message_id=$2, finished_at=clock_timestamp() where id=$1")
        .bind(next).bind(old).execute(&mut *tx).await.unwrap();
    event(&mut tx, next, "run.completed").await;
    rejected(tx.commit().await, "23503");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn checkpoint_requires_current_lease_and_next_version(pool: PgPool) {
    let f = fixture(&pool).await;
    rejected(
        sqlx::query(
            "insert into run_checkpoints (run_id,state,saved_by_lease_epoch) values ($1,'{}',1)",
        )
        .bind(f.run)
        .execute(&pool)
        .await,
        "23514",
    );
    claim(&pool, &f).await;
    sqlx::query("insert into run_checkpoints (run_id,state,saved_by_lease_epoch) values ($1,'{\"iteration\":1}',1)")
        .bind(f.run).execute(&pool).await.unwrap();
    rejected(
        sqlx::query("update run_checkpoints set state='{}' where run_id=$1")
            .bind(f.run)
            .execute(&pool)
            .await,
        "23514",
    );
    sqlx::query("update runs set status='ready',available_at=clock_timestamp(),worker_id=null,lease_expires_at=null,version=version+1 where id=$1")
        .bind(f.run).execute(&pool).await.unwrap();
    claim(&pool, &f).await;
    rejected(sqlx::query("update run_checkpoints set version=2,state='{}',saved_by_lease_epoch=1 where run_id=$1")
        .bind(f.run).execute(&pool).await, "23514");
    sqlx::query(
        "update run_checkpoints set version=2,state='{}',saved_by_lease_epoch=2 where run_id=$1",
    )
    .bind(f.run)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(f.run)
    .execute(&pool)
    .await
    .unwrap();
    rejected(
        sqlx::query("update run_checkpoints set version=3,state='{}' where run_id=$1")
            .bind(f.run)
            .execute(&pool)
            .await,
        "23514",
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn disabled_harness_blocks_new_runs_not_recovery(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::query("update harnesses set enabled=false where id='test'")
        .execute(&pool)
        .await
        .unwrap();
    claim(&pool, &f).await;
    rejected(
        sqlx::query("insert into runs (id,project_id,session_id) values ($1,$2,$3)")
            .bind(Uuid::new_v4())
            .bind(f.project)
            .bind(f.session)
            .execute(&pool)
            .await,
        "23514",
    );
    rejected(
        sqlx::query("update runs set config='{\"changed\":true}' where id=$1")
            .bind(f.run)
            .execute(&pool)
            .await,
        "23514",
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn worker_and_run_lease_shapes_are_checked(pool: PgPool) {
    let f = fixture(&pool).await;
    rejected(
        sqlx::query("update workers set capacity=0 where id=$1")
            .bind(f.worker)
            .execute(&pool)
            .await,
        "23514",
    );
    sqlx::query("update workers set status='draining' where id=$1")
        .bind(f.worker)
        .execute(&pool)
        .await
        .unwrap();
    rejected(sqlx::query("update runs set status='running',available_at=null,worker_id=$2,lease_epoch=1,version=2,started_at=clock_timestamp(),lease_expires_at=clock_timestamp()+interval '1 minute' where id=$1")
        .bind(f.run).bind(f.worker).execute(&pool).await, "23514");
    rejected(
        sqlx::query("update runs set worker_id=$2 where id=$1")
            .bind(f.run)
            .bind(f.worker)
            .execute(&pool)
            .await,
        "23514",
    );
    rejected(sqlx::query("update runs set status='failed',available_at=null,version=2,finished_at=clock_timestamp() where id=$1")
        .bind(f.run).execute(&pool).await, "23514");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn inputs_are_ordered_deduplicated_and_keep_original_payload(pool: PgPool) {
    let f = fixture(&pool).await;
    let id = Uuid::new_v4();
    sqlx::query("insert into run_inputs (id,project_id,run_id,kind,deduplication_key,payload) values ($1,$2,$3,'user_message','steer-1','{\"text\":\"New direction\"}')")
        .bind(id).bind(f.project).bind(f.run).execute(&pool).await.unwrap();
    rejected(sqlx::query("insert into run_inputs (id,project_id,run_id,kind,deduplication_key,payload) values ($1,$2,$3,'user_message','steer-1','{}')")
        .bind(Uuid::new_v4()).bind(f.project).bind(f.run).execute(&pool).await, "23505");
    rejected(
        sqlx::query("update run_inputs set payload='{}' where id=$1")
            .bind(id)
            .execute(&pool)
            .await,
        "23514",
    );
    rejected(
        sqlx::query("update run_inputs set status='handled' where id=$1")
            .bind(id)
            .execute(&pool)
            .await,
        "23514",
    );
    sqlx::query("update run_inputs set status='handled',handled_at=clock_timestamp(),handling='{\"provider_queued\":true}' where id=$1")
        .bind(id).execute(&pool).await.unwrap();
    rejected(
        sqlx::query(
            "update run_inputs set status='pending',handled_at=null,handling=null where id=$1",
        )
        .bind(id)
        .execute(&pool)
        .await,
        "23514",
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn waits_require_dependencies_and_valid_target_shapes(pool: PgPool) {
    let f = fixture(&pool).await;
    rejected(sqlx::query("insert into run_waits (id,project_id,run_id,wait_key,mode) values ($1,$2,$3,'empty','all')")
        .bind(Uuid::new_v4()).bind(f.project).bind(f.run).execute(&pool).await, "23514");
    let w = wait(&pool, &f, "all", 1).await;
    rejected(sqlx::query("insert into run_wait_dependencies (id,project_id,wait_id,kind,target_run_id) values ($1,$2,$3,'timer',$4)")
        .bind(Uuid::new_v4()).bind(f.project).bind(w).bind(f.run).execute(&pool).await, "23514");
    rejected(sqlx::query("insert into run_wait_dependencies (id,project_id,wait_id,kind,target_run_id) values ($1,$2,$3,'run_completion',$4)")
        .bind(Uuid::new_v4()).bind(f.project).bind(w).bind(f.run).execute(&pool).await, "23514");
    rejected(
        sqlx::query(
            "update run_waits set status='timed_out',resolved_at=clock_timestamp() where id=$1",
        )
        .bind(w)
        .execute(&pool)
        .await,
        "23514",
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn wait_resolution_and_wakeup_must_commit_together(pool: PgPool) {
    let f = fixture(&pool).await;
    let w = wait(&pool, &f, "all", 2).await;
    sqlx::query("update runs set status='waiting',available_at=null,version=version+1 where id=$1")
        .bind(f.run)
        .execute(&pool)
        .await
        .unwrap();
    rejected(
        sqlx::query(
            "update run_waits set status='satisfied',resolved_at=clock_timestamp() where id=$1",
        )
        .bind(w)
        .execute(&pool)
        .await,
        "23514",
    );
    sqlx::query("update run_wait_dependencies set satisfied_at=clock_timestamp() where wait_id=$1")
        .bind(w)
        .execute(&pool)
        .await
        .unwrap();
    rejected(
        sqlx::query(
            "update run_waits set status='satisfied',resolved_at=clock_timestamp() where id=$1",
        )
        .bind(w)
        .execute(&pool)
        .await,
        "23514",
    );
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("update run_waits set status='satisfied',resolved_at=clock_timestamp(),result='{}' where id=$1")
        .bind(w).execute(&mut *tx).await.unwrap();
    sqlx::query("update runs set status='ready',available_at=clock_timestamp(),version=version+1 where id=$1")
        .bind(f.run).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn any_wait_can_resolve_without_waiting_for_every_dependency(pool: PgPool) {
    let f = fixture(&pool).await;
    let w = wait(&pool, &f, "any", 2).await;
    sqlx::query("update run_wait_dependencies set satisfied_at=clock_timestamp() where wait_id=$1 and correlation_key='operation-0'")
        .bind(w).execute(&pool).await.unwrap();
    sqlx::query(
        "update run_waits set status='satisfied',resolved_at=clock_timestamp() where id=$1",
    )
    .bind(w)
    .execute(&pool)
    .await
    .unwrap();
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn events_serialize_concurrent_appends_without_counter_drift(pool: PgPool) {
    let f = fixture(&pool).await;
    let insert = || {
        sqlx::query("insert into run_events (id,run_id,type,source) values ($1,$2,'harness.progress','harness') returning sequence")
        .bind(Uuid::new_v4()).bind(f.run).fetch_one(&pool)
    };
    let (left, right) = tokio::join!(insert(), insert());
    left.unwrap();
    right.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select last_event_sequence from runs where id=$1")
            .bind(f.run)
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    rejected(sqlx::query("insert into run_events (id,run_id,sequence,type,source) values ($1,$2,99,'progress','harness')")
        .bind(Uuid::new_v4()).bind(f.run).execute(&pool).await, "23514");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select last_event_sequence from runs where id=$1")
            .bind(f.run)
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    rejected(
        sqlx::query("delete from run_events where run_id=$1")
            .bind(f.run)
            .execute(&pool)
            .await,
        "23514",
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn request_receipts_are_scoped_immutable_and_retained_for_run_recovery(pool: PgPool) {
    let f = fixture(&pool).await;
    let other = fixture(&pool).await;
    sqlx::query("insert into runtime_requests (project_id,scope_kind,scope_id,key,request_hash,result) values ($1,'run.spawn_child',$2,'reviewer-1',repeat('a',64),'{}')")
        .bind(f.project).bind(f.run).execute(&pool).await.unwrap();
    rejected(sqlx::query("insert into runtime_requests (project_id,scope_kind,scope_id,key,request_hash,result) values ($1,'run.spawn_child',$2,'reviewer-1',repeat('b',64),'{}')")
        .bind(f.project).bind(f.run).execute(&pool).await, "23505");
    rejected(sqlx::query("insert into runtime_requests (project_id,scope_kind,scope_id,key,request_hash,result) values ($1,'run.spawn_child',$2,'cross',repeat('a',64),'{}')")
        .bind(other.project).bind(f.run).execute(&pool).await, "23503");
    rejected(sqlx::query("insert into runtime_requests (project_id,scope_kind,scope_id,key,request_hash,result,expires_at) values ($1,'run.spawn_child',$2,'expiring',repeat('a',64),'{}',now()+interval '1 day')")
        .bind(f.project).bind(f.run).execute(&pool).await, "23514");
    rejected(
        sqlx::query("update runtime_requests set result='{\"changed\":true}' where scope_id=$1")
            .bind(f.run)
            .execute(&pool)
            .await,
        "23514",
    );
    rejected(
        sqlx::query("delete from runtime_requests where scope_id=$1")
            .bind(f.run)
            .execute(&pool)
            .await,
        "23514",
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn concurrent_history_appends_are_serialized_and_shared_ids_are_project_scoped(pool: PgPool) {
    let f = fixture(&pool).await;
    let (first, second) = tokio::join!(
        message(&pool, &f, "{\"role\":\"user\",\"content\":[]}"),
        message(
            &pool,
            &f,
            "{\"role\":\"custom\",\"content\":{\"harness\":\"test\"}}"
        )
    );
    assert_ne!(first, second);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select current_revision from sessions where id=$1")
            .bind(f.session)
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    for body in [
        "null",
        "[]",
        "{}",
        "{\"role\":\"assistant\"}",
        "{\"role\":\"custom\",\"content\":[]}",
    ] {
        rejected(
            sqlx::query("insert into messages (id,project_id,message) values ($1,$2,$3::jsonb)")
                .bind(Uuid::new_v4())
                .bind(f.project)
                .bind(body)
                .execute(&pool)
                .await,
            "23514",
        );
    }
    rejected(
        sqlx::query(
            "update sessions set forked_from_session_id=id,forked_at_revision=0 where id=$1",
        )
        .bind(f.session)
        .execute(&pool)
        .await,
        "23514",
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn abort_acknowledgement_releases_lease_and_local_waits_without_stopping_children(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    claim(&pool, &f).await;
    let w = wait(&pool, &f, "any", 1).await;
    let session = Uuid::new_v4();
    let child = Uuid::new_v4();
    sqlx::query(
        "insert into sessions (id,project_id,harness_id,config) values ($1,$2,'test','{}')",
    )
    .bind(session)
    .bind(f.project)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("insert into runs (id,project_id,session_id,parent_run_id) values ($1,$2,$3,$4)")
        .bind(child)
        .bind(f.project)
        .bind(session)
        .bind(f.run)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("insert into run_inputs (id,project_id,run_id,kind,deduplication_key,payload) values ($1,$2,$3,'abort','abort-1','{}')")
        .bind(Uuid::new_v4()).bind(f.project).bind(f.run).execute(&mut *tx).await.unwrap();
    sqlx::query(
        "update runs set abort_requested_at=clock_timestamp(),version=version+1 where id=$1",
    )
    .bind(f.run)
    .execute(&mut *tx)
    .await
    .unwrap();
    event(&mut tx, f.run, "run.abort_requested").await;
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("select status from runs where id=$1")
            .bind(f.run)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "running"
    );
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "update run_waits set status='cancelled',resolved_at=clock_timestamp() where id=$1",
    )
    .bind(w)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "update run_inputs set status='handled',handled_at=clock_timestamp() where run_id=$1",
    )
    .bind(f.run)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("update runs set status='aborted',worker_id=null,lease_expires_at=null,finished_at=clock_timestamp(),version=version+1 where id=$1")
        .bind(f.run).execute(&mut *tx).await.unwrap();
    event(&mut tx, f.run, "run.aborted").await;
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("select status from runs where id=$1")
            .bind(child)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "ready"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn run_dependencies_cannot_report_completion_while_the_target_is_live(pool: PgPool) {
    let f = fixture(&pool).await;
    let session = Uuid::new_v4();
    let child = Uuid::new_v4();
    sqlx::query(
        "insert into sessions (id,project_id,harness_id,config) values ($1,$2,'test','{}')",
    )
    .bind(session)
    .bind(f.project)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("insert into runs (id,project_id,session_id,parent_run_id) values ($1,$2,$3,$4)")
        .bind(child)
        .bind(f.project)
        .bind(session)
        .bind(f.run)
        .execute(&pool)
        .await
        .unwrap();
    let w = wait(&pool, &f, "any", 1).await;
    let dependency = Uuid::new_v4();
    sqlx::query("insert into run_wait_dependencies (id,project_id,wait_id,kind,target_run_id) values ($1,$2,$3,'run_completion',$4)")
        .bind(dependency).bind(f.project).bind(w).bind(child).execute(&pool).await.unwrap();
    rejected(
        sqlx::query("update run_wait_dependencies set satisfied_at=clock_timestamp() where id=$1")
            .bind(dependency)
            .execute(&pool)
            .await,
        "23514",
    );
    fail(&pool, child).await;
    sqlx::query("update run_wait_dependencies set satisfied_at=clock_timestamp() where id=$1")
        .bind(dependency)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "update run_waits set status='satisfied',resolved_at=clock_timestamp() where id=$1",
    )
    .bind(w)
    .execute(&pool)
    .await
    .unwrap();
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn lifecycle_events_cannot_pretend_a_live_run_has_finished(pool: PgPool) {
    let f = fixture(&pool).await;
    rejected(sqlx::query("insert into run_events (id,run_id,type,source) values ($1,$2,'run.completed','runtime')")
        .bind(Uuid::new_v4()).bind(f.run).execute(&pool).await, "23514");
    rejected(sqlx::query("insert into run_events (id,run_id,type,source) values ($1,$2,'run.completed','harness')")
        .bind(Uuid::new_v4()).bind(f.run).execute(&pool).await, "23514");
    claim(&pool, &f).await;
    rejected(
        sqlx::query("update runs set lease_epoch=lease_epoch+1,version=version+1 where id=$1")
            .bind(f.run)
            .execute(&pool)
            .await,
        "23514",
    );
}

#[sqlx::test(migrations = false)]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn upgrade_preserves_existing_projects_and_environments(pool: PgPool) {
    for migration in [
        include_str!("../migrations/20260830000000_create_projects.sql"),
        include_str!("../migrations/20260830010000_expand_project_avatar_size.sql"),
        include_str!("../migrations/20260904000000_create_project_environments.sql"),
        include_str!("../migrations/20260904010000_add_environment_workspace_root_path.sql"),
    ] {
        sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    }
    let project = Uuid::new_v4();
    let environment = Uuid::new_v4();
    sqlx::query("insert into projects (project_id,name) values ($1,'Keep me')")
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into project_environments (id,project_id,name,type,machine_id,workspace_root,workspace_root_path,path) values ($1,$2,'Desktop test','machine',$3,'root','/','Users/test/Desktop/test')")
        .bind(environment).bind(project).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20260904020000_create_agent_runtime.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let existing = fixture_version(&pool, false).await;
    claim(&pool, &existing).await;
    sqlx::query("insert into run_checkpoints(run_id,state,saved_by_lease_epoch) values($1,'{\"phase\":\"existing\"}',1)")
        .bind(existing.run).execute(&pool).await.unwrap();
    for migration in [
        include_str!("../migrations/20260904030000_worker_runtime.sql"),
        include_str!("../migrations/20260904040000_allow_empty_workers.sql"),
        include_str!("../migrations/20260904050000_create_session_state.sql"),
        include_str!("../migrations/20260905060000_session_configuration.sql"),
        include_str!("../migrations/20260906000000_environment_native_paths.sql"),
    ] {
        sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    }
    let frozen: serde_json::Value = sqlx::query_scalar("select config from sessions where id=$1")
        .bind(existing.session)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(frozen, serde_json::json!({}));
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "select state->>'phase' from run_checkpoints where run_id=$1"
        )
        .bind(existing.run)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "existing"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from session_state")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("select name from projects where project_id=$1")
            .bind(project)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "Keep me"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "select workspace_root || path from project_environments where id=$1"
        )
        .bind(environment)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "/Users/test/Desktop/test"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn session_state_database_guards_scope_ownership_versions_and_deletion(pool: PgPool) {
    let f = fixture(&pool).await;
    claim(&pool, &f).await;
    sqlx::query("insert into session_state(project_id,session_id,namespace,key,version,value,saved_by_run_id,saved_by_lease_epoch) values($1,$2,'tool','a',1,'{}',$3,1)")
        .bind(f.project).bind(f.session).bind(f.run).execute(&pool).await.unwrap();
    let other = fixture(&pool).await;
    claim(&pool, &other).await;
    rejected(
        sqlx::query(
            "update session_state set version=version+1,saved_by_run_id=$2 where session_id=$1",
        )
        .bind(f.session)
        .bind(other.run)
        .execute(&pool)
        .await,
        "23514",
    );
    rejected(
        sqlx::query("update session_state set version=version+1,project_id=$2 where session_id=$1")
            .bind(f.session)
            .bind(other.project)
            .execute(&pool)
            .await,
        "23514",
    );
    for sql in [
        "update session_state set version=version+2 where session_id=$1",
        "update session_state set version=version+1,key='different' where session_id=$1",
        "update session_state set version=version+1,namespace='different' where session_id=$1",
        "update session_state set version=version+1,value='[]' where session_id=$1",
        "update session_state set version=version+1,saved_by_lease_epoch=2 where session_id=$1",
        "delete from session_state where session_id=$1",
    ] {
        rejected(
            sqlx::query(sql).bind(f.session).execute(&pool).await,
            "23514",
        );
    }
    sqlx::query("update session_state set version=version+1,value=null where session_id=$1")
        .bind(f.session)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select version from session_state where session_id=$1")
            .bind(f.session)
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(f.run)
    .execute(&pool)
    .await
    .unwrap();
    rejected(
        sqlx::query("update session_state set version=version+1,value='{}' where session_id=$1")
            .bind(f.session)
            .execute(&pool)
            .await,
        "23514",
    );
}

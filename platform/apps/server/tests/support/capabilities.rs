//! Step 2 across actual worker HTTP and sandboxed Sites SDK adapters.
use super::*;
use platform_runtime_client::{
    ClientConfig, Command as Cmd, PlatformClient, RequestKey, WorkerRegistration,
    types::{self as t, capabilities as c},
};

fn user(text: &str) -> Value {
    json!({"role":"user","id":Uuid::now_v7().to_string(),"timestamp":1,"content":[{"type":"text","content":text}]})
}
fn command<T>(body: T) -> Cmd<T> {
    Cmd::new(RequestKey::new(Uuid::now_v7().to_string()).unwrap(), body)
}
fn create(f: &Fixture) -> Value {
    json!({"harnessId":HARNESS,"accountId":f.account,"config":{"model":{"provider":"openai","id":"test-model"}},"title":"Capability session"})
}
fn mutation(input: Value, key: &str) -> Value {
    json!([input,{"idempotencyKey":key}])
}
fn ok(reply: Value) -> Value {
    assert_eq!(reply["status"], 200, "{reply}");
    reply["body"].clone()
}
pub(super) async fn declared(f: &Fixture) {
    sqlx::query("update harnesses set default_config=$1,config_schema=$2,harness_contract=$3 where id=$4")
        .bind(json!({"model":{"provider":"openai","id":"test-model"},"serverOnly":"fixed"}))
        .bind(json!({"type":"object","additionalProperties":false,"required":["model","account_id","serverOnly"],"properties":{"model":{"type":"object","required":["provider","id"]},"account_id":{"type":"string"},"serverOnly":{"const":"fixed"},"task":{"type":"object","properties":{"environment":{"type":"string"},"label":{"type":"string"}},"additionalProperties":false},"evaluators":{"type":"array","items":{"type":"string"}}}}))
        .bind(json!({"environment_inputs":[{"config_pointer":"/task/environment","cardinality":"single"},{"config_pointer":"/evaluators","cardinality":"multiple"}],"outputs":{"result":{"kind":"json"}}}))
        .bind(HARNESS).execute(&f.pool).await.unwrap();
    // Exercise the real admin API and omitted environmentMode default.
    f.request(Method::PUT,&format!("/internal/site-access/{}",f.project),ADMIN,
        json!({"token":TOKEN,"enabled":true,"harnesses":[{"id":HARNESS,"configurableFields":["model","task","evaluators"]}],"accountIds":[f.account]}),200).await;
}
pub(super) async fn worker(f: &mut Fixture) -> (PlatformClient, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = platform_server::runtime::worker_router(f.runtime.clone(), ADMIN);
    f.tasks.push(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let client = PlatformClient::new(&url, Uuid::new_v4(), TOKEN, ClientConfig::default()).unwrap();
    client
        .register(
            ADMIN,
            &WorkerRegistration {
                build_id: "capabilities".into(),
                supported_harnesses: vec![HARNESS.into()],
                capacity: 100,
            },
        )
        .await
        .unwrap();
    (client, url)
}
pub(super) async fn claim(client: &PlatformClient, id: Uuid) -> platform_runtime_client::RunClient {
    let a = client
        .claim(&command(t::Claim { limit: 100 }))
        .await
        .unwrap();
    client
        .run(
            a.items
                .iter()
                .find(|a| a.lease().run_id == id)
                .expect("assignment")
                .lease(),
        )
        .unwrap()
}
async fn finish(f: &Fixture, run: Uuid) {
    let mut tx = f.pool.begin().await.unwrap();
    sqlx::query("update runs set status='failed',error='{}',version=version+1,worker_id=null,lease_expires_at=null,available_at=null,finished_at=clock_timestamp() where id=$1").bind(run).execute(&mut *tx).await.unwrap();
    sqlx::query("insert into run_events(id,run_id,type,source,payload) values($1,$2,'run.failed','runtime','{}')").bind(Uuid::now_v7()).bind(run).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn shared_sites_configuration_inventory_and_atomic_admission(pool: PgPool) {
    let f = Fixture::new(pool).await;
    declared(&f).await;
    let accounts = ok(f.sdk("accounts.list", json!([])).await);
    assert_eq!(accounts["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        accounts["items"][0].as_object().unwrap().len(),
        4,
        "no credentials or config"
    );
    let options = ok(f.sdk("harnesses.startOptions", json!([HARNESS])).await);
    assert_eq!(options["environmentMode"], "declared");
    assert_eq!(
        options["configurableFields"],
        json!(["model", "task", "evaluators"])
    );
    assert_eq!(
        options["harnessContract"]["environment_inputs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for (key, config) in [
        ("zero", json!({})),
        (
            "single",
            json!({"task":{"environment":f.environment,"label":"training"}}),
        ),
        ("many", json!({"evaluators":[f.environment,f.environment]})),
    ] {
        let mut input = create(&f);
        input["config"] = config.clone();
        let result = ok(f.sdk("sessions.create", mutation(input.clone(), key)).await);
        assert!(result["run"].is_null());
        assert_eq!(result["session"]["config"]["serverOnly"], "fixed");
        assert_eq!(
            result["session"]["config"]["account_id"],
            f.account.to_string()
        );
        assert_eq!(
            ok(f.sdk("sessions.create", mutation(input, key)).await),
            result
        );
    }
    for (key, config) in [
        (
            "foreign",
            json!({"task":{"environment":f.foreign_environment}}),
        ),
        ("wrong-type", json!({"evaluators":"x"})),
        ("privileged", json!({"serverOnly":"overridden"})),
        ("nested", json!({"task":{"unknown":true}})),
        ("account-conflict", json!({"account_id":Uuid::new_v4()})),
    ] {
        let mut input = create(&f);
        input["config"] = config;
        assert_eq!(
            f.sdk("sessions.create", mutation(input, key)).await["status"],
            400
        );
    }
    let mut input = create(&f);
    input["initialInput"] = user("first");
    input["onComplete"] = json!({"path":"https://elsewhere/"});
    assert_eq!(
        f.sdk("sessions.create", mutation(input.clone(), "atomic"))
            .await["status"],
        400
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from sessions where project_id=$1")
            .bind(f.project)
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        3
    );
    input["onComplete"] = json!({"path":"/completed","payload":{"job":1}});
    let (first, retry) = tokio::join!(
        f.sdk("sessions.create", mutation(input.clone(), "atomic")),
        f.sdk("sessions.create", mutation(input.clone(), "atomic")),
    );
    let created = ok(first);
    assert_eq!(ok(retry), created);
    assert!(created["callback"]["subscriptionId"].is_string());
    assert_eq!(
        ok(f.sdk("sessions.create", mutation(input, "atomic")).await),
        created
    );
    let page = ok(f.sdk("sessions.list", json!([{"limit":2}])).await);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let rest = ok(f
        .sdk(
            "sessions.list",
            json!([{"limit":2,"cursor":page["next_cursor"]}]),
        )
        .await);
    assert_eq!(rest["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        f.sdk("harnesses.list", json!([{"cursor":page["next_cursor"]}]))
            .await["status"],
        400
    );
    let attribution: i64=sqlx::query_scalar("select count(*) from site_runtime_operations where site_id=$1 and operation='site.platform.sessions.create'").bind(f.site).fetch_one(&f.pool).await.unwrap();
    assert_eq!(attribution, 4);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn shared_sites_exact_runs_frozen_config_and_grants(pool: PgPool) {
    let f = Fixture::new(pool).await;
    declared(&f).await;
    let session = ok(f
        .sdk("sessions.create", mutation(create(&f), "session"))
        .await)["session"]
        .clone();
    let stats = ok(f.sdk("sessions.stats", json!([session["id"]])).await);
    assert!(stats["inputTokens"]["total"].is_null());
    assert_eq!(stats["inputTokens"]["contributingMessages"], 0);
    let start = json!({"sessionId":session["id"],"expectedRevision":0,"input":user("start")});
    let (a, b) = tokio::join!(
        f.sdk("runs.create", mutation(start.clone(), "run-a")),
        f.sdk("runs.create", mutation(start.clone(), "run-b"))
    );
    assert_eq!(
        usize::from(a["status"] == 200) + usize::from(b["status"] == 200),
        1,
        "{a} {b}"
    );
    let (created, key) = if a["status"] == 200 {
        (a["body"].clone(), "run-a")
    } else {
        (b["body"].clone(), "run-b")
    };
    let run = created["run"]["id"].clone();
    let mut forbidden = start.clone();
    forbidden["config"] = json!({"account_id":Uuid::new_v4()});
    assert_eq!(
        f.sdk("runs.create", mutation(forbidden, "config-override"))
            .await["status"],
        400
    );
    assert_eq!(
        ok(f.sdk("runs.create", mutation(start.clone(), key)).await),
        created
    );
    let steer = json!({"runId":run,"input":user("steer")});
    let accepted = ok(f.sdk("runs.steer", mutation(steer.clone(), "steer")).await);
    assert_eq!(
        ok(f.sdk("runs.steer", mutation(steer, "steer")).await),
        accepted
    );
    assert_eq!(accepted["input"]["status"], "pending");
    let stopped = ok(f
        .sdk("runs.abort", mutation(json!({"runId":run}), "abort"))
        .await);
    assert!(!stopped["run"]["abort_requested_at"].is_null());
    finish(&f, serde_json::from_value(run.clone()).unwrap()).await;
    // Changing catalog defaults does not alter a session or its follow-up run.
    sqlx::query("update harnesses set default_config='{}',harness_contract='{}' where id=$1")
        .bind(HARNESS)
        .execute(&f.pool)
        .await
        .unwrap();
    let mut follow = start;
    follow["input"] = user("follow-up");
    let next = ok(f.sdk("runs.create", mutation(follow, "next")).await);
    assert_eq!(next["run"]["config"], session["config"]);
    assert_ne!(next["run"]["id"], run);
    for method in ["runs.steer", "runs.abort"] {
        let input = if method == "runs.steer" {
            json!({"runId":run,"input":user("late")})
        } else {
            json!({"runId":run})
        };
        assert_eq!(f.sdk(method, mutation(input, "late")).await["status"], 400);
    }
    assert!(
        ok(f.sdk("runs.get", json!([next["run"]["id"]])).await)["abort_requested_at"].is_null()
    );
    let page = ok(f.sdk("runs.list", json!([session["id"],{"limit":1}])).await);
    assert_eq!(page["items"][0]["id"], run);
    let next_page = ok(f
        .sdk(
            "runs.list",
            json!([session["id"],{"limit":1,"cursor":page["next_cursor"]}]),
        )
        .await);
    assert_eq!(next_page["items"][0]["id"], next["run"]["id"]);
    sqlx::query("delete from site_account_grants where project_id=$1")
        .bind(f.project)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        ok(f.sdk("accounts.list", json!([])).await)["items"],
        json!([])
    );
    assert_eq!(
        f.sdk(
            "runs.steer",
            mutation(
                json!({"runId":next["run"]["id"],"input":user("revoked")}),
                "revoked"
            )
        )
        .await["status"],
        400
    );
    // Accepted receipts remain available, while abort does not need the removed account.
    assert_eq!(ok(f.sdk("runs.create",mutation(json!({"sessionId":session["id"],"expectedRevision":0,"input":created["input"]["payload"]["message"]}),key)).await),created);
    ok(f.sdk(
        "runs.abort",
        mutation(json!({"runId":next["run"]["id"]}), "abort-next"),
    )
    .await);
    sqlx::query("delete from site_harness_grants where project_id=$1")
        .bind(f.project)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        f.sdk(
            "runs.abort",
            mutation(json!({"runId":next["run"]["id"]}), "abort-revoked")
        )
        .await["status"],
        400
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn shared_agent_sdk_scope_recovery_and_typed_records(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    declared(&f).await;
    let mut initial = create(&f);
    initial["initialInput"] = user("orchestrator");
    let source = ok(f.sdk("sessions.create", mutation(initial, "source")).await);
    let source_id = serde_json::from_value(source["run"]["id"].clone()).unwrap();
    let (client, url) = worker(&mut f).await;
    let run = claim(&client, source_id).await;
    let api = run.platform();
    let accounts = api.list_accounts(&c::PageOptions::default()).await.unwrap();
    assert_eq!(
        accounts.items.len(),
        2,
        "trusted worker is not limited to Sites grants"
    );
    let options = api.start_options(HARNESS).await.unwrap();
    assert_eq!(options.accounts.len(), 2);
    assert!(options.configurable_fields.is_none());
    api.get_harness(HARNESS).await.unwrap();
    assert!(
        !api.list_harnesses(&c::PageOptions::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        api.list_environments(&c::PageOptions::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    api.get_environment(f.environment).await.unwrap();
    assert!(api.get_environment(f.foreign_environment).await.is_err());
    let create: c::CreateSession = serde_json::from_value(create(&f)).unwrap();
    let create = command(create);
    let session = api.create_session(&create).await.unwrap();
    assert!(session.run.is_none());
    let request = command(c::CreateRun {
        session_id: session.session.id,
        expected_revision: 0,
        input: serde_json::from_value(user("child")).unwrap(),
        on_complete: None,
    });
    let child = api.create_run(&request).await.unwrap();
    assert_eq!(child.run.parent_run_id, Some(source_id));
    assert_eq!(
        api.get_session(session.session.id).await.unwrap().config,
        session.session.config
    );
    assert!(
        api.messages(session.session.id, &c::MessageOptions::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        api.session_stats(session.session.id)
            .await
            .unwrap()
            .input_tokens
            .total
            .is_none()
    );
    assert_eq!(
        api.list_runs(session.session.id, &c::PageOptions::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    api.get_run(child.run.id).await.unwrap();
    api.run_stats(child.run.id).await.unwrap();
    assert!(
        api.run_outputs(child.run.id, &c::OutputOptions::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let steered = api
        .steer_run(&command(c::SteerRun {
            run_id: child.run.id,
            input: serde_json::from_value(user("steer")).unwrap(),
        }))
        .await
        .unwrap();
    assert_eq!(steered.input.source_run_id, Some(source_id));
    api.abort_run(&command(c::AbortRun {
        run_id: child.run.id,
        reason: None,
    }))
    .await
    .unwrap();
    let foreign_session = Uuid::now_v7();
    let foreign_run = Uuid::now_v7();
    sqlx::query("insert into sessions(id,project_id,harness_id,config) values($1,$2,$3,'{}')")
        .bind(foreign_session)
        .bind(f.other)
        .bind(HARNESS)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("insert into runs(id,project_id,session_id) values($1,$2,$3)")
        .bind(foreign_run)
        .bind(f.other)
        .bind(foreign_session)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(api.get_session(foreign_session).await.is_err());
    assert!(api.get_run(foreign_run).await.is_err());
    assert!(
        api.create_run(&command(c::CreateRun {
            session_id: foreign_session,
            expected_revision: 0,
            input: serde_json::from_value(user("foreign")).unwrap(),
            on_complete: None
        }))
        .await
        .is_err()
    );
    for (method, args) in [
        ("runs.get", json!({"runId":foreign_run})),
        ("runs.stats", json!({"runId":foreign_run})),
        ("sessions.list", json!({"projectId":f.other})),
    ] {
        let response = f
            .client
            .post(format!("{url}/internal/runs/{source_id}/capabilities"))
            .bearer_auth(TOKEN)
            .header("x-worker-id", client.worker_id().to_string())
            .header("x-lease-epoch", run.lease().lease_epoch)
            .json(&json!({"method":method,"args":args}))
            .send()
            .await
            .unwrap();
        assert!(!response.status().is_success());
    }
    let response = f
        .client
        .post(format!("{url}/internal/runs/{source_id}/capabilities"))
        .json(&json!({"method":"accounts.list","args":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(source_id)
    .execute(&f.pool)
    .await
    .unwrap();
    let replacement =
        PlatformClient::new(&url, Uuid::new_v4(), SERVICE, ClientConfig::default()).unwrap();
    replacement
        .register(
            ADMIN,
            &WorkerRegistration {
                build_id: "replacement".into(),
                supported_harnesses: vec![HARNESS.into()],
                capacity: 100,
            },
        )
        .await
        .unwrap();
    let replacement = claim(&replacement, source_id).await;
    assert_eq!(
        replacement
            .platform()
            .create_session(&create)
            .await
            .unwrap()
            .session
            .id,
        session.session.id
    );
    assert_eq!(
        api.create_session(&create).await.unwrap().session.id,
        session.session.id,
        "original issuer receipt"
    );
    assert!(api.get_session(session.session.id).await.is_err());
    assert!(
        api.abort_run(&command(c::AbortRun {
            run_id: child.run.id,
            reason: None
        }))
        .await
        .is_err()
    );
    let replacement_api = replacement.platform();
    assert_eq!(
        replacement_api
            .list_sessions(&c::PageOptions {
                limit: Some(1),
                cursor: None
            })
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let mut changed: c::CreateSession =
        serde_json::from_value(super::capabilities::create(&f)).unwrap();
    changed.title = Some("changed".into());
    assert!(
        replacement_api
            .create_session(&Cmd::new(create.key().clone(), changed))
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn shared_statistics_and_bounded_history_are_scoped(pool: PgPool) {
    let f = Fixture::new(pool).await;
    declared(&f).await;
    let mut input = create(&f);
    input["initialInput"] = user("metrics");
    let created = ok(f.sdk("sessions.create", mutation(input, "metrics")).await);
    let session: Uuid = serde_json::from_value(created["session"]["id"].clone()).unwrap();
    let run: Uuid = serde_json::from_value(created["run"]["id"].clone()).unwrap();
    for n in 1..=4_i64 {
        let id = Uuid::now_v7();
        let usage = if n == 4 {
            Value::Null
        } else {
            json!({"input":10,"output":0,"cache_read":2,"cache_write":0,"cost":{"total":0.25}})
        };
        sqlx::query("insert into messages(id,project_id,origin_run_id,message) values($1,$2,$3,$4)").bind(id).bind(f.project).bind(run).bind(json!({"id":id,"role":"assistant","content":[{"type":"response","response":{"content":"x".repeat(60*1024)}}],"usage":usage})).execute(&f.pool).await.unwrap();
        sqlx::query("insert into session_messages(project_id,session_id,revision,message_id,run_id) values($1,$2,$3,$4,$5)").bind(f.project).bind(session).bind(n).bind(id).bind(run).execute(&f.pool).await.unwrap();
    }
    let page = ok(f
        .sdk("sessions.messages", json!([session,{"limit":50}]))
        .await);
    assert_eq!(page["items"].as_array().unwrap().len(), 3);
    assert_eq!(page["next_after_revision"], 3);
    assert_eq!(
        ok(
            f.sdk("sessions.messages", json!([session,{"afterRevision":3}]))
                .await
        )["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let stats = ok(f.sdk("runs.stats", json!([run])).await);
    assert_eq!(stats["assistantMessages"], 4);
    assert_eq!(
        stats["inputTokens"],
        json!({"total":30,"contributingMessages":3})
    );
    assert_eq!(stats["outputTokens"]["total"], 0);
    assert_eq!(stats["costUsd"]["total"], 0.75);
    assert!(stats["runWallSeconds"].is_null());
    assert_eq!(
        ok(f.sdk("sessions.stats", json!([session])).await)["costUsd"],
        stats["costUsd"]
    );
    let foreign = Uuid::new_v4();
    for method in ["runs.get", "runs.stats", "runs.outputs", "sessions.stats"] {
        assert_eq!(f.sdk(method, json!([foreign])).await["status"], 400);
    }
}

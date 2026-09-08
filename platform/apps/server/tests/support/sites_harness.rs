//! Real Sites harness -> leased Platform adapter -> isolated cells -> real Sites service.
use super::*;
use ::sites_harness::SitesHarness;
use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use harness_runtime::{Execution, Harness, Signals};
use llm_client::{
    CompletionRequest, CompletionResponse, LlmClient, LlmClientConfig, Run, RunState,
};
use llm_contracts::*;
use platform_runtime_client::{PlatformClient, RunClient};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use tokio::sync::{Mutex, watch};
#[derive(Default)]
struct Script {
    responses: VecDeque<RunState>,
    requests: Vec<CompletionRequest>,
    receipts: BTreeMap<String, (CompletionRequest, Run)>,
    jobs: BTreeMap<Uuid, Run>,
    lose_submit: bool,
    aborts: usize,
}
async fn setup(
    f: &mut Fixture,
    responses: Vec<RunState>,
) -> (SitesHarness, Arc<Mutex<Script>>, PlatformClient) {
    setup_with_browser(f, responses, None).await
}
async fn setup_with_browser(
    f: &mut Fixture,
    responses: Vec<RunState>,
    browser: Option<::sites_harness::Browser>,
) -> (SitesHarness, Arc<Mutex<Script>>, PlatformClient) {
    let schema: Value = sqlx::query_scalar("select config_schema from harnesses where id='sites'")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(schema, ::sites_harness::config_schema());
    let models: Value =
        sqlx::query_scalar("select supported_models from harnesses where id='sites'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(models, json!(::sites_harness::supported_models()));
    super::sites_authoring::setup(f).await;
    sqlx::query("update harnesses set config_schema=$1,default_config=$2,supported_models=$3 where id='sites'")
        .bind(schema).bind(json!({"reasoning_level":"high","siteId":null})).bind(models).execute(&f.pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let script = Arc::new(Mutex::new(Script {
        responses: responses.into(),
        ..Default::default()
    }));
    let submit = script.clone();
    let get_job = script.clone();
    let abort = script.clone();
    let app = Router::new()
        .route(
            "/v1/llm/runs",
            post(
                move |headers: HeaderMap, Json(request): Json<CompletionRequest>| {
                    let script = submit.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer sites-llm-test-token");
                        let key = headers["idempotency-key"].to_str().unwrap().to_owned();
                        let mut script = script.lock().await;
                        let result = if let Some((saved, result)) = script.receipts.get(&key) {
                            assert_eq!(saved, &request);
                            result.clone()
                        } else {
                            let mut state = script
                                .responses
                                .pop_front()
                                .expect("unexpected model generation");
                            let run_id = Uuid::now_v7();
                            if let RunState::Succeeded(result) = &mut state {
                                result.request_id = run_id;
                            }
                            let result = Run {
                                run_id,
                                state,
                                created_at: chrono::Utc::now(),
                                completed_at: None,
                                expires_at: None,
                            };
                            script.requests.push(request.clone());
                            script.jobs.insert(run_id, result.clone());
                            script.receipts.insert(key, (request, result.clone()));
                            result
                        };
                        if std::mem::take(&mut script.lose_submit) {
                            StatusCode::BAD_GATEWAY.into_response()
                        } else {
                            Json(result).into_response()
                        }
                    }
                },
            ),
        )
        .route(
            "/v1/llm/runs/{id}",
            get(move |Path(id): Path<Uuid>| {
                let script = get_job.clone();
                async move { Json(script.lock().await.jobs[&id].clone()) }
            }),
        )
        .route(
            "/v1/llm/runs/{id}/abort",
            post(move |Path(id): Path<Uuid>| {
                let script = abort.clone();
                async move {
                    let mut script = script.lock().await;
                    script.aborts += 1;
                    let job = script.jobs.get_mut(&id).unwrap();
                    job.state = RunState::Aborted;
                    Json(job.clone())
                }
            }),
        );
    f.tasks.push(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let mut config = LlmClientConfig::new(url.parse().unwrap(), "sites-llm-test-token");
    config.allow_insecure_http = true;
    let guest = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("code-mode-runtime");
    assert!(guest.is_file(), "Build code-mode-runtime first");
    let harness = SitesHarness::new(LlmClient::new(config).unwrap(), guest, None, browser);
    let (worker, _) = super::sites_authoring::worker(f).await;
    (harness, script, worker)
}
async fn session(f: &Fixture, worker: &PlatformClient, site: Option<Uuid>) -> RunClient {
    let reply=f.sdk("sessions.create",json!([{"harnessId":"sites","accountId":f.account,"config":{"siteId":site,"model":{"provider":"openai","id":"gpt-5.6-terra"}},"initialInput":user("Do the task")},{"idempotencyKey":Uuid::now_v7().to_string()}])).await;
    assert_eq!(reply["status"], 200, "{reply}");
    super::capabilities::claim(
        worker,
        serde_json::from_value(reply["body"]["run"]["id"].clone()).unwrap(),
    )
    .await
}
async fn activate(harness: &SitesHarness, client: RunClient) {
    let (_send, signals) = watch::channel(Signals::default());
    tokio::time::timeout(
        Duration::from_secs(35),
        harness.run(Execution { client, signals }),
    )
    .await
    .unwrap()
    .unwrap();
}
async fn status(f: &Fixture, id: Uuid) -> String {
    sqlx::query_scalar("select status from runs where id=$1")
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap()
}
async fn messages(f: &Fixture, id: Uuid) -> Vec<Value> {
    sqlx::query_scalar("select m.message from session_messages sm join messages m on m.id=sm.message_id where sm.run_id=$1 order by sm.revision").bind(id).fetch_all(&f.pool).await.unwrap()
}
fn user(text: &str) -> Value {
    json!({"role":"user","id":Uuid::now_v7().to_string(),"timestamp":1,"content":[{"type":"text","content":text}]})
}
fn response(content: Vec<AssistantContent>, stop_reason: StopReason) -> RunState {
    RunState::Succeeded(Box::new(CompletionResponse {
        request_id: Uuid::now_v7(),
        account_id: Uuid::now_v7(),
        message: AssistantMessage {
            id: MessageId::new(Uuid::now_v7().to_string()).unwrap(),
            model: ModelRef {
                provider: ProviderId::new("openai").unwrap(),
                id: ModelId::new("gpt-5.6-terra").unwrap(),
                name: None,
            },
            usage: None,
            duration_ms: 1,
            native_message: json!({"preserved":true}),
            content,
            stop_reason,
            timestamp: Timestamp(1),
        },
    }))
}
fn call(name: &str, args: Value) -> RunState {
    response(
        vec![AssistantContent::ToolCall {
            name: name.into(),
            arguments: ToolArguments::Object(args.as_object().unwrap().clone()),
            tool_call_id: ToolCallId::new(Uuid::now_v7().to_string()).unwrap(),
        }],
        StopReason::ToolUse,
    )
}
fn code(source: &str) -> RunState {
    call("code_mode", json!({"source":source}))
}
fn done() -> RunState {
    response(
        vec![AssistantContent::Response {
            response: TextContent {
                content: "Done".into(),
                metadata: None,
            },
        }],
        StopReason::Stop,
    )
}
async fn count_bindings(f: &Fixture) -> i64 {
    sqlx::query_scalar("select count(*) from site_authoring_bindings")
        .fetch_one(&f.pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn research_only_and_ambiguous_model_submit_recover_without_site(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let (harness,script,worker)=setup(&mut f,vec![code("text(await ctx.platform.environments.list()); text(await ctx.platform.harnesses.list()); text(await ctx.platform.accounts.list());"),done()]).await;
    let run = session(&f, &worker, None).await;
    let id = run.lease().run_id;
    script.lock().await.lose_submit = true;
    // LLM client may retry internally; either way the fake gateway enforces exact receipts.
    let (_send, signals) = watch::channel(Signals::default());
    if harness
        .run(Execution {
            client: run,
            signals,
        })
        .await
        .is_err()
    {
        sqlx::query(
            "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
        )
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
        let (replacement, _) = super::sites_authoring::worker(&mut f).await;
        activate(&harness, super::capabilities::claim(&replacement, id).await).await;
    }
    assert_eq!(status(&f, id).await, "completed");
    assert_eq!(count_bindings(&f).await, 0);
    let script = script.lock().await;
    assert_eq!(script.requests.len(), 2);
    let request = &script.requests[0].request;
    assert!(
        request
            .instructions
            .as_ref()
            .unwrap()
            .contains("Environments and execution")
    );
    assert!(
        request
            .instructions
            .as_ref()
            .unwrap()
            .contains("Backend handler SDK reference")
    );
    assert_eq!(request.tools.len(), 4);
    let text = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(text.contains("completed"));
    assert!(!text.contains("sites-llm-test-token"));
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn live_edit_backend_data_and_output(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let source = r#"
const current = await ctx.sites.read();
await ctx.sites.execute({sql:'create table harness_checks(value integer)'});
await ctx.sites.execute({sql:'insert into harness_checks values (7)'});
const frontend = '<!doctype html><html><body><h1>Sites harness</h1></body></html>\n';
const backend = "export default async (r,ctx) => ({status:200,body:await ctx.db.query('select value from harness_checks')});\n";
const section = (path,old,next) => '*** Update File: '+path+'\n@@\n'+old.trimEnd().split('\n').map(l=>'-'+l).join('\n')+'\n'+next.trimEnd().split('\n').map(l=>'+'+l).join('\n')+'\n';
text(await ctx.sites.applyPatch({patch:'*** Begin Patch\n'+section('index.html',current.files.frontend,frontend)+section('backend.js',current.files.backend,backend)+'*** End Patch'}));
text(await ctx.sites.invoke({method:'POST',path:'/checks'}));
text(await ctx.sites.query({sql:'select value from harness_checks'}));
"#;
    let site = f.site;
    let (harness, _, worker) = setup(&mut f, vec![code(source), done()]).await;
    let run = session(&f, &worker, Some(site)).await;
    let id = run.lease().run_id;
    activate(&harness, run.clone()).await;
    assert_eq!(status(&f, id).await, "completed");
    let transcript = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(transcript.contains("succeeded"), "{transcript}");
    assert!(transcript.contains("value\\\":7"), "{transcript}");
    let output = f.sdk("runs.outputs", json!([id])).await;
    assert_eq!(output["body"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(output["body"]["items"][0]["name"], "site");
    assert_eq!(count_bindings(&f).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn new_site_timer_wait_and_steering(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let (harness, script, worker) = setup(
        &mut f,
        vec![
            code("try { text(await ctx.sites.read()); } catch(e) { text(String(e)); }"),
            call("wait", json!({"seconds":3600})),
            code(r#"const current=await ctx.sites.read(); const lines=current.files.frontend.trimEnd().split('\n').map(line=>'-'+line).join('\n'); text(await ctx.sites.applyPatch({patch:'*** Begin Patch\n*** Update File: index.html\n@@\n'+lines+'\n+<!doctype html><html><body><h1>Blue dashboard</h1></body></html>\n*** End Patch'}));"#),
            done(),
        ],
    )
    .await;
    let run = session(&f, &worker, None).await;
    let id = run.lease().run_id;
    activate(&harness, run).await;
    assert_eq!(status(&f, id).await, "waiting");
    assert_eq!(count_bindings(&f).await, 1);
    application_post(
        &f,
        &format!("/api/runs/{id}/inputs"),
        json!({"kind":"user_message","message":user("Use a blue theme")}),
    )
    .await;
    let (replacement, _) = super::sites_authoring::worker(&mut f).await;
    activate(&harness, super::capabilities::claim(&replacement, id).await).await;
    assert_eq!(status(&f, id).await, "completed");
    assert_eq!(count_bindings(&f).await, 1);
    let transcript = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(transcript.contains("succeeded"), "{transcript}");
    assert!(
        serde_json::to_string(&script.lock().await.requests.last().unwrap())
            .unwrap()
            .contains("Use a blue theme")
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn drain_interrupts_cell_and_keeps_accepted_effect_once(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let site = f.site;
    let (harness,_,worker)=setup(&mut f,vec![code("await ctx.sites.execute({sql:'create table durable_effect(value integer)'}); while(true){};"),done()]).await;
    let run = session(&f, &worker, Some(site)).await;
    let id = run.lease().run_id;
    let (send, signals) = watch::channel(Signals::default());
    let task = tokio::spawn(harness.run(Execution {
        client: run,
        signals,
    }));
    tokio::time::timeout(Duration::from_secs(15),async {loop {
        let count:i64=sqlx::query_scalar("select count(*) from session_state where namespace=$1 and value->>'tool'='sites.execute' and value->>'status'='succeeded'").bind(format!("code_mode.{id}")).fetch_one(&f.pool).await.unwrap();
        if count==1 {break;}tokio::time::sleep(Duration::from_millis(20)).await;
    }}).await.unwrap();
    send.send(Signals {
        draining: true,
        ..Default::default()
    })
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(status(&f, id).await, "ready");
    let (replacement, _) = super::sites_authoring::worker(&mut f).await;
    activate(&harness, super::capabilities::claim(&replacement, id).await).await;
    assert_eq!(status(&f, id).await, "completed");
    let count: i64 = sqlx::query_scalar(
        "select count(*) from site_authoring_calls where run_id=$1 and method='sites.execute'",
    )
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    assert!(
        serde_json::to_string(&messages(&f, id).await)
            .unwrap()
            .contains("interrupted")
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn abort_reconciles_model_and_terminates_run(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let (harness, script, worker) = setup(&mut f, vec![RunState::Running]).await;
    let run = session(&f, &worker, None).await;
    let id = run.lease().run_id;
    let (send, signals) = watch::channel(Signals::default());
    let task = tokio::spawn(harness.run(Execution {
        client: run,
        signals,
    }));
    tokio::time::timeout(Duration::from_secs(10), async {
        while script.lock().await.requests.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    application_post(
        &f,
        &format!("/api/runs/{id}/abort"),
        json!({"reason":"Stop"}),
    )
    .await;
    send.send(Signals {
        abort_requested: true,
        ..Default::default()
    })
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(status(&f, id).await, "aborted");
    assert_eq!(script.lock().await.aborts, 1);
    assert_eq!(count_bindings(&f).await, 0);
}

async fn application_post(f: &Fixture, path: &str, body: Value) -> Value {
    let response = f
        .client
        .post(format!("{}{path}", f.url))
        .bearer_auth(ADMIN)
        .header("Idempotency-Key", Uuid::now_v7().to_string())
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let value: Value = response.json().await.unwrap();
    assert!(status.is_success(), "{value}");
    value
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires PostgreSQL, Sites/code-mode binaries, npm browser setup and SITES_TEST_BROWSER_NODE"]
async fn browser_probe_uses_bound_live_site(pool: PgPool) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bind = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut f =
        Fixture::with_content(pool, None, Some((bind, "http://127.0.0.1:3102".into()))).await;
    let site = f.site;
    let browser = ::sites_harness::Browser {
        node: std::env::var_os("SITES_TEST_BROWSER_NODE")
            .expect("Set SITES_TEST_BROWSER_NODE")
            .into(),
        script: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/harnesses/sites/browser/verify.mjs")
            .canonicalize()
            .unwrap(),
    };
    let (harness, _, worker) = setup_with_browser(
        &mut f,
        vec![
            code("text(await tools['browser.verify']({selectors:['body']}));"),
            done(),
        ],
        Some(browser),
    )
    .await;
    let run = session(&f, &worker, Some(site)).await;
    let id = run.lease().run_id;
    activate(&harness, run).await;
    let transcript = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(transcript.contains("visible\\\":true"), "{transcript}");
    assert!(transcript.contains("Frontend DOM only"), "{transcript}");
    assert!(transcript.contains("errors\\\":[]"), "{transcript}");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn crash_recovers_abandoned_cell_without_replaying_accepted_sql(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let site = f.site;
    let (harness,_,worker)=setup(&mut f,vec![code("await ctx.sites.execute({sql:'create table crash_effect(value integer)'}); while(true){};"),done()]).await;
    let run = session(&f, &worker, Some(site)).await;
    let id = run.lease().run_id;
    let (_send, signals) = watch::channel(Signals::default());
    let task = tokio::spawn(harness.run(Execution {
        client: run,
        signals,
    }));
    tokio::time::timeout(Duration::from_secs(15),async {loop {
        let count:i64=sqlx::query_scalar("select count(*) from session_state where namespace=$1 and value->>'tool'='sites.execute' and value->>'status'='succeeded'").bind(format!("code_mode.{id}")).fetch_one(&f.pool).await.unwrap();
        if count==1 {break;}tokio::time::sleep(Duration::from_millis(20)).await;
    }}).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(id)
    .execute(&f.pool)
    .await
    .unwrap();
    let (replacement, _) = super::sites_authoring::worker(&mut f).await;
    activate(&harness, super::capabilities::claim(&replacement, id).await).await;
    assert_eq!(status(&f, id).await, "completed");
    let count: i64 = sqlx::query_scalar(
        "select count(*) from site_authoring_calls where run_id=$1 and method='sites.execute'",
    )
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let transcript = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(transcript.contains("OWNER_REPLACED"), "{transcript}");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn orchestration_creates_child_and_waits_for_exact_run(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let source = format!(
        "return await ctx.platform.sessions.create({});",
        json!({"harnessId":"sites","accountId":f.account,"config":{"model":{"provider":"openai","id":"gpt-5.6-terra"}},"title":"Orchestrated child","initialInput":user("Child experiment")})
    );
    let (harness, script, worker) =
        setup(&mut f, vec![code(&source), RunState::Running, done()]).await;
    let run = session(&f, &worker, None).await;
    let id = run.lease().run_id;
    let (_send, signals) = watch::channel(Signals::default());
    let task = tokio::spawn(harness.run(Execution {
        client: run,
        signals,
    }));
    tokio::time::timeout(Duration::from_secs(15), async {
        while script.lock().await.requests.len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let child:Uuid=sqlx::query_scalar("select r.id from runs r join sessions s on s.id=r.session_id where s.title='Orchestrated child'").fetch_one(&f.pool).await.unwrap();
    {
        let mut script = script.lock().await;
        let job = script
            .jobs
            .values_mut()
            .find(|j| matches!(j.state, RunState::Running))
            .unwrap();
        job.state = call("wait", json!({"seconds":3600,"runIds":[child]}));
        if let RunState::Succeeded(result) = &mut job.state {
            result.request_id = job.run_id;
        }
    }
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(status(&f, id).await, "waiting");
    let child_run = super::capabilities::claim(&worker, child).await;
    let context = child_run.context(&Default::default()).await.unwrap();
    let mut commit = platform_runtime_client::types::Commit::new(
        context.run.run.version,
        context.session.current_revision,
    );
    commit.disposition = platform_runtime_client::types::Disposition::Failed {
        error: json!({"kind":"experiment_result","message":"Known failed experiment"})
            .as_object()
            .unwrap()
            .clone(),
    };
    child_run
        .commit(&platform_runtime_client::Command::new(
            platform_runtime_client::RequestKey::new("child-result").unwrap(),
            commit,
        ))
        .await
        .unwrap();
    f.runtime.reconcile_once().await.unwrap();
    let (replacement, _) = super::sites_authoring::worker(&mut f).await;
    activate(&harness, super::capabilities::claim(&replacement, id).await).await;
    assert_eq!(status(&f, id).await, "completed");
    assert_eq!(count_bindings(&f).await, 0);
    let waits: Value = sqlx::query_scalar("select to_jsonb(w) from run_waits w where run_id=$1")
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(waits["status"], "satisfied");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn timer_wait_resumes_without_duplicate_tool_result(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let (harness, _, worker) =
        setup(&mut f, vec![call("wait", json!({"seconds":1})), done()]).await;
    let run = session(&f, &worker, None).await;
    let id = run.lease().run_id;
    activate(&harness, run).await;
    assert_eq!(status(&f, id).await, "waiting");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    f.runtime.reconcile_once().await.unwrap();
    let (replacement, _) = super::sites_authoring::worker(&mut f).await;
    activate(&harness, super::capabilities::claim(&replacement, id).await).await;
    assert_eq!(status(&f, id).await, "completed");
    let messages = messages(&f, id).await;
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["role"] == "tool_result")
            .count(),
        1
    );
}

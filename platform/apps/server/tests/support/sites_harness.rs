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
                                for part in &mut result.message.content {
                                    if let AssistantContent::ToolCall {
                                        name,
                                        arguments: ToolArguments::Object(args),
                                        ..
                                    } = part
                                    {
                                        if name == "wait"
                                            && args.get("cell_id") == Some(&json!("$latest"))
                                        {
                                            let cell = request
                                                .request
                                                .messages
                                                .iter()
                                                .rev()
                                                .find_map(|message| {
                                                    let Message::ToolResult(result) = message
                                                    else {
                                                        return None;
                                                    };
                                                    result.content.iter().find_map(|part| {
                                                        let ContentPart::Text(text) = part else {
                                                            return None;
                                                        };
                                                        text.content
                                                            .strip_prefix(
                                                                "Script running with cell ID ",
                                                            )
                                                            .and_then(|s| s.lines().next())
                                                            .map(str::to_owned)
                                                    })
                                                })
                                                .expect("wait requires a yielded cell");
                                            args.insert("cell_id".into(), json!(cell));
                                        }
                                    }
                                }
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
    let harness = SitesHarness::new(
        LlmClient::new(config).unwrap(),
        guest,
        ::sites_harness::BrowserConfig {
            node: std::env::var_os("SITES_TEST_BROWSER_NODE")
                .map(Into::into)
                .unwrap_or_else(|| "/usr/bin/node".into()),
            script: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packages/harnesses/sites/browser/runtime.mjs"),
        },
    );
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
    response(
        vec![AssistantContent::ToolCall {
            name: "exec".into(),
            arguments: ToolArguments::String(source.into()),
            tool_call_id: ToolCallId::new(Uuid::now_v7().to_string()).unwrap(),
        }],
        StopReason::ToolUse,
    )
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
async fn cell_only_and_ambiguous_model_submit_recover_without_site(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let (harness, script, worker) = setup(
        &mut f,
        vec![code("text(ALL_TOOLS); text(typeof ctx);"), done()],
    )
    .await;
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
            .contains("Your role and what Sites are for")
    );
    assert_eq!(
        request.instructions.as_deref().unwrap(),
        include_str!("../../../../packages/harnesses/sites/src/system_prompt.md")
    );
    assert_eq!(request.tools.len(), 2);
    let text = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(text.contains("completed"));
    assert!(!text.contains("sites-llm-test-token"));
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn first_parallel_access_waits_for_provisioning_and_keeps_rejection_details(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let source = r#"
const [metadata, frontend, backend] = await Promise.all([
  tools.metadata(), tools.read({file_path:'index.html'}), tools.read({file_path:'backend.js'})
]);
if (!metadata.id || !frontend.content || !backend.content) throw new Error('first access failed');
await tools.sql({sql:'CREATE TABLE entries(id INTEGER PRIMARY KEY, title TEXT)'});
await tools.sql({sql:'CREATE INDEX entries_title ON entries(title DESC, id DESC)'});
let rejected = false;
try { await tools.apply_patch('*** Begin Patch\n*** Delete File: other.js\n*** End Patch'); }
catch (error) { rejected = true; if (!String(error).includes('Only index.html and backend.js may be patched')) throw error; }
if (!rejected) throw new Error('unsupported patch was accepted');
const originalBackend = backend.content;
const replacement = 'export default async () => ({status:200,body:{replaced:true}});';
const result = await tools.apply_patch('*** Begin Patch\n*** Delete File: backend.js\n*** Add File: backend.js\n+' + replacement + '\n*** End Patch');
if (JSON.stringify(result) !== '{}') throw new Error('patch result changed');
if (!(await tools.invoke({method:'GET',path:'/'})).response.body.replaced) throw new Error('replacement did not activate');
await tools.apply_patch('*** Begin Patch\n*** Delete File: backend.js\n*** End Patch');
if ((await tools.invoke({method:'GET',path:'/'})).response.status !== 404) throw new Error('delete did not reset backend');
await tools.apply_patch('*** Begin Patch\n*** Add File: backend.js\n' + originalBackend.trimEnd().split('\n').map(line=>'+'+line).join('\n') + '\n*** End Patch');
text('first access, index creation, and public rejection details passed');
"#;
    let (harness, _, worker) = setup(&mut f, vec![code(source), done()]).await;
    let run = session(&f, &worker, None).await;
    let id = run.lease().run_id;
    activate(&harness, run).await;
    let saved = messages(&f, id).await;
    assert_eq!(status(&f, id).await, "completed");
    assert!(
        saved
            .iter()
            .filter(|m| m["role"] == "tool_result")
            .all(|m| m["outcome"]["status"] == "success"),
        "{saved:?}"
    );
    assert!(
        serde_json::to_string(&saved)
            .unwrap()
            .contains("public rejection details passed")
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn native_provider_messages_are_saved_and_sent_unchanged(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let mut replies = Vec::new();
    let native_messages = vec![
        json!({
            "output":[{"type":"reasoning","encrypted_content":"opaque-openai-item"}],
            "instructions":"full OpenAI instructions",
            "tools":[{"type":"function","name":"example"}],
            "usage":{"input_tokens":123}
        }),
        json!({
            "type":"chatgpt_response_stream",
            "output":[{"type":"reasoning","encrypted_content":"opaque-chatgpt-item"}],
            "response":{
                "instructions":"full ChatGPT instructions",
                "tools":[{"type":"function","name":"example"}],
                "usage":{"input_tokens":456}
            }
        }),
    ];
    for (provider, native) in ["openai", "chatgpt"].into_iter().zip(&native_messages) {
        let mut reply = code("text('small result')");
        if let RunState::Succeeded(result) = &mut reply {
            result.message.model.provider = ProviderId::new(provider).unwrap();
            result.message.native_message = native.clone();
        }
        replies.push(reply);
    }
    replies.push(done());
    let (harness, script, worker) = setup(&mut f, replies).await;
    let run = session(&f, &worker, None).await;
    let id = run.lease().run_id;
    activate(&harness, run).await;
    assert_eq!(status(&f, id).await, "completed");
    let saved = messages(&f, id).await;
    let saved_native: Vec<_> = saved
        .iter()
        .filter(|m| m["role"] == "assistant")
        .take(native_messages.len())
        .map(|m| m["native_message"].clone())
        .collect();
    assert_eq!(saved_native, native_messages);
    let script = script.lock().await;
    assert_eq!(script.requests.len(), 3);
    for (index, request) in script.requests.iter().enumerate() {
        let sent_native: Vec<_> = request
            .request
            .messages
            .iter()
            .filter_map(|m| match m {
                Message::Assistant(m) => Some(m.native_message.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(sent_native, native_messages[..index]);
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn live_edit_backend_data_and_output(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let source = r#"
const metadata = await tools.metadata();
if ('files' in metadata || !metadata.id || !metadata.name) throw new Error('Incorrect metadata shape');
text(metadata);
if (typeof ctx !== 'undefined') throw new Error('Unexpected SDK context');
if (JSON.stringify(ALL_TOOLS.map(t=>t.name).sort()) !== JSON.stringify(['apply_patch','browser','invoke','metadata','read','sql'])) throw new Error('Incorrect tools');
const current = {files:{frontend:(await tools.read({file_path:'index.html'})).content,backend:(await tools.read({file_path:'backend.js'})).content}};
await tools.sql({sql:'create table harness_checks(value integer)'});
const insert = {sql:'insert into harness_checks values (?) returning value',params:[7],idempotency_key:'one-insert'};
const inserted = await tools.sql(insert);
if (JSON.stringify(inserted) !== JSON.stringify(await tools.sql(insert))) throw new Error('Receipt replay mismatch');
if (inserted.changes !== 1 || inserted.rows[0].value !== 7 || inserted.read_only !== false || 'site_id' in inserted) throw new Error('Incorrect SQL shape');
const schema = await tools.sql({sql:"select name from sqlite_schema where name='harness_checks'"});
if (schema.rows.length !== 1 || !schema.read_only) throw new Error('Schema inspection failed');
const frontend = '<!doctype html><html><body><h1>Sites harness</h1></body></html>\n';
const backend = "export default async (r,ctx) => ({status:200,body:await ctx.db.query('select value from harness_checks')});\n";
const section = (path,old,next) => '*** Update File: '+path+'\n@@\n'+old.trimEnd().split('\n').map(l=>'-'+l).join('\n')+'\n'+next.trimEnd().split('\n').map(l=>'+'+l).join('\n')+'\n';
const patched = await tools.apply_patch('*** Begin Patch\n'+section('index.html',current.files.frontend,frontend)+section('backend.js',current.files.backend,backend)+'*** End Patch');
if (JSON.stringify(patched) !== '{}') throw new Error('Patch result is not empty');
text(patched);
text(await tools.invoke({method:'POST',path:'/checks'}));
text(await tools.sql({sql:'select value from harness_checks'}));
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
            code("try { text(await tools.metadata()); } catch(e) { text(String(e)); }"),
            code("notify('ready for steering'); await new Promise(r => setTimeout(r,1000));"),
            code(r#"const current=await tools.read({file_path:'index.html'}); const lines=current.content.trimEnd().split('\n').map(line=>'-'+line).join('\n'); text(await tools.apply_patch('*** Begin Patch\n*** Update File: index.html\n@@\n'+lines+'\n+<!doctype html><html><body><h1>Blue dashboard</h1></body></html>\n*** End Patch'));"#),
            done(),
        ],
    )
    .await;
    let run = session(&f, &worker, None).await;
    let id = run.lease().run_id;
    let (_send, signals) = watch::channel(Signals::default());
    let task = tokio::spawn(harness.run(Execution {
        client: run,
        signals,
    }));
    tokio::time::timeout(Duration::from_secs(15), async {
        while !serde_json::to_string(&messages(&f, id).await)
            .unwrap()
            .contains("ready for steering")
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(count_bindings(&f).await, 1);
    application_post(
        &f,
        &format!("/api/runs/{id}/inputs"),
        json!({"kind":"user_message","message":user("Use a blue theme")}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(status(&f, id).await, "completed");
    assert_eq!(count_bindings(&f).await, 1);
    let transcript = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(transcript.contains("Script completed"), "{transcript}");
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
    let (harness,_,worker)=setup(&mut f,vec![code("await tools.sql({sql:'create table durable_effect(value integer)'}); while(true){};"),done()]).await;
    let run = session(&f, &worker, Some(site)).await;
    let id = run.lease().run_id;
    let (send, signals) = watch::channel(Signals::default());
    let task = tokio::spawn(harness.run(Execution {
        client: run,
        signals,
    }));
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "select count(*) from site_authoring_calls where run_id=$1 and method='sites.sql'",
            )
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
            if count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
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
        "select count(*) from site_authoring_calls where run_id=$1 and method='sites.sql'",
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
#[ignore = "requires PostgreSQL and built Sites/code-mode binaries"]
async fn browser_rejects_retired_input_without_creating_a_site(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let (harness, _, worker) = setup(&mut f, vec![
        code("try { await tools.browser({code:'throw new Error()'}); } catch(e) { text({code:e.code,uncertain:e.uncertain}); }"),
        done()
    ]).await;
    let run = session(&f, &worker, None).await;
    let id = run.lease().run_id;
    activate(&harness, run).await;
    let transcript = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(transcript.contains("INVALID_TOOL_INPUT"), "{transcript}");
    assert_eq!(count_bindings(&f).await, 0);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires PostgreSQL, built binaries, and SITES_TEST_BROWSER_NODE with installed Playwright Chromium"]
async fn browser_real_frontend_backend_sql_screenshot_and_pinned_reload(pool: PgPool) {
    assert!(
        std::env::var_os("SITES_TEST_BROWSER_NODE").is_some(),
        "Set SITES_TEST_BROWSER_NODE to an absolute Node path"
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut f =
        Fixture::with_content(pool, None, Some((address, "http://127.0.0.1:1".into()))).await;
    let source = r#"
const section = (path,old,next) => '*** Update File: '+path+'\n@@\n'+old.trimEnd().split('\n').map(l=>'-'+l).join('\n')+'\n'+next.trimEnd().split('\n').map(l=>'+'+l).join('\n')+'\n';
const frontend = '<!doctype html><h1>Browser integration</h1><button onclick="window.saved=callBackend(\'/save\',{value:7}).then(v=>{this.textContent=\'Saved\';return v;})">Save</button>';
const backend = "export default async (r,ctx) => { if(r.path==='/save') await ctx.db.execute('insert into browser_values values (?)',[r.body.value]); return {status:200,body:{version:1,rows:await ctx.db.query('select value from browser_values')}}; };";
await tools.sql({sql:'create table browser_values(value integer)'});
await tools.apply_patch('*** Begin Patch\n'+section('index.html',(await tools.read({file_path:'index.html'})).content,frontend)+section('backend.js',(await tools.read({file_path:'backend.js'})).content,backend)+'*** End Patch');
text(await tools.browser({action:'reload'}));
text(await tools.browser({action:'evaluate',code:"document.querySelector('button').click(); return await window.saved;"}));
const shot = await tools.browser({action:'screenshot'}); image(shot.image);
await tools.apply_patch('*** Begin Patch\n'+section('backend.js',backend,backend.replace('version:1','version:2'))+'*** End Patch');
const old = await tools.browser({action:'evaluate',code:"return await callBackend('/read');"});
if(old.result.version !== 1) throw new Error('Loaded preview backend changed without reload');
store('old',old.result);
"#;
    let next = r#"
const state = await tools.browser({action:'evaluate',code:"return document.querySelector('button').textContent;"});
if(state.result !== 'Saved') throw new Error('Page state did not persist across cells');
text(await tools.browser({action:'reload'}));
const fresh = await tools.browser({action:'evaluate',code:"return await callBackend('/read');"});
if(fresh.result.version !== 2) throw new Error('Reload did not load latest backend');
const rows = await tools.sql({sql:'select value from browser_values'});
if(rows.rows.length !== 1 || rows.rows[0].value !== 7) throw new Error('Frontend did not write real SQLite data exactly once');
text({browserVerified:true,old:load('old'),fresh:fresh.result});
"#;
    let site = f.site;
    let (harness, script, worker) = setup(&mut f, vec![code(source), code(next), done()]).await;
    let run = session(&f, &worker, Some(site)).await;
    let id = run.lease().run_id;
    activate(&harness, run).await;
    assert_eq!(status(&f, id).await, "completed");
    let transcript = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(transcript.contains("browserVerified"), "{transcript}");
    assert!(!transcript.contains("Script failed"), "{transcript}");
    assert!(!transcript.contains(SERVICE));
    assert!(!transcript.contains(CAP));
    let script = script.lock().await;
    assert!(script.requests.iter().any(|request| request.request.messages.iter().any(|message| {
        matches!(message, Message::ToolResult(result) if result.content.iter().any(|part| matches!(part, ContentPart::Image(_))))
    })), "Screenshot did not reach the next model request as an image");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires PostgreSQL, built binaries, and SITES_TEST_BROWSER_NODE with installed Playwright Chromium"]
async fn browser_cell_cancellation_requires_reload_without_replaying_backend_effects(pool: PgPool) {
    assert!(std::env::var_os("SITES_TEST_BROWSER_NODE").is_some());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut f =
        Fixture::with_content(pool, None, Some((address, "http://127.0.0.1:1".into()))).await;
    let site = f.site;
    let prepare = r#"
await tools.sql({sql:'create table browser_cancel(value integer)'});
const before=(await tools.read({file_path:'backend.js'})).content;
const backend="export default async (r,ctx) => { if(r.path==='/entered') await ctx.db.execute('insert into browser_cancel values (1)'); return {status:200,body:{ok:true}}; };";
await tools.apply_patch('*** Begin Patch\n*** Update File: backend.js\n@@\n'+before.trimEnd().split('\n').map(l=>'-'+l).join('\n')+'\n+'+backend+'\n*** End Patch');
text(await tools.browser({action:'reload'}));
"#;
    let (harness, _, worker) = setup(&mut f, vec![
        code(prepare),
        code("// @exec: {\"yield_time_ms\":1}\nawait tools.browser({action:'evaluate',code:'window.cancelMark=1; await callBackend(\"/entered\"); await new Promise(()=>{});'});"),
        code("let entered=false; for(let i=0;i<50;i++){const r=await tools.sql({sql:'select count(*) as n from browser_cancel'}); if(r.rows[0].n===1){entered=true;break;} await new Promise(r=>setTimeout(r,100));} if(!entered) throw new Error('Browser never reached backend'); text('entered');"),
        call("wait",json!({"cell_id":"$latest","terminate":true})),
        code("await new Promise(r=>setTimeout(r,500)); let reset=false; try{await tools.browser({action:'evaluate',code:'return 1;'});}catch(e){reset=e.code==='BROWSER_RESET_REQUIRED';} if(!reset) throw new Error('Cancellation retained the page'); await tools.browser({action:'reload'}); const fresh=await tools.browser({action:'evaluate',code:'return typeof window.cancelMark;'}); if(fresh.result!=='undefined') throw new Error('Reload retained old DOM state'); const rows=await tools.sql({sql:'select count(*) as n from browser_cancel'}); if(rows.rows[0].n!==1) throw new Error('Cancelled source replayed its write'); text('browserCancellationVerified');"),
        done(),
    ]).await;
    let run = session(&f, &worker, Some(site)).await;
    let id = run.lease().run_id;
    activate(&harness, run).await;
    let transcript = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(
        transcript.contains("browserCancellationVerified"),
        "{transcript}"
    );
    assert!(!transcript.contains("Script failed"), "{transcript}");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn crash_recovers_abandoned_cell_without_replaying_accepted_sql(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let site = f.site;
    let (harness, _, worker) = setup(
        &mut f,
        vec![
            code(
                "await tools.sql({sql:'create table crash_effect(value integer)'}); while(true){};",
            ),
            done(),
        ],
    )
    .await;
    let run = session(&f, &worker, Some(site)).await;
    let id = run.lease().run_id;
    let (_send, signals) = watch::channel(Signals::default());
    let task = tokio::spawn(harness.run(Execution {
        client: run,
        signals,
    }));
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "select count(*) from site_authoring_calls where run_id=$1 and method='sites.sql'",
            )
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
            if count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
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
        "select count(*) from site_authoring_calls where run_id=$1 and method='sites.sql'",
    )
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let transcript = serde_json::to_string(&messages(&f, id).await).unwrap();
    assert!(
        transcript.contains("Source was not replayed"),
        "{transcript}"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and built Sites/code-mode binaries"]
async fn timer_wait_resumes_without_duplicate_tool_result(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let (harness, _, worker) =
        setup(&mut f, vec![code("// @exec: {\"yield_time_ms\":1}\ntext('before timer'); await new Promise(r => setTimeout(r,1000)); text('after timer');"), call("wait",json!({"cell_id":"$latest"})), done()]).await;
    let run = session(&f, &worker, None).await;
    let id = run.lease().run_id;
    activate(&harness, run).await;
    assert_eq!(status(&f, id).await, "completed");
    let messages = messages(&f, id).await;
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["role"] == "tool_result")
            .count(),
        2
    );
    let outputs: Vec<_> = messages
        .iter()
        .filter(|m| m["role"] == "tool_result")
        .collect();
    let text = serde_json::to_string(&outputs).unwrap();
    assert_eq!(text.matches("before timer").count(), 1);
    assert_eq!(text.matches("after timer").count(), 1);
}

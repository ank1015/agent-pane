//! Real Platform HTTP -> real sites process -> sandboxed QuickJS -> Platform SDK.
//! Uses disposable PostgreSQL and filesystem state; never calls hosted gateways.
use axum::{Json, Router, routing::get};
use base64::{Engine, engine::general_purpose::STANDARD};
use platform_server::{
    providers::{LlmGatewayClient, ProviderService},
    runtime::RuntimeService,
    sites::{SitesClient, SitesService},
};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::task::JoinHandle;
use uuid::Uuid;
const ADMIN: &str = "sites-test-admin-token-01234567890123456789";
const CAP: &str = "sites-test-capability-token-01234567890123456789";
const TOKEN: &str = "sites-test-project-token-01234567890123456789";
const SERVICE: &str = "sites-test-service-token-01234567890123456789";
const HARNESS: &str = "basic-cc-tools-harness";
#[cfg(test)]
#[path = "support/capabilities.rs"]
mod capabilities;
#[cfg(all(test, unix))]
#[path = "support/code_mode.rs"]
mod code_mode;
#[cfg(all(test, unix))]
#[path = "support/remote.rs"]
mod remote;
struct Fixture {
    service: SitesService,
    pool: PgPool,
    root: tempfile::TempDir,
    child: Child,
    tasks: Vec<JoinHandle<()>>,
    client: Client,
    url: String,
    sites_url: String,
    project: Uuid,
    other: Uuid,
    site: Uuid,
    environment: Uuid,
    foreign_environment: Uuid,
    account: Uuid,
    release: Uuid,
    runtime: RuntimeService,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for task in &self.tasks {
            task.abort();
        }
    }
}
fn file(path: &str, text: &str) -> Value {
    json!({"path":path,"content_base64":STANDARD.encode(text),"sha256":format!("{:x}",Sha256::digest(text))})
}
impl Fixture {
    async fn new(pool: PgPool) -> Self {
        Self::with_code(pool, None).await
    }
    async fn with_code(pool: PgPool, code: Option<&str>) -> Self {
        Self::with_content(pool, code, None).await
    }
    async fn with_content(
        pool: PgPool,
        code: Option<&str>,
        content: Option<(String, String)>,
    ) -> Self {
        Self::with_remote_content(pool, code, content, None).await
    }
    async fn with_remote_content(
        pool: PgPool,
        code: Option<&str>,
        content: Option<(String, String)>,
        gateway: Option<&str>,
    ) -> Self {
        let project = Uuid::now_v7();
        let other = Uuid::now_v7();
        let site = Uuid::now_v7();
        let account = Uuid::now_v7();
        for id in [project, other] {
            sqlx::query("insert into projects(project_id,name) values($1,'Sites integration')")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("update harnesses set supported_models=$1 where id=$2")
            .bind(json!({"openai":["test-model"]}))
            .bind(HARNESS)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "insert into project_harnesses(project_id,harness_id,enabled) values($1,$2,true)",
        )
        .bind(project)
        .bind(HARNESS)
        .execute(&pool)
        .await
        .unwrap();
        let environment = Uuid::now_v7();
        let foreign_environment = Uuid::now_v7();
        for (id, owner) in [(environment, project), (foreign_environment, other)] {
            sqlx::query("insert into project_environments(id,project_id,name,type,machine_id,workspace_root,path) values($1,$2,'Workspace','machine',$3,'/work','.')").bind(id).bind(owner).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
        }
        let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider_url = format!("http://{}", provider_listener.local_addr().unwrap());
        let provider_task = tokio::spawn(async move {
            let record = move |id: Uuid| json!({"id":id,"name":"Test account","provider":"openai","status":"active","is_default":true,"created_at":"2026-09-05T00:00:00Z","updated_at":"2026-09-05T00:00:00Z","runtime_revision":1,"config":{},"credential":{"version":1,"encryption_key_version":1,"updated_at":"2026-09-05T00:00:00Z"}});
            let payload = json!({"accounts":[record(account),record(Uuid::now_v7())]});
            axum::serve(
                provider_listener,
                Router::new().route(
                    "/v1/admin/accounts",
                    get(move || {
                        let payload = payload.clone();
                        async move { Json(payload) }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let platform_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", platform_listener.local_addr().unwrap());
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let sites_address = port.local_addr().unwrap();
        drop(port);
        let sites_url = format!("http://{sites_address}");
        let root = tempfile::tempdir().unwrap();
        let binary = std::env::var_os("SITES_TEST_BINARY")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("platform-sites-service")
            });
        assert!(
            binary.exists(),
            "Build platform-sites-service before running this test"
        );
        let child = Command::new(binary)
            .env("SITES_DATA_DIR", root.path())
            .env("SITES_API_TOKEN", SERVICE)
            .env("SITES_BIND_ADDRESS", sites_address.to_string())
            .env(
                "SITES_CONTENT_BIND_ADDRESS",
                content
                    .as_ref()
                    .map(|c| c.0.as_str())
                    .unwrap_or("127.0.0.1:3103"),
            )
            .env(
                "SITES_CONTENT_ORIGIN",
                content
                    .as_ref()
                    .map(|c| format!("http://{}", c.0))
                    .unwrap_or_default(),
            )
            .env(
                "SITES_DASHBOARD_ORIGIN",
                content.as_ref().map(|c| c.1.as_str()).unwrap_or(""),
            )
            .env("SITES_PLATFORM_URL", &url)
            .env("SITES_PLATFORM_CAPABILITY_TOKEN", CAP)
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let client = Client::builder()
            .timeout(Duration::from_secs(40))
            .build()
            .unwrap();
        let providers = ProviderService::new(
            LlmGatewayClient::new(provider_url.parse().unwrap(), ADMIN, Duration::from_secs(3))
                .unwrap(),
        );
        let mut runtime = RuntimeService::new(pool.clone())
            .with_providers(providers.clone())
            .with_sites(SitesClient::new(sites_url.parse().unwrap(), SERVICE).unwrap());
        if let Some(url) = gateway {
            let mut config =
                execution_client::ExecutionClientConfig::new(url.parse().unwrap(), ADMIN);
            config.allow_insecure_http = true;
            runtime =
                runtime.with_execution(execution_client::ExecutionClient::new(config).unwrap());
        }
        let service = SitesService::new(
            pool.clone(),
            SitesClient::new(sites_url.parse().unwrap(), SERVICE).unwrap(),
            runtime.clone(),
            providers,
            CAP,
            ADMIN,
        )
        .unwrap();
        let app_service = service.clone();
        let app_service_pool = pool.clone();
        let task = tokio::spawn(async move {
            axum::serve(
                platform_listener,
                platform_server::sites::router(app_service.clone())
                    .merge(platform_server::projects::router(
                        platform_server::projects::ProjectService::new(app_service_pool.clone()),
                    ))
                    .merge(platform_server::runtime::router(RuntimeService::new(
                        app_service_pool,
                    ))),
            )
            .await
            .unwrap()
        });
        let mut f = Self {
            service,
            pool,
            root,
            child,
            tasks: vec![task, provider_task],
            client,
            url,
            sites_url,
            project,
            other,
            site,
            environment,
            foreign_environment,
            account,
            release: Uuid::nil(),
            runtime,
        };
        for _ in 0..100 {
            if f.client
                .get(format!("{}/readyz", f.sites_url))
                .bearer_auth(SERVICE)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            assert!(f.child.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        f.request(
            Method::PUT,
            &format!("/internal/site-access/{project}"),
            ADMIN,
            json!({"token":TOKEN,"enabled":true}),
            200,
        )
        .await;
        f.request(
            Method::PUT,
            &format!("/api/projects/{project}/sites/{site}"),
            TOKEN,
            json!({"name":"Evals"}),
            200,
        )
        .await;
        let code = code.unwrap_or(r#"export default async (r,ctx)=>{
    if(r.path==='/sdk') {
      try { const method=r.body.method.split('.'); return {status:200,body:await ctx.platform[method[0]][method[1]](...r.body.args)}; }
      catch(e){return {status:400,body:{code:e.code??'JS_ERROR',message:e.message}};}
    }
    if(r.path==='/transaction'){
      try {await ctx.db.transaction(async tx=>{await ctx.platform.environments.list()});return {status:200,body:'unexpected'};}
      catch(e){return {status:409,body:'blocked'};}
    }
    return {status:200,body:{env:Object.keys(ctx.platform.environments),harness:Object.keys(ctx.platform.harnesses)}};
  }"#);
        let revision = Uuid::now_v7();
        f.release = Uuid::now_v7();
        f.internal(Method::POST,&format!("/internal/sites/{site}/revisions"),json!({"id":revision,"files":[file("backend.ts",code),file("frontend/index.html","<h1>Sites</h1>"),file("migrations/001.sql","CREATE TABLE events(id TEXT PRIMARY KEY, trial INTEGER, status TEXT); CREATE TABLE intents(id TEXT PRIMARY KEY, input TEXT NOT NULL, session TEXT); CREATE TABLE flags(id TEXT PRIMARY KEY);")]})).await;
        f.internal(Method::POST,&format!("/internal/sites/{site}/releases"),json!({"id":f.release,"manifest":{"source_revision_id":revision,"frontend_entrypoint":"public/index.html","backend_entrypoint":"backend.js","sdk_version":"1","schema":{"min":1,"max":1},"migrations":[{"version":1,"path":"migrations/001.sql"}]},"files":[file("backend.js",code),file("public/index.html","<h1>Sites</h1>")]})).await;
        f.internal(
            Method::PATCH,
            &format!("/internal/sites/{site}"),
            json!({"status":"suspended"}),
        )
        .await;
        f.internal(
            Method::POST,
            &format!("/internal/sites/{site}/schema/migrations"),
            json!({"release_id":f.release}),
        )
        .await;
        f.internal(
            Method::PATCH,
            &format!("/internal/sites/{site}"),
            json!({"status":"ready"}),
        )
        .await;
        f
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Value,
        expected: u16,
    ) -> Value {
        let response = self
            .client
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let bytes = response.bytes().await.unwrap();
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&bytes)}))
        };
        assert_eq!(status, expected, "{path}: {value}");
        value
    }
    async fn internal(&self, method: Method, path: &str, body: Value) -> Value {
        let response = self
            .client
            .request(method, format!("{}{path}", self.sites_url))
            .bearer_auth(SERVICE)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{value}");
        value
    }
    async fn sdk(&self, method: &str, args: Value) -> Value {
        let result=self.request(Method::POST,&format!("/api/projects/{}/sites/{}/invocations",self.project,self.site),TOKEN,json!({"id":Uuid::now_v7(),"release_id":self.release,"request":{"method":"POST","path":"/sdk","body":{"method":method,"args":args}}}),200).await;
        assert_eq!(result["status"], "succeeded", "{result}");
        result["response"].clone()
    }
    fn start(&self) -> Value {
        json!({"harnessId":HARNESS,"environmentId":self.environment,"prompt":"Evaluate this task","model":{"provider":"openai","id":"test-model"},"accountId":self.account})
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn site_sdk_end_to_end(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.request(
        Method::GET,
        &format!("/api/projects/{}/sites", f.project),
        CAP,
        Value::Null,
        401,
    )
    .await;
    f.request(
        Method::GET,
        &format!("/api/projects/{}/sites", f.other),
        TOKEN,
        Value::Null,
        401,
    )
    .await;
    f.request(
        Method::POST,
        "/internal/site-capabilities",
        TOKEN,
        json!({}),
        401,
    )
    .await;
    let env = f.sdk("environments.list", json!([])).await;
    assert_eq!(env["body"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        f.sdk("environments.get", json!([f.foreign_environment]))
            .await["status"],
        400
    );
    let h = f.sdk("harnesses.list", json!([])).await;
    assert_eq!(h["body"]["items"].as_array().unwrap().len(), 3);
    assert_eq!(
        f.sdk("harnesses.get", json!(["environments"])).await["status"],
        200
    );
    assert_eq!(
        f.sdk("harnesses.setEnabled", json!([HARNESS, false])).await["status"],
        400
    );
    assert_eq!(
        f.sdk("environments.remove", json!([f.environment])).await["status"],
        400
    );
    let opts = f.sdk("sessions.startOptions", json!([HARNESS])).await;
    assert_eq!(opts["body"]["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(opts["body"]["environmentRequired"], true);
    let mut bad = f.start();
    bad["environmentId"] = json!(f.foreign_environment);
    assert_eq!(
        f.sdk("sessions.start", json!([bad,{"idempotencyKey":"foreign"}]))
            .await["status"],
        400
    );
    let mut bad = f.start();
    bad["accountId"] = json!(Uuid::now_v7());
    assert_eq!(
        f.sdk(
            "sessions.start",
            json!([bad,{"idempotencyKey":"bad-account"}])
        )
        .await["status"],
        400
    );
    let mut bad = f.start();
    bad["configOverride"] = json!({"environment":{"machine_id":Uuid::now_v7()}});
    assert_eq!(
        f.sdk(
            "sessions.start",
            json!([bad,{"idempotencyKey":"injection"}])
        )
        .await["status"],
        400
    );
    let (first, duplicate) = tokio::join!(
        f.sdk(
            "sessions.start",
            json!([f.start(),{"idempotencyKey":"trial-1"}])
        ),
        f.sdk(
            "sessions.start",
            json!([f.start(),{"idempotencyKey":"trial-1"}])
        )
    );
    assert_eq!(first["status"], 200, "{first}");
    assert_eq!(first["body"], duplicate["body"]);
    let session = first["body"]["sessionId"].as_str().unwrap();
    let run = first["body"]["runId"].as_str().unwrap();
    assert_eq!(
        first["body"]["run"]["config"]["environment"]["workspace_root"],
        "/work"
    );
    assert!(first["body"]["run"].get("worker_id").is_none());
    let mut changed = f.start();
    changed["prompt"] = json!("Changed");
    assert_eq!(
        f.sdk(
            "sessions.start",
            json!([changed,{"idempotencyKey":"trial-1"}])
        )
        .await["status"],
        400
    );
    let state = f.sdk("sessions.get", json!([session])).await;
    assert_eq!(state["body"]["status"], "ready");
    assert_eq!(f.sdk("sessions.send",json!([session,{"prompt":"Steer","expectedRevision":0,"expectedRunId":null},{"idempotencyKey":"wrong-run"}])).await["status"],400);
    let sent=f.sdk("sessions.send",json!([session,{"prompt":"Steer","expectedRevision":0,"expectedRunId":run},{"idempotencyKey":"send-1"}])).await;
    assert_eq!(sent["status"], 200, "{sent}");
    let stopped = f
        .sdk(
            "sessions.stop",
            json!([session,{"expectedRunId":run,"idempotencyKey":"stop-1"}]),
        )
        .await;
    assert_eq!(stopped["status"], 200);
    assert!(!stopped["body"]["run"]["abort_requested_at"].is_null());
    let mut finish = f.pool.begin().await.unwrap();
    let changed = sqlx::query("update runs set status='aborted',version=version+1,available_at=null,finished_at=clock_timestamp() where id=$1 and status in ('ready','running','waiting')").bind(Uuid::parse_str(run).unwrap()).execute(&mut *finish).await.unwrap().rows_affected();
    if changed == 1 {
        sqlx::query("insert into run_events(id,run_id,type,source,payload) values($1,$2,'run.aborted','runtime','{}')").bind(Uuid::now_v7()).bind(Uuid::parse_str(run).unwrap()).execute(&mut *finish).await.unwrap();
    }
    finish.commit().await.unwrap();
    assert_eq!(
        f.sdk("sessions.send", json!([session,{"prompt":"Continue","expectedRevision":0},{"idempotencyKey":"missing-observation"}])).await["status"],
        400
    );
    let next=f.sdk("sessions.send",json!([session,{"prompt":"Continue","expectedRevision":0,"expectedRunId":null},{"idempotencyKey":"send-2"}])).await;
    assert_eq!(next["status"], 200, "{next}");
    assert_ne!(next["body"]["runId"], run);
    assert_eq!(
        f.sdk(
            "sessions.stop",
            json!([session,{"expectedRunId":run,"idempotencyKey":"old-run"}])
        )
        .await["status"],
        400
    );
    assert_eq!(
        f.sdk(
            "sessions.stop",
            json!([session,{"expectedRunId":run,"idempotencyKey":"stop-1"}])
        )
        .await["body"],
        stopped["body"]
    );
    // Foreign sessions/messages/metrics cannot be addressed by ID.
    let foreign = Uuid::now_v7();
    sqlx::query("insert into sessions(id,project_id,harness_id,config) values($1,$2,$3,'{}')")
        .bind(foreign)
        .bind(f.other)
        .bind(HARNESS)
        .execute(&f.pool)
        .await
        .unwrap();
    for method in ["sessions.get", "sessions.messages", "sessions.metrics"] {
        assert_eq!(f.sdk(method, json!([foreign])).await["status"], 400);
    }
    // More messages than a single SDK page: metrics must aggregate all of them.
    for n in 1..=205 {
        let id = Uuid::now_v7();
        let usage = if n == 205 {
            Value::Null
        } else {
            json!({"input":10,"output":2,"cache_read":5,"cost":{"total":0.01}})
        };
        sqlx::query(
            "insert into messages(id,project_id,origin_run_id,message) values($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(f.project)
        .bind(Uuid::parse_str(run).unwrap())
        .bind(json!({"id":id,"role":"assistant","content":[],"usage":usage}))
        .execute(&f.pool)
        .await
        .unwrap();
        sqlx::query("insert into session_messages(project_id,session_id,revision,message_id,run_id) values($1,$2,$3,$4,$5)").bind(f.project).bind(Uuid::parse_str(session).unwrap()).bind(i64::from(n)).bind(id).bind(Uuid::parse_str(run).unwrap()).execute(&f.pool).await.unwrap();
    }
    let messages = f
        .sdk("sessions.messages", json!([session,{"limit":2}]))
        .await;
    assert_eq!(messages["body"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(messages["body"]["next_after_revision"], 2);
    let metrics = f.sdk("sessions.metrics", json!([session])).await;
    assert_eq!(metrics["body"]["assistant_messages"], 205);
    assert_eq!(metrics["body"]["input_tokens"], 2040);
    assert!((metrics["body"]["cost_usd"].as_f64().unwrap() - 2.04).abs() < 1e-8);
    assert_eq!(metrics["body"]["completeUsage"], false);
    let inside=f.request(Method::POST,&format!("/api/projects/{}/sites/{}/invocations",f.project,f.site),TOKEN,json!({"id":Uuid::now_v7(),"release_id":f.release,"request":{"method":"POST","path":"/transaction"}}),200).await;
    assert_eq!(inside["response"]["status"], 409);
    let operations: i64 =
        sqlx::query_scalar("select count(*) from site_runtime_operations where site_id=$1")
            .bind(f.site)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(operations, 4);
    // Successful operation receipts remain replayable.
    assert_eq!(
        f.sdk(
            "sessions.start",
            json!([f.start(),{"idempotencyKey":"trial-1"}])
        )
        .await["body"],
        first["body"]
    );
    assert_eq!(
        f.sdk(
            "sessions.start",
            json!([f.start(),{"idempotencyKey":"new-trial"}])
        )
        .await["status"],
        200
    );
    f.request(
        Method::DELETE,
        &format!("/api/projects/{}/sites/{}", f.project, f.site),
        TOKEN,
        Value::Null,
        204,
    )
    .await;
    f.request(
        Method::POST,
        &format!("/api/projects/{}/sites/{}/invocations", f.project, f.site),
        TOKEN,
        json!({"id":Uuid::now_v7(),"request":{"method":"GET","path":"/"}}),
        404,
    )
    .await;
    assert!(f.root.path().join("service.sqlite").exists());
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn site_reads_harness_outputs_through_the_real_backend_bridge(pool: PgPool) {
    use platform_runtime_client::{
        ClientConfig, Command as RuntimeCommand, PlatformClient, RequestKey, WorkerRegistration,
        types::*,
    };
    let mut f = Fixture::new(pool).await;
    sqlx::query("update harnesses set harness_contract=$1 where id=$2")
        .bind(json!({"outputs":{"result":{"kind":"json"}}}))
        .bind(HARNESS)
        .execute(&f.pool)
        .await
        .unwrap();
    let accepted = f
        .sdk(
            "sessions.start",
            json!([f.start(),{"idempotencyKey":"output-test"}]),
        )
        .await;
    assert_eq!(accepted["status"], 200, "{accepted}");
    let run: Uuid = serde_json::from_value(accepted["body"]["runId"].clone()).unwrap();
    assert_eq!(
        f.sdk("runs.outputs", json!([run])).await["body"]["items"],
        json!([])
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let routes =
        platform_server::runtime::worker_router(RuntimeService::new(f.pool.clone()), ADMIN);
    f.tasks.push(tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    }));
    let client = PlatformClient::new(&url, Uuid::new_v4(), TOKEN, ClientConfig::default()).unwrap();
    client
        .register(
            ADMIN,
            &WorkerRegistration {
                build_id: "outputs-test".into(),
                supported_harnesses: vec![HARNESS.into()],
                capacity: 1,
            },
        )
        .await
        .unwrap();
    let assignments = client
        .claim(&RuntimeCommand::new(
            RequestKey::new("claim").unwrap(),
            Claim { limit: 1 },
        ))
        .await
        .unwrap();
    let caller = client.run(assignments.items[0].lease()).unwrap();
    assert_eq!(caller.lease().run_id, run);
    let published = caller
        .publish_output(&RuntimeCommand::new(
            RequestKey::new("publish").unwrap(),
            PublishRunOutput {
                name: "result".into(),
                output: OutputValue::Json(json!({"score":42})),
            },
        ))
        .await
        .unwrap();
    let ctx = caller.context(&ContextQuery::default()).await.unwrap();
    let mut commit = Commit::new(ctx.run.run.version, ctx.session.current_revision);
    commit.disposition = Disposition::Failed {
        error: json!({"kind":"test"}).as_object().unwrap().clone(),
    };
    caller
        .commit(&RuntimeCommand::new(
            RequestKey::new("finish").unwrap(),
            commit,
        ))
        .await
        .unwrap();
    let result = f.sdk("runs.outputs", json!([run,{"limit":1}])).await;
    assert_eq!(result["status"], 200, "{result}");
    let page: RunOutputsPage = serde_json::from_value(result["body"].clone()).unwrap();
    assert_eq!(page.items, vec![published]);
    assert_eq!(
        f.sdk("runs.outputs", json!([run,{"afterSequence":1}]))
            .await["body"]["items"],
        json!([])
    );
    assert_eq!(
        f.sdk("runs.outputs", json!([run,{"limit":51}])).await["status"],
        400
    );
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
    assert_eq!(
        f.sdk("runs.outputs", json!([foreign_run])).await["body"]["code"],
        "RUNTIME_NOT_FOUND"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn site_management_access_and_uncertain_provisioning(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let path = format!("/api/projects/{}/sites/{}", f.project, f.site);
    f.request(Method::PUT, &path, TOKEN, json!({"name":"Evals"}), 200)
        .await;
    f.request(Method::PUT, &path, TOKEN, json!({"name":"Changed"}), 409)
        .await;
    f.request(
        Method::PATCH,
        &path,
        TOKEN,
        json!({"name":"Renamed","status":"suspended"}),
        200,
    )
    .await;
    let call =
        json!({"id":Uuid::now_v7(),"release_id":f.release,"request":{"method":"GET","path":"/"}});
    f.request(
        Method::POST,
        &format!("{path}/invocations"),
        TOKEN,
        call.clone(),
        409,
    )
    .await;
    f.request(Method::PATCH, &path, TOKEN, json!({"status":"ready"}), 200)
        .await;
    let first = f
        .request(
            Method::POST,
            &format!("{path}/invocations"),
            TOKEN,
            call.clone(),
            200,
        )
        .await;
    let replay = f
        .request(
            Method::POST,
            &format!("{path}/invocations"),
            TOKEN,
            call.clone(),
            200,
        )
        .await;
    assert_eq!(first, replay);
    assert_eq!(first["response"]["body"]["env"], json!(["list", "get"]));
    assert_eq!(
        first["response"]["body"]["harness"],
        json!(["list", "get", "startOptions"])
    );
    let mut changed = call.clone();
    changed["request"]["path"] = json!("/other");
    f.request(
        Method::POST,
        &format!("{path}/invocations"),
        TOKEN,
        changed,
        409,
    )
    .await;
    // A real service credential still cannot forge a project or unforwarded invocation.
    let forged = json!({"site_id":f.site,"project_id":f.other,"invocation_id":call["id"],"release_id":f.release,"method":"environments.list","args":{}});
    f.request(
        Method::POST,
        "/internal/site-capabilities",
        CAP,
        forged,
        404,
    )
    .await;
    let unregistered = json!({"site_id":f.site,"project_id":f.project,"invocation_id":Uuid::now_v7(),"release_id":f.release,"method":"environments.list","args":{}});
    f.request(
        Method::POST,
        "/internal/site-capabilities",
        CAP,
        unregistered,
        401,
    )
    .await;
    let wrong_release = json!({"site_id":f.site,"project_id":f.project,"invocation_id":call["id"],"release_id":Uuid::now_v7(),"method":"environments.list","args":{}});
    f.request(
        Method::POST,
        "/internal/site-capabilities",
        CAP,
        wrong_release,
        401,
    )
    .await;
    // Project enablement is the harness authorization source for Sites too.
    sqlx::query("update project_harnesses set enabled=false where project_id=$1 and harness_id=$2")
        .bind(f.project)
        .bind(HARNESS)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(
        f.sdk("harnesses.list", json!([])).await["body"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["id"] != HARNESS)
    );
    assert_eq!(
        f.sdk(
            "sessions.start",
            json!([f.start(),{"idempotencyKey":"disabled"}])
        )
        .await["status"],
        400
    );
    // Operator replacement rotates the credential; the previous token stops working.
    let rotated = "sites-test-project-rotated-01234567890123456789";
    f.request(
        Method::PUT,
        &format!("/internal/site-access/{}", f.project),
        ADMIN,
        // Older control-plane callers may still send this field during rollout;
        // it must not restore or otherwise affect project harness enablement.
        json!({"token":rotated,"enabled":true,"harnesses":[{"id":HARNESS,"environmentMode":"single"}],"accountIds":[]}),
        200,
    )
    .await;
    f.request(Method::GET, &path, TOKEN, Value::Null, 401).await;
    f.request(Method::GET, &path, rotated, Value::Null, 200)
        .await;
    // Persistence precedes a failed external provisioning effect.
    f.child.kill().unwrap();
    f.child.wait().unwrap();
    let new_site = Uuid::now_v7();
    f.request(
        Method::PUT,
        &format!("/api/projects/{}/sites/{new_site}", f.project),
        rotated,
        json!({"name":"Pending"}),
        502,
    )
    .await;
    let saved: Uuid = sqlx::query_scalar("select project_id from project_sites where id=$1")
        .bind(new_site)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(saved, f.project);
    f.request(Method::DELETE, &path, rotated, Value::Null, 204)
        .await;
    f.request(Method::DELETE, &path, rotated, Value::Null, 204)
        .await;
}

// Persist stage intent before Platform calls, and replay unfinished intent even
// when the event itself was already seen. Failure is injected after acceptance.
const CALLBACK_BACKEND: &str = r#"
export default async (r,ctx)=>{
  if(r.path==='/sdk') {
    try {const p=r.body.method.split('.');return {status:200,body:await ctx.platform[p[0]][p[1]](...r.body.args)}}
    catch(e){return {status:400,body:{code:e.code,message:e.message}}}
  }
  if(r.path==='/state')return {status:200,body:{events:await ctx.db.query('SELECT * FROM events'),intents:await ctx.db.query('SELECT * FROM intents')}};
  if(ctx.invocation.source!=='callback' || ctx.invocation.eventId!==r.body.id || ctx.invocation.subscriptionId!==r.body.subscriptionId)return {status:403,body:'not a callback'};
  if(r.path==='/always-fail')return {status:503,body:'try later'};
  if(r.path==='/crash'){
    const first=await ctx.db.execute("INSERT OR IGNORE INTO flags VALUES('crash')");
    if(first.changes===1){while(true){}}
    return {status:200,body:ctx.invocation};
  }
  const event=r.body;
  await ctx.db.transaction(async tx=>{
    await tx.execute('INSERT OR IGNORE INTO events VALUES(?,?,?)',[event.id,event.payload.trial,event.status]);
    const count=(await tx.query('SELECT count(*) n FROM events'))[0].n;
    if(count===10)for(let i=0;i<10;i++)await tx.execute('INSERT OR IGNORE INTO intents VALUES(?,?,NULL)',['next:'+i,JSON.stringify(event.payload.start)]);
  });
  for(const intent of await ctx.db.query('SELECT * FROM intents WHERE session IS NULL ORDER BY id')){
    const accepted=await ctx.platform.sessions.start(JSON.parse(intent.input),{idempotencyKey:intent.id});
    const first=await ctx.db.execute("INSERT OR IGNORE INTO flags VALUES('after-start')");
    if(first.changes===1)return {status:503,body:'failed after Platform accepted, before saving receipt'};
    await ctx.db.execute('UPDATE intents SET session=? WHERE id=?',[accepted.sessionId,intent.id]);
  }
  return {status:200,body:{release:ctx.site.releaseId,eventId:ctx.invocation.eventId}};
}"#;

impl Fixture {
    async fn terminal(&self, run: Uuid, status: &str) {
        let mut tx = self.pool.begin().await.unwrap();
        if status == "completed" {
            let id = Uuid::now_v7();
            let session: Uuid = sqlx::query_scalar("select session_id from runs where id=$1")
                .bind(run)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            sqlx::query(
                "insert into messages(id,project_id,origin_run_id,message) values($1,$2,$3,$4)",
            )
            .bind(id)
            .bind(self.project)
            .bind(run)
            .bind(json!({"id":id,"role":"assistant","content":[],"stop_reason":"stop"}))
            .execute(&mut *tx)
            .await
            .unwrap();
            sqlx::query("insert into session_messages(project_id,session_id,revision,message_id,run_id) values($1,$2,1,$3,$4)").bind(self.project).bind(session).bind(id).bind(run).execute(&mut *tx).await.unwrap();
            sqlx::query("update runs set status='completed',available_at=null,version=version+1,final_message_id=$2,finished_at=clock_timestamp() where id=$1").bind(run).bind(id).execute(&mut *tx).await.unwrap();
        } else {
            sqlx::query("update runs set status=$2,available_at=null,version=version+1,error=case when $2='failed' then '{}'::jsonb else null end,abort_requested_at=case when $2='aborted' then clock_timestamp() else null end,finished_at=clock_timestamp() where id=$1").bind(run).bind(status).execute(&mut *tx).await.unwrap();
        }
        sqlx::query(
            "insert into run_events(id,run_id,type,source,payload) values($1,$2,$3,'runtime','{}')",
        )
        .bind(Uuid::now_v7())
        .bind(run)
        .bind(format!("run.{status}"))
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    async fn callback_start(&self, path: &str, payload: Value, key: &str) -> Value {
        let mut input = self.start();
        input["onComplete"] = json!({"path":path,"payload":payload});
        let result = self
            .sdk("sessions.start", json!([input,{"idempotencyKey":key}]))
            .await;
        assert_eq!(result["status"], 200, "{result}");
        result["body"].clone()
    }
    async fn due(&self) {
        sqlx::query("update site_callback_deliveries set next_attempt_at=clock_timestamp() where status='pending'").execute(&self.pool).await.unwrap();
    }
    async fn restart_sites(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let binary = std::env::var_os("SITES_TEST_BINARY")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("platform-sites-service")
            });
        self.child = Command::new(binary)
            .env("SITES_DATA_DIR", self.root.path())
            .env("SITES_API_TOKEN", SERVICE)
            .env(
                "SITES_BIND_ADDRESS",
                self.sites_url.strip_prefix("http://").unwrap(),
            )
            .env("SITES_CONTENT_ORIGIN", "")
            .env("SITES_DASHBOARD_ORIGIN", "")
            .env("SITES_PLATFORM_URL", &self.url)
            .env("SITES_PLATFORM_CAPABILITY_TOKEN", CAP)
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if self
                .client
                .get(format!("{}/readyz", self.sites_url))
                .bearer_auth(SERVICE)
                .send()
                .await
                .is_ok_and(|v| v.status().is_success())
            {
                return;
            }
            assert!(self.child.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        panic!("sites service did not restart");
    }
    async fn restart_platform(&mut self) {
        self.tasks[0].abort();
        let _ = (&mut self.tasks[0]).await;
        let listener = tokio::net::TcpListener::bind(self.url.strip_prefix("http://").unwrap())
            .await
            .unwrap();
        let service = self.service.clone();
        self.tasks[0] = tokio::spawn(async move {
            axum::serve(listener, platform_server::sites::router(service))
                .await
                .unwrap()
        });
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn callbacks_resume_two_batches_after_restarts_without_duplicate_trials(pool: PgPool) {
    let mut f = Fixture::with_code(pool, Some(CALLBACK_BACKEND)).await;
    let mut runs = vec![];
    for trial in 0..10 {
        let input = json!({"trial":trial,"start":f.start()});
        let accepted = f
            .callback_start("/done", input.clone(), &format!("first:{trial}"))
            .await;
        let replay = f
            .callback_start("/done", input, &format!("first:{trial}"))
            .await;
        assert_eq!(accepted, replay);
        runs.push(Uuid::parse_str(accepted["runId"].as_str().unwrap()).unwrap());
    }
    assert_eq!(
        f.sdk("callbacks.list", json!([])).await["body"]["items"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    // Rollback must not leak an outbox event.
    let mut tx = f.pool.begin().await.unwrap();
    sqlx::query("update runs set status='failed',available_at=null,version=version+1,error='{}',finished_at=clock_timestamp() where id=$1").bind(runs[0]).execute(&mut *tx).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from site_callback_deliveries")
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
        1
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from site_callback_deliveries")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        0
    );
    for (n, run) in runs.iter().enumerate() {
        f.terminal(*run, ["completed", "failed", "aborted"][n % 3])
            .await;
    }
    // Publish another release. Callbacks must still execute the original code.
    let newer = Uuid::now_v7();
    let revision = Uuid::now_v7();
    let site = f.site;
    f.internal(Method::POST,&format!("/internal/sites/{site}/revisions"),json!({"id":revision,"files":[file("backend.ts","export default ()=>({status:500,body:'wrong release'})"),file("frontend/index.html","new")]})).await;
    f.internal(Method::POST,&format!("/internal/sites/{site}/releases"),json!({"id":newer,"manifest":{"source_revision_id":revision,"frontend_entrypoint":"public/index.html","backend_entrypoint":"backend.js","sdk_version":"1","schema":{"min":0,"max":1}},"files":[file("backend.js","export default ()=>({status:500,body:'wrong release'})"),file("public/index.html","new")]})).await;
    let activated = f
        .client
        .put(format!(
            "{}/internal/sites/{site}/active-release",
            f.sites_url
        ))
        .bearer_auth(SERVICE)
        .header("Idempotency-Key", "new-release")
        .json(&json!({"release_id":newer,"expected_generation":0}))
        .send()
        .await
        .unwrap();
    assert!(
        activated.status().is_success(),
        "{}",
        activated.text().await.unwrap()
    );
    f.restart_sites().await;
    f.restart_platform().await;
    // No browser requests from here: only the durable dispatcher and run events.
    for _ in 0..10 {
        assert!(f.service.deliver_callback_once().await.unwrap());
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from runs")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        11
    );
    let pending: Uuid =
        sqlx::query_scalar("select id from site_callback_deliveries where status='pending'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    let first_attempt: Uuid =
        sqlx::query_scalar("select last_invocation_id from site_callback_deliveries where id=$1")
            .bind(pending)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    f.restart_sites().await;
    f.restart_platform().await;
    f.due().await;
    let dispatcher = f.service.spawn_callback_dispatcher();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if sqlx::query_scalar::<_, i64>(
                "select count(*) from site_callback_deliveries where status='delivered'",
            )
            .fetch_one(&f.pool)
            .await
            .unwrap()
                == 10
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    dispatcher.abort();
    let _ = dispatcher.await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from runs")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        20
    );
    let last_attempt: Uuid =
        sqlx::query_scalar("select last_invocation_id from site_callback_deliveries where id=$1")
            .bind(pending)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_ne!(first_attempt, last_attempt);
    // Simulate lost acknowledgement: lease recovery reuses the saved invocation.
    sqlx::query("update site_callback_deliveries set status='delivering',lease_id=$2,lease_expires_at=clock_timestamp()-interval '1 second',finished_at=null where id=$1").bind(pending).bind(Uuid::now_v7()).execute(&f.pool).await.unwrap();
    assert!(f.service.deliver_callback_once().await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>(
            "select last_invocation_id from site_callback_deliveries where id=$1"
        )
        .bind(pending)
        .fetch_one(&f.pool)
        .await
        .unwrap(),
        last_attempt
    );
    // Force an actual duplicate handler execution: SQLite stage dedup still wins.
    sqlx::query("update site_callback_deliveries set status='pending',invocation_id=null,next_attempt_at=clock_timestamp(),finished_at=null where id=$1").bind(pending).execute(&f.pool).await.unwrap();
    let (a, b) = tokio::join!(
        f.service.deliver_callback_once(),
        f.service.deliver_callback_once()
    );
    assert_ne!(a.unwrap(), b.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from runs")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        20
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "select count(*) from site_callback_deliveries where status='delivered'"
        )
        .fetch_one(&f.pool)
        .await
        .unwrap(),
        10
    );
    let state=f.internal(Method::POST,&format!("/internal/sites/{site}/invocations"),json!({"id":Uuid::now_v7(),"release_id":f.release,"request":{"method":"GET","path":"/state"}})).await;
    assert_eq!(
        state["response"]["body"]["events"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    assert!(
        state["response"]["body"]["intents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["session"].is_string())
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn callback_validation_pause_failure_retry_and_scope(pool: PgPool) {
    let f = Fixture::with_code(pool, Some(CALLBACK_BACKEND)).await;
    let path = format!("/api/projects/{}/sites/{}", f.project, f.site);
    // A browser cannot forge trusted callback context, even with project access.
    f.request(Method::POST,&format!("{path}/invocations"),TOKEN,json!({"id":Uuid::now_v7(),"release_id":f.release,"callback":{"event_id":Uuid::now_v7(),"subscription_id":Uuid::now_v7()},"request":{"method":"POST","path":"/done"}}),422).await;
    for invalid in [
        json!({"path":"https://elsewhere.test"}),
        json!({"path":"/ok","payload":"x".repeat(17000)}),
        json!({"path":"/ok","releaseId":Uuid::now_v7()}),
    ] {
        let mut input = f.start();
        input["onComplete"] = invalid;
        assert_eq!(
            f.sdk(
                "sessions.start",
                json!([input,{"idempotencyKey":"invalid-callback"}])
            )
            .await["status"],
            400
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from runs")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        0
    );
    let accepted = f
        .callback_start("/always-fail", Value::Null, "failed")
        .await;
    let callback = accepted["callback"]["subscriptionId"].as_str().unwrap();
    let immutable = sqlx::query("update site_callbacks set path='/changed' where id=$1")
        .bind(Uuid::parse_str(callback).unwrap())
        .execute(&f.pool)
        .await
        .unwrap_err();
    assert_eq!(
        immutable.as_database_error().unwrap().code().as_deref(),
        Some("23514")
    );
    let session = accepted["sessionId"].as_str().unwrap();
    let run = Uuid::parse_str(accepted["runId"].as_str().unwrap()).unwrap();
    assert_eq!(f.sdk("sessions.send",json!([session,{"prompt":"steer","expectedRevision":0,"expectedRunId":run,"onComplete":{"path":"/done"}},{"idempotencyKey":"steer-callback"}])).await["status"],400);
    f.terminal(run, "failed").await;
    let immutable = sqlx::query("update site_callback_deliveries set event='{}'")
        .execute(&f.pool)
        .await
        .unwrap_err();
    assert_eq!(
        immutable.as_database_error().unwrap().code().as_deref(),
        Some("23514")
    );
    f.request(
        Method::PATCH,
        &path,
        TOKEN,
        json!({"status":"suspended"}),
        200,
    )
    .await;
    assert!(!f.service.deliver_callback_once().await.unwrap());
    let paused = f
        .request(
            Method::GET,
            &format!("{path}/callbacks/{callback}"),
            TOKEN,
            Value::Null,
            200,
        )
        .await;
    assert_eq!(paused["status"], "paused");
    assert_eq!(paused["delivery"]["attempts"], 0);
    f.request(Method::PATCH, &path, TOKEN, json!({"status":"ready"}), 200)
        .await;
    sqlx::query("update site_project_access set enabled=false where project_id=$1")
        .bind(f.project)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(!f.service.deliver_callback_once().await.unwrap());
    sqlx::query("update site_project_access set enabled=true where project_id=$1")
        .bind(f.project)
        .execute(&f.pool)
        .await
        .unwrap();
    // Run the production retry state machine at its last attempt without sleeping
    // through eleven backoff windows. Failed application status is not success.
    sqlx::query("update site_callback_deliveries set attempts=11")
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(f.service.deliver_callback_once().await.unwrap());
    let failed = f
        .request(
            Method::GET,
            &format!("{path}/callbacks/{callback}"),
            TOKEN,
            Value::Null,
            200,
        )
        .await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["delivery"]["last_error"], "CALLBACK_HTTP_ERROR");
    assert!(failed["delivery"]["last_invocation_id"].is_string());
    f.request(
        Method::POST,
        &format!("{path}/callbacks/{callback}/retry"),
        TOKEN,
        json!({"expectedVersion":-1}),
        409,
    )
    .await;
    f.request(
        Method::POST,
        &format!("{path}/callbacks/{callback}/retry"),
        TOKEN,
        json!({"expectedVersion":failed["delivery"]["version"]}),
        200,
    )
    .await;
    f.request(
        Method::POST,
        &format!("{path}/callbacks/{callback}/retry"),
        TOKEN,
        json!({"expectedVersion":failed["delivery"]["version"]}),
        409,
    )
    .await;
    assert!(f.service.deliver_callback_once().await.unwrap());
    let retried = f.sdk("callbacks.get", json!([callback])).await;
    assert_eq!(retried["body"]["delivery"]["id"], failed["delivery"]["id"]);
    assert_eq!(retried["body"]["delivery"]["attempts"], 1);
    assert_ne!(
        retried["body"]["delivery"]["last_invocation_id"],
        failed["delivery"]["last_invocation_id"]
    );
    assert!(
        retried["body"]["delivery"]["next_attempt_at"]
            .as_str()
            .unwrap()
            > retried["body"]["delivery"]["created_at"].as_str().unwrap()
    );
    // A second site in the same project cannot inspect this site's callbacks.
    let other_site = Uuid::now_v7();
    f.request(
        Method::PUT,
        &format!("/api/projects/{}/sites/{other_site}", f.project),
        TOKEN,
        json!({"name":"Other"}),
        200,
    )
    .await;
    f.request(
        Method::GET,
        &format!(
            "/api/projects/{}/sites/{other_site}/callbacks/{callback}",
            f.project
        ),
        TOKEN,
        Value::Null,
        404,
    )
    .await;
    f.request(
        Method::GET,
        &format!("/api/projects/{}/sites/{}/callbacks", f.other, f.site),
        TOKEN,
        Value::Null,
        401,
    )
    .await;
    // An idle follow-up can atomically register its own completion callback.
    let follow=f.sdk("sessions.send",json!([session,{"prompt":"next","expectedRevision":0,"expectedRunId":null,"onComplete":{"path":"/done","payload":{"trial":1}}},{"idempotencyKey":"follow-callback"}])).await;
    assert_eq!(follow["status"], 200, "{follow}");
    assert!(follow["body"]["callback"]["subscriptionId"].is_string());
    f.terminal(
        Uuid::parse_str(follow["body"]["runId"].as_str().unwrap()).unwrap(),
        "aborted",
    )
    .await;
    f.request(Method::DELETE, &path, TOKEN, Value::Null, 204)
        .await;
    assert!(!f.service.deliver_callback_once().await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "select count(*) from site_callback_deliveries where status='cancelled'"
        )
        .fetch_one(&f.pool)
        .await
        .unwrap(),
        2
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn callback_interrupted_execution_and_expired_lease_recover(pool: PgPool) {
    let mut f = Fixture::with_code(pool, Some(CALLBACK_BACKEND)).await;
    let accepted = f.callback_start("/crash", Value::Null, "crash").await;
    f.terminal(
        Uuid::parse_str(accepted["runId"].as_str().unwrap()).unwrap(),
        "aborted",
    )
    .await;
    let service = f.service.clone();
    let delivery = tokio::spawn(async move { service.deliver_callback_once().await });
    let (invocation,old_lease)=tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            let row:Option<(Uuid,Uuid)>=sqlx::query_as("select invocation_id,lease_id from site_callback_deliveries where invocation_id is not null").fetch_optional(&f.pool).await.unwrap();
            if let Some((invocation,lease))=row {
                // Wait until the database effect occurred and the guest is busy.
                // Backend logs are saved only at completion, so inspect the site's
                // SQLite table through a separate read-only connection.
                let db=f.root.path().join("sites").join(f.site.to_string()).join("data/site.sqlite");
                if db.exists() {
                    let output=Command::new("sqlite3").arg(&db).arg("select count(*) from flags where id='crash'").output().unwrap();
                    if output.status.success() && String::from_utf8_lossy(&output.stdout).trim()=="1"{break (invocation,lease)}
                }
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }).await.unwrap();
    // A replacement lease must fence a late response from the first dispatcher.
    let replacement = Uuid::now_v7();
    sqlx::query("update site_callback_deliveries set lease_id=$1,lease_expires_at=clock_timestamp()+interval '60 seconds' where lease_id=$2").bind(replacement).bind(old_lease).execute(&f.pool).await.unwrap();
    f.restart_sites().await;
    assert!(delivery.await.unwrap().unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("select lease_id from site_callback_deliveries")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        replacement
    );
    sqlx::query("update site_callback_deliveries set lease_expires_at=clock_timestamp()-interval '1 second'").execute(&f.pool).await.unwrap();
    assert!(f.service.deliver_callback_once().await.unwrap());
    let interrupted = f
        .internal(
            Method::GET,
            &format!("/internal/sites/{}/invocations/{invocation}", f.site),
            Value::Null,
        )
        .await;
    assert_eq!(interrupted["status"], "interrupted");
    f.due().await;
    assert!(f.service.deliver_callback_once().await.unwrap());
    let final_state = f
        .sdk(
            "callbacks.get",
            json!([accepted["callback"]["subscriptionId"]]),
        )
        .await;
    assert_eq!(final_state["body"]["status"], "delivered", "{final_state}");
    assert_ne!(
        final_state["body"]["delivery"]["last_invocation_id"],
        invocation.to_string()
    );
}

/// Manual browser fixture; real services and storage, simulated agent output.
/// Called by `cargo run -p platform-server --example sites-preview` only.
pub async fn browser_preview(pool: PgPool) {
    let origin = "http://127.0.0.1:5176";
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = port.local_addr().unwrap().to_string();
    drop(port);
    let code = include_str!("../../sites-service/examples/harness-browser/backend.js");
    let html = include_str!("../../sites-service/examples/harness-browser/index.html");
    let f = Fixture::with_content(pool, Some(code), Some((address.clone(), origin.into()))).await;
    sqlx::query("update projects set name='Sites demo' where project_id=$1")
        .bind(f.project)
        .execute(&f.pool)
        .await
        .unwrap();
    f.request(
        Method::PATCH,
        &format!("/api/projects/{}/sites/{}", f.project, f.site),
        TOKEN,
        json!({"name":"Harness browser"}),
        200,
    )
    .await;
    let revision = Uuid::now_v7();
    let release = Uuid::now_v7();
    let site = f.site;
    f.internal(
        Method::POST,
        &format!("/internal/sites/{site}/revisions"),
        json!({"id":revision,"files":[file("backend.ts",code),file("frontend/index.html",html)]}),
    )
    .await;
    f.internal(Method::POST,&format!("/internal/sites/{site}/releases"),json!({"id":release,"manifest":{"source_revision_id":revision,"frontend_entrypoint":"public/index.html","backend_entrypoint":"backend.js","sdk_version":"1","schema":{"min":0,"max":1}},"files":[file("backend.js",code),file("public/index.html",html)]})).await;
    let activated = f
        .client
        .put(format!(
            "{}/internal/sites/{site}/active-release",
            f.sites_url
        ))
        .bearer_auth(SERVICE)
        .header("Idempotency-Key", "preview")
        .json(&json!({"release_id":release,"expected_generation":0}))
        .send()
        .await
        .unwrap();
    assert!(activated.status().is_success());
    let dashboard = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../dashboard");
    let mut vite = Command::new("node")
        .args([
            "node_modules/vite/bin/vite.js",
            "--host",
            "127.0.0.1",
            "--port",
            "5176",
            "--strictPort",
        ])
        .current_dir(dashboard)
        .env("DASHBOARD_ORIGIN", origin)
        .env(
            "DASHBOARD_SITES_CONTENT_ORIGIN",
            format!("http://{address}"),
        )
        .env("DASHBOARD_PLATFORM_URL", &f.url)
        .env(
            "DASHBOARD_SITE_PROJECT_TOKENS",
            json!({f.project.to_string():TOKEN}).to_string(),
        )
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    println!(
        "\nIsolated Sites preview: {origin}/projects/{}/sites\nRuns in this preview are simulated; no gateway credits or machines are used. Ctrl-C cleans up.\n",
        f.project
    );
    loop {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>break,
            _=tokio::time::sleep(Duration::from_secs(2))=>{
                if vite.try_wait().unwrap().is_some(){break;}
                let runs:Vec<Uuid>=sqlx::query_scalar("select id from runs where status='ready' and created_at<clock_timestamp()-interval '3 seconds'").fetch_all(&f.pool).await.unwrap();
                for run in runs {
                    let mut tx=f.pool.begin().await.unwrap();
                    let (session,prompt):(Uuid,Value)=sqlx::query_as("select r.session_id,i.payload->'message' from runs r join run_inputs i on i.run_id=r.id where r.id=$1 order by i.sequence limit 1").bind(run).fetch_one(&mut *tx).await.unwrap();
                    let user=Uuid::now_v7();let assistant=Uuid::now_v7();
                    for (id,revision,message) in [(user,1,prompt),(assistant,2,json!({"id":assistant,"role":"assistant","content":[{"type":"response","response":{"content":"Mock agent completed the task. This isolated demo exercises real site, bridge, session, and storage APIs without calling an LLM or machine."}}],"stop_reason":"stop","usage":{"input":0,"output":0,"cache_read":0,"cache_write":0,"cost":{"total":0}}}))] {
                        sqlx::query("insert into messages(id,project_id,origin_run_id,message) values($1,$2,$3,$4)").bind(id).bind(f.project).bind(run).bind(message).execute(&mut *tx).await.unwrap();
                        sqlx::query("insert into session_messages(project_id,session_id,revision,message_id,run_id) values($1,$2,$3,$4,$5)").bind(f.project).bind(session).bind(i64::from(revision)).bind(id).bind(run).execute(&mut *tx).await.unwrap();
                    }
                    sqlx::query("update runs set status='completed',available_at=null,version=version+1,final_message_id=$2,finished_at=clock_timestamp() where id=$1").bind(run).bind(assistant).execute(&mut *tx).await.unwrap();
                    sqlx::query("insert into run_events(id,run_id,type,source,payload) values($1,$2,'run.completed','runtime','{}')").bind(Uuid::now_v7()).bind(run).execute(&mut *tx).await.unwrap();
                    tx.commit().await.unwrap();
                }
            }
        }
    }
    let _ = vite.kill();
    let _ = vite.wait();
}

#[cfg(all(test, unix))]
#[path = "support/sites_authoring.rs"]
mod sites_authoring;

#[cfg(all(test, unix))]
#[path = "support/sites_harness.rs"]
mod sites_harness;

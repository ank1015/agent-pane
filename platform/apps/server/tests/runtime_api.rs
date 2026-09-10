//! Application API contract tests against HTTP and disposable PostgreSQL databases.
use platform_server::runtime::{RuntimeService, router};
use reqwest::{Client, Method, Response};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use tokio::task::JoinHandle;
use uuid::Uuid;

struct App {
    pool: PgPool,
    client: Client,
    url: String,
    task: JoinHandle<()>,
    project: Uuid,
}
impl Drop for App {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl App {
    async fn new(pool: PgPool) -> Self {
        let project = Uuid::now_v7();
        sqlx::query("insert into projects(project_id,name) values($1,'API tests')")
            .bind(project)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into harnesses(id,name,default_config,config_schema) values('test','Test',$1,$2)")
            .bind(json!({"model":"test-model","nested":{"keep":true,"change":1},"remove":true}))
            .bind(json!({"type":"object","required":["model"],"properties":{"model":{"type":"string"},"nested":{"type":"object"}}})).execute(&pool).await.unwrap();
        sqlx::query(
            "insert into project_harnesses(project_id,harness_id,enabled) values($1,'test',true)",
        )
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let service = RuntimeService::new(pool.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router(service)).await.unwrap();
        });
        Self {
            pool,
            client: Client::new(),
            url,
            task,
            project,
        }
    }
    async fn raw(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Response {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.url))
            .timeout(Duration::from_secs(10));
        if let Some(key) = key {
            request = request.header("Idempotency-Key", key);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await.unwrap()
    }
    async fn check(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
        status: u16,
    ) -> Value {
        let response = self.raw(method, path, key, body).await;
        let actual = response.status().as_u16();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body: Value = response.json().await.unwrap();
        assert_eq!(actual, status, "{path}: {body}");
        body
    }
    async fn get(&self, path: &str) -> Value {
        self.check(Method::GET, path, None, None, 200).await
    }
    async fn post(&self, path: &str, key: &str, body: Value, status: u16) -> Value {
        self.check(Method::POST, path, Some(key), Some(body), status)
            .await
    }
    async fn create(&self, key: &str, initial: bool) -> Value {
        let mut request = json!({"harness_id":"test","title":"Test session"});
        if initial {
            request["initial_run"] = start(0);
        }
        self.post(
            &format!("/api/projects/{}/sessions", self.project),
            key,
            request,
            201,
        )
        .await
    }
}
fn user(text: &str) -> Value {
    json!({"role":"user","id":Uuid::now_v7(),"timestamp":0,"content":[{"type":"text","content":text}]})
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn project_harness_defaults_scope_required_policy_and_validation(pool: PgPool) {
    let app = App::new(pool).await;
    let other = Uuid::now_v7();
    sqlx::query("insert into projects(project_id,name) values($1,'Fresh project')")
        .bind(other)
        .execute(&app.pool)
        .await
        .unwrap();
    let list = app.get(&format!("/api/projects/{other}/harnesses")).await;
    let items = list["items"].as_array().unwrap();
    let env = items.iter().find(|h| h["id"] == "environments").unwrap();
    assert_eq!(env["project_policy"], "required");
    assert_eq!(env["project_enabled"], true);
    assert_eq!(env["available"], true);
    let sites = items.iter().find(|h| h["id"] == "sites").unwrap();
    assert_eq!(sites["project_policy"], "required");
    assert_eq!(sites["project_enabled"], true);
    assert_eq!(sites["available"], true);
    let required_path = format!("/api/projects/{other}/harnesses/sites");
    let denied = app
        .check(
            Method::PUT,
            &required_path,
            None,
            Some(json!({"enabled":false})),
            409,
        )
        .await;
    assert_eq!(denied["error"]["code"], "PROJECT_HARNESS_REQUIRED");
    let optional = items.iter().find(|h| h["id"] == "test").unwrap();
    assert_eq!(optional["project_policy"], "opt_in");
    assert_eq!(optional["globally_enabled"], true);
    assert_eq!(optional["project_enabled"], false);
    assert_eq!(optional["available"], false);
    for initial in [false, true] {
        let mut body = json!({"harness_id":"test"});
        if initial {
            body["initial_run"] = start(0);
        }
        let denied = app
            .post(
                &format!("/api/projects/{other}/sessions"),
                &format!("denied-{initial}"),
                body,
                409,
            )
            .await;
        assert_eq!(denied["error"]["code"], "PROJECT_HARNESS_DISABLED");
    }
    let path = format!("/api/projects/{other}/harnesses/test");
    for invalid in [
        json!({}),
        json!({"enabled":null}),
        json!({"enabled":"true"}),
        json!({"enabled":true,"project_policy":"required"}),
    ] {
        app.check(Method::PUT, &path, None, Some(invalid), 422)
            .await;
    }
    let enabled = app
        .check(Method::PUT, &path, None, Some(json!({"enabled":true})), 200)
        .await;
    assert_eq!(enabled["available"], true);
    assert_eq!(
        app.check(Method::PUT, &path, None, Some(json!({"enabled":true})), 200)
            .await,
        enabled
    );
    // Project grants cannot override the global switch.
    sqlx::query("update harnesses set enabled=false where id='test'")
        .execute(&app.pool)
        .await
        .unwrap();
    let disabled = app
        .check(Method::PUT, &path, None, Some(json!({"enabled":true})), 200)
        .await;
    assert_eq!(disabled["project_enabled"], true);
    assert_eq!(disabled["available"], false);
    assert_eq!(
        app.post(
            &format!("/api/projects/{other}/sessions"),
            "global-disabled",
            json!({"harness_id":"test"}),
            409
        )
        .await["error"]["code"],
        "HARNESS_DISABLED"
    );
    let required = format!("/api/projects/{other}/harnesses/environments");
    assert_eq!(
        app.check(
            Method::PUT,
            &required,
            None,
            Some(json!({"enabled":false})),
            409
        )
        .await["error"]["code"],
        "PROJECT_HARNESS_REQUIRED"
    );
    app.check(
        Method::PUT,
        &required,
        None,
        Some(json!({"enabled":true})),
        200,
    )
    .await;
    let count: i64 = sqlx::query_scalar(
        "select count(*) from project_harnesses where harness_id='environments'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
    for path in [
        format!("/api/projects/{}/harnesses/test", Uuid::now_v7()),
        format!("/api/projects/{other}/harnesses/missing"),
    ] {
        app.check(Method::PUT, &path, None, Some(json!({"enabled":true})), 404)
            .await;
    }
    app.check(
        Method::GET,
        &format!("/api/projects/{}/harnesses", Uuid::now_v7()),
        None,
        None,
        404,
    )
    .await;
    app.check(
        Method::PUT,
        "/api/projects/not-a-uuid/harnesses/test",
        None,
        Some(json!({"enabled":true})),
        400,
    )
    .await;
    app.check(
        Method::PUT,
        &format!("/api/projects/{other}/harnesses/Bad"),
        None,
        Some(json!({"enabled":true})),
        400,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn disabling_preserves_history_blocks_new_work_and_reenabling_preserves_config(pool: PgPool) {
    let app = App::new(pool).await;
    let created = app.create("old-session", false).await;
    let session = uuid(&created["session"]["id"]);
    let path = format!("/api/projects/{}/harnesses/test", app.project);
    app.check(
        Method::PUT,
        &path,
        None,
        Some(json!({"enabled":false})),
        200,
    )
    .await;
    assert_eq!(
        app.get(&format!("/api/sessions/{session}")).await["config"],
        created["session"]["config"]
    );
    assert_eq!(
        app.get(&format!("/api/projects/{}/sessions", app.project))
            .await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Replaying accepted work remains valid; it does not create another session.
    assert_eq!(app.create("old-session", false).await, created);
    for (url, body) in [
        (format!("/api/sessions/{session}/runs"), start(0)),
        (
            format!("/api/sessions/{session}/forks"),
            json!({"at_revision":0}),
        ),
        (
            format!("/api/projects/{}/sessions", app.project),
            json!({"harness_id":"test"}),
        ),
    ] {
        assert_eq!(
            app.post(&url, &Uuid::now_v7().to_string(), body, 409).await["error"]["code"],
            "PROJECT_HARNESS_DISABLED"
        );
    }
    app.check(Method::PUT, &path, None, Some(json!({"enabled":true})), 200)
        .await;
    let started = app
        .post(
            &format!("/api/sessions/{session}/runs"),
            "start",
            start(0),
            201,
        )
        .await;
    assert_eq!(started["run"]["config"], created["session"]["config"]);
    assert_eq!(
        app.check(
            Method::PUT,
            &path,
            None,
            Some(json!({"enabled":false})),
            409
        )
        .await["error"]["code"],
        "PROJECT_HARNESS_ACTIVE_RUNS"
    );
    fail(&app, uuid(&started["run"]["id"])).await;
    app.check(
        Method::PUT,
        &path,
        None,
        Some(json!({"enabled":false})),
        200,
    )
    .await;
    assert!(
        app.get(&format!("/api/runs/{}", uuid(&started["run"]["id"])))
            .await
            .is_object()
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn disabling_racing_run_creation_never_leaves_disabled_active_work(pool: PgPool) {
    let app = App::new(pool).await;
    let path = format!("/api/projects/{}/harnesses/test", app.project);
    for index in 0..12 {
        app.check(Method::PUT, &path, None, Some(json!({"enabled":true})), 200)
            .await;
        let url = format!("/api/projects/{}/sessions", app.project);
        let key = format!("race-{index}");
        let (creation, disable) = tokio::join!(
            app.raw(
                Method::POST,
                &url,
                Some(&key),
                Some(json!({"harness_id":"test","initial_run":start(0)}))
            ),
            app.raw(Method::PUT, &path, None, Some(json!({"enabled":false})))
        );
        let statuses = (creation.status().as_u16(), disable.status().as_u16());
        assert!(
            matches!(statuses, (201, 409) | (409, 200)),
            "unexpected race result: {statuses:?}"
        );
        let body: Value = creation.json().await.unwrap();
        if statuses.0 == 201 {
            fail(&app, uuid(&body["run"]["id"])).await;
        }
    }
}
fn start(revision: i64) -> Value {
    json!({"input":user("Start"),"expected_session_revision":revision})
}
fn uuid(v: &Value) -> Uuid {
    v.as_str().unwrap().parse().unwrap()
}
async fn add_message(app: &App, session: Uuid, run: Uuid, text: &str) -> Uuid {
    let id = Uuid::now_v7();
    let mut tx = app.pool.begin().await.unwrap();
    sqlx::query("insert into messages(id,project_id,origin_run_id,message) values($1,$2,$3,$4)")
        .bind(id)
        .bind(app.project)
        .bind(run)
        .bind(user(text))
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "insert into session_messages(project_id,session_id,message_id,run_id) values($1,$2,$3,$4)",
    )
    .bind(app.project)
    .bind(session)
    .bind(id)
    .bind(run)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}
async fn fail(app: &App, run: Uuid) {
    let mut tx = app.pool.begin().await.unwrap();
    sqlx::query("update runs set status='failed',available_at=null,worker_id=null,lease_expires_at=null,version=version+1,error='{}',finished_at=clock_timestamp() where id=$1").bind(run).execute(&mut *tx).await.unwrap();
    sqlx::query(
        "insert into run_events(id,run_id,type,source) values($1,$2,'run.failed','runtime')",
    )
    .bind(Uuid::now_v7())
    .bind(run)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
async fn make_wait(app: &App, run: Uuid, mode: &str, keys: &[&str]) -> Uuid {
    let id = Uuid::now_v7();
    let mut tx = app.pool.begin().await.unwrap();
    sqlx::query("insert into run_waits(id,project_id,run_id,wait_key,mode) values($1,$2,$3,$4,$5)")
        .bind(id)
        .bind(app.project)
        .bind(run)
        .bind(id.to_string())
        .bind(mode)
        .execute(&mut *tx)
        .await
        .unwrap();
    for key in keys {
        sqlx::query("insert into run_wait_dependencies(id,project_id,wait_id,kind,input_kind,correlation_key) values($1,$2,$3,'input','approval_response',$4)").bind(Uuid::now_v7()).bind(app.project).bind(id).bind(key).execute(&mut *tx).await.unwrap();
    }
    sqlx::query("update runs set status='waiting',available_at=null,version=version+1 where id=$1")
        .bind(run)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn all_application_routes_and_cooperative_abort(pool: PgPool) {
    let app = App::new(pool).await;
    assert!(
        app.get("/api/harnesses").await["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["id"] == "test")
    );
    assert_eq!(app.get("/api/harnesses/test").await["enabled"], true);
    let created = app.create("create", false).await;
    assert!(created["run"].is_null());
    let session = uuid(&created["session"]["id"]);
    assert_eq!(
        app.get(&format!("/api/projects/{}/sessions", app.project))
            .await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        app.get(&format!("/api/sessions/{session}")).await["current_revision"],
        0
    );
    let patched = app
        .check(
            Method::PATCH,
            &format!("/api/sessions/{session}"),
            None,
            Some(json!({"title":"Renamed"})),
            200,
        )
        .await;
    assert_eq!(patched["title"], "Renamed");
    let request = start(0);
    let started = app
        .post(
            &format!("/api/sessions/{session}/runs"),
            "start",
            request.clone(),
            201,
        )
        .await;
    assert_eq!(
        app.post(
            &format!("/api/sessions/{session}/runs"),
            "start",
            request,
            201
        )
        .await,
        started
    );
    let run = uuid(&started["run"]["id"]);
    assert_eq!(started["run"]["status"], "ready");
    assert!(started["run"].get("worker_id").is_none());
    assert_eq!(
        app.get(&format!("/api/sessions/{session}/runs")).await["items"][0]["id"],
        run.to_string()
    );
    assert_eq!(
        app.get(&format!("/api/runs/{run}")).await["status"],
        "ready"
    );
    assert!(
        app.get(&format!("/api/runs/{run}/children")).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        app.get(&format!("/api/sessions/{session}/messages")).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let input_body = json!({"kind":"user_message","message":user("Steer")});
    let accepted = app
        .post(
            &format!("/api/runs/{run}/inputs"),
            "steer",
            input_body.clone(),
            201,
        )
        .await;
    assert_eq!(accepted["input"]["sequence"], 2);
    assert_eq!(accepted["input"]["status"], "pending");
    assert_eq!(
        app.post(&format!("/api/runs/{run}/inputs"), "steer", input_body, 201)
            .await,
        accepted
    );
    let inputs = app.get(&format!("/api/runs/{run}/inputs")).await;
    assert_eq!(inputs["items"].as_array().unwrap().len(), 2);
    assert!(inputs["items"][0].get("deduplication_key").is_none());
    assert!(
        app.get(&format!("/api/runs/{run}/waits")).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let fork = app
        .post(
            &format!("/api/sessions/{session}/forks"),
            "fork",
            json!({"at_revision":0}),
            201,
        )
        .await;
    assert_eq!(
        fork["session"]["forked_from_session_id"],
        session.to_string()
    );
    let aborted = app
        .post(
            &format!("/api/runs/{run}/abort"),
            "abort",
            json!({"reason":"Stop"}),
            202,
        )
        .await;
    assert_eq!(aborted["run"]["status"], "aborted");
    assert!(aborted["run"]["started_at"].is_null());
    assert!(aborted["run"]["finished_at"].is_string());
    assert!(aborted["run"]["abort_requested_at"].is_string());
    assert_eq!(aborted["input"]["kind"], "abort");
    let again = app
        .post(
            &format!("/api/runs/{run}/abort"),
            "abort-again",
            json!({}),
            202,
        )
        .await;
    assert_eq!(again["input"], aborted["input"]);
    let events = app.get(&format!("/api/runs/{run}/events")).await;
    assert_eq!(events["items"].as_array().unwrap().len(), 4);
    assert_eq!(events["items"][2]["type"], "run.abort_requested");
    assert_eq!(events["items"][3]["type"], "run.aborted");
    let inputs = app.get(&format!("/api/runs/{run}/inputs")).await;
    for input in inputs["items"].as_array().unwrap() {
        assert_eq!(
            input["status"],
            if input["kind"] == "abort" {
                "handled"
            } else {
                "rejected"
            }
        );
        assert_eq!(input["handling"]["reason"], "aborted_before_start");
    }
    assert!(app.get(&format!("/api/sessions/{session}")).await["active_run"].is_null());
    assert_eq!(
        app.post(
            &format!("/api/runs/{run}/abort"),
            "abort",
            json!({"reason":"Stop"}),
            202
        )
        .await,
        aborted
    );
    app.post(
        &format!("/api/runs/{run}/inputs"),
        "late",
        json!({"kind":"user_message","message":user("Late")}),
        409,
    )
    .await;
    let stream = app
        .raw(
            Method::GET,
            &format!("/api/runs/{run}/events/stream"),
            None,
            None,
        )
        .await;
    assert_eq!(stream.status(), 200);
    assert_eq!(stream.headers()["content-type"], "text/event-stream");
    let body = stream.text().await.unwrap();
    assert!(body.contains("event: run.created"), "{body}");
    assert!(body.contains("event: run.aborted"), "{body}");
    assert!(body.contains("id: 4"), "{body}");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn creation_configuration_validation_and_atomic_failure(pool: PgPool) {
    let app = App::new(pool).await;
    let path = format!("/api/projects/{}/sessions", app.project);
    let request = json!({"harness_id":"test","initial_run":start(0),"config_override":{"nested":{"change":2},"remove":null}});
    let created = app.post(&path, "good", request.clone(), 201).await;
    assert_eq!(
        created["run"]["config"],
        json!({"model":"test-model","nested":{"keep":true,"change":2}})
    );
    assert_eq!(app.post(&path, "good", request.clone(), 201).await, created);
    let mut changed = request.clone();
    changed["title"] = json!("Other");
    app.post(&path, "good", changed, 409).await;
    let mut invalid = request.clone();
    invalid["config_override"] = json!({"model":null});
    app.post(&path, "bad-config", invalid, 422).await;
    let mut stale = request.clone();
    stale["initial_run"]["expected_session_revision"] = json!(5);
    app.post(&path, "bad-revision", stale, 409).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from sessions")
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from runtime_requests")
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        1
    );
    sqlx::query("update harnesses set enabled=false where id='test'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert!(
        app.get("/api/harnesses").await["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["id"] != "test")
    );
    assert_eq!(app.get("/api/harnesses/test").await["enabled"], false);
    app.post(&path, "disabled", request.clone(), 409).await;
    assert_eq!(app.post(&path, "good", request, 201).await, created);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn session_configuration_is_frozen_and_forks_can_override(pool: PgPool) {
    let app = App::new(pool).await;
    let created = app.create("frozen", true).await;
    let session = uuid(&created["session"]["id"]);
    let run = uuid(&created["run"]["id"]);
    let original = created["session"]["config"].clone();
    assert_eq!(original, created["run"]["config"]);
    sqlx::query("update harnesses set default_config=jsonb_set(default_config,'{model}','\"new-default\"') where id='test'")
        .execute(&app.pool).await.unwrap();
    fail(&app, run).await;
    let followup = app
        .post(
            &format!("/api/sessions/{session}/runs"),
            "followup",
            start(0),
            201,
        )
        .await;
    assert_eq!(followup["run"]["config"], original);
    let mut override_run = start(0);
    override_run["config_override"] = json!({});
    app.post(
        &format!("/api/sessions/{session}/runs"),
        "no-override",
        override_run,
        422,
    )
    .await;
    app.check(
        Method::PATCH,
        &format!("/api/sessions/{session}"),
        None,
        Some(json!({"config":{}})),
        422,
    )
    .await;
    let fork = app
        .post(
            &format!("/api/sessions/{session}/forks"),
            "fork",
            json!({"at_revision":0,"config_override":{"nested":{"change":42}}}),
            201,
        )
        .await;
    assert_eq!(fork["session"]["config"]["model"], original["model"]);
    assert_eq!(fork["session"]["config"]["nested"]["keep"], true);
    assert_eq!(fork["session"]["config"]["nested"]["change"], 42);
    assert_eq!(
        app.get(&format!("/api/sessions/{session}")).await["config"],
        original
    );
    assert!(
        sqlx::query("update sessions set config='{}' where id=$1")
            .bind(session)
            .execute(&app.pool)
            .await
            .is_err()
    );

    let path = format!("/api/sessions/{session}/inputs?limit=1");
    let page = app.get(&path).await;
    assert_eq!(page["items"][0]["status"], "pending");
    assert_eq!(page["items"][0]["id"], created["input"]["id"]);
    assert!(page["items"][0].get("deduplication_key").is_none());
    let next = app
        .get(&format!(
            "{path}&cursor={}",
            page["next_cursor"].as_str().unwrap()
        ))
        .await;
    assert_eq!(next["items"][0]["id"], followup["input"]["id"]);
    assert!(next["next_cursor"].is_null());
    assert!(
        app.get(&format!("/api/sessions/{session}/messages")).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let fork_id = fork["session"]["id"].as_str().unwrap();
    assert!(
        app.get(&format!("/api/sessions/{fork_id}/inputs")).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn concurrent_idempotency_and_one_active_run(pool: PgPool) {
    let app = App::new(pool).await;
    let path = format!("/api/projects/{}/sessions", app.project);
    let request = json!({"harness_id":"test","initial_run":start(0)});
    let (a, b) = tokio::join!(
        app.post(&path, "same", request.clone(), 201),
        app.post(&path, "same", request, 201)
    );
    assert_eq!(a, b);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from runs")
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        1
    );
    let session = app.create("empty", false).await["session"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/sessions/{session}/runs");
    let request = start(0);
    let (a, b) = tokio::join!(
        app.raw(Method::POST, &path, Some("a"), Some(request.clone())),
        app.raw(Method::POST, &path, Some("b"), Some(request))
    );
    let mut statuses = [a.status().as_u16(), b.status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [201, 409]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from run_inputs")
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        2
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn immutable_history_forks_and_revision_checks(pool: PgPool) {
    let app = App::new(pool).await;
    let source = app.create("source", true).await;
    let session = uuid(&source["session"]["id"]);
    let run = uuid(&source["run"]["id"]);
    let first = add_message(&app, session, run, "First").await;
    add_message(&app, session, run, "Second").await;
    let fork_body = json!({"at_revision":1,"initial_run":start(1)});
    let fork = app
        .post(
            &format!("/api/sessions/{session}/forks"),
            "fork",
            fork_body.clone(),
            201,
        )
        .await;
    let child = uuid(&fork["session"]["id"]);
    assert_eq!(fork["session"]["current_revision"], 1);
    assert!(fork["run"]["parent_run_id"].is_null());
    let history = app.get(&format!("/api/sessions/{child}/messages")).await;
    assert_eq!(history["items"].as_array().unwrap().len(), 1);
    assert_eq!(history["items"][0]["message_id"], first.to_string());
    assert!(history["items"][0]["run_id"].is_null());
    assert_eq!(history["items"][0]["origin_run_id"], run.to_string());
    add_message(&app, session, run, "Third").await;
    assert_eq!(
        app.post(
            &format!("/api/sessions/{session}/forks"),
            "fork",
            fork_body,
            201
        )
        .await,
        fork
    );
    app.post(
        &format!("/api/sessions/{session}/forks"),
        "future",
        json!({"at_revision":99}),
        409,
    )
    .await;
    fail(&app, run).await;
    app.post(
        &format!("/api/sessions/{session}/runs"),
        "stale",
        start(2),
        409,
    )
    .await;
    app.post(
        &format!("/api/sessions/{session}/runs"),
        "new",
        start(3),
        201,
    )
    .await;
    let page = app
        .get(&format!("/api/sessions/{session}/messages?limit=1"))
        .await;
    assert_eq!(page["next_after_revision"], 1);
    let page = app
        .get(&format!(
            "/api/sessions/{session}/messages?limit=2&after_revision=1"
        ))
        .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert!(page["next_after_revision"].is_null());
    app.check(
        Method::GET,
        &format!("/api/sessions/{child}/messages?run_id={run}"),
        None,
        None,
        404,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn input_resolves_waits_and_wakes_without_acknowledging_model_delivery(pool: PgPool) {
    let app = App::new(pool).await;
    let source = app.create("source", true).await;
    let run = uuid(&source["run"]["id"]);
    let wait = make_wait(&app, run, "all", &["a", "b"]).await;
    let path = format!("/api/runs/{run}/inputs");
    let first = app
        .post(
            &path,
            "a",
            json!({"kind":"approval_response","correlation_key":"a","approved":true}),
            201,
        )
        .await;
    assert_eq!(first["run"]["status"], "ready");
    assert_eq!(first["input"]["status"], "pending");
    let waits = app.get(&format!("/api/runs/{run}/waits")).await;
    assert_eq!(waits["items"][0]["status"], "pending");
    assert_eq!(
        waits["items"][0]["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| !d["satisfied_at"].is_null())
            .count(),
        1
    );
    sqlx::query("update runs set status='waiting',available_at=null,version=version+1 where id=$1")
        .bind(run)
        .execute(&app.pool)
        .await
        .unwrap();
    app.post(
        &path,
        "b",
        json!({"kind":"approval_response","correlation_key":"b","approved":false}),
        201,
    )
    .await;
    let waits = app
        .get(&format!("/api/runs/{run}/waits?status=satisfied"))
        .await;
    assert_eq!(waits["items"][0]["id"], wait.to_string());
    assert_eq!(waits["items"][0]["status"], "satisfied");
    let any = make_wait(&app, run, "any", &["c", "d"]).await;
    app.post(
        &path,
        "c",
        json!({"kind":"approval_response","correlation_key":"c","approved":true}),
        201,
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("select status from run_waits where id=$1")
            .bind(any)
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        "satisfied"
    );
    let unmatched = make_wait(&app, run, "any", &["unmatched"]).await;
    app.post(
        &path,
        "message",
        json!({"kind":"user_message","message":user("Update")}),
        201,
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("select status from run_waits where id=$1")
            .bind(unmatched)
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        "pending"
    );
    sqlx::query("update runs set status='waiting',available_at=null,version=version+1 where id=$1")
        .bind(run)
        .execute(&app.pool)
        .await
        .unwrap();
    let aborted = app
        .post(&format!("/api/runs/{run}/abort"), "abort", json!({}), 202)
        .await;
    assert_eq!(aborted["run"]["status"], "ready");
    assert_eq!(
        sqlx::query_scalar::<_, String>("select status from run_waits where id=$1")
            .bind(unmatched)
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        "pending"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn validation_pagination_archiving_and_not_found(pool: PgPool) {
    let app = App::new(pool).await;
    let path = format!("/api/projects/{}/sessions", app.project);
    app.check(
        Method::POST,
        &path,
        None,
        Some(json!({"harness_id":"test"})),
        400,
    )
    .await;
    app.post(
        &path,
        "unknown",
        json!({"harness_id":"test","unknown":true}),
        422,
    )
    .await;
    app.post(&path,"bad-message",json!({"harness_id":"test","initial_run":{"input":{"role":"user","content":[]},"expected_session_revision":0}}),422).await;
    app.check(Method::GET, "/api/sessions/not-uuid", None, None, 400)
        .await;
    app.check(Method::GET, &format!("{path}?limit=201"), None, None, 400)
        .await;
    app.check(Method::GET, &format!("{path}?unknown=1"), None, None, 400)
        .await;
    app.check(
        Method::GET,
        &format!("{path}?cursor=garbage"),
        None,
        None,
        400,
    )
    .await;
    app.check(
        Method::GET,
        &format!("/api/projects/{}/sessions", Uuid::now_v7()),
        None,
        None,
        404,
    )
    .await;
    app.check(Method::GET, "/api/harnesses/missing", None, None, 404)
        .await;
    let first = app.create("first", false).await;
    let first = uuid(&first["session"]["id"]);
    app.create("second", false).await;
    app.create("third", false).await;
    let page = app.get(&format!("{path}?limit=2")).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let cursor = page["next_cursor"].as_str().unwrap();
    let next = app.get(&format!("{path}?limit=2&cursor={cursor}")).await;
    assert_eq!(next["items"].as_array().unwrap().len(), 1);
    assert!(next["next_cursor"].is_null());
    app.check(
        Method::GET,
        &format!("{path}?archived=true&cursor={cursor}"),
        None,
        None,
        400,
    )
    .await;
    let session_path = format!("/api/sessions/{first}");
    app.check(
        Method::PATCH,
        &session_path,
        None,
        Some(json!({"archived":null})),
        422,
    )
    .await;
    app.check(Method::PATCH, &session_path, None, Some(json!({})), 400)
        .await;
    let archived = app
        .check(
            Method::PATCH,
            &session_path,
            None,
            Some(json!({"title":null,"archived":true})),
            200,
        )
        .await;
    assert!(archived["title"].is_null());
    assert!(archived["archived_at"].is_string());
    assert_eq!(
        app.get(&format!("{path}?archived=true")).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    app.post(&format!("{session_path}/runs"), "archived", start(0), 409)
        .await;
    app.check(
        Method::PATCH,
        &session_path,
        None,
        Some(json!({"archived":false})),
        200,
    )
    .await;
    let run = app
        .post(&format!("{session_path}/runs"), "unarchived", start(0), 201)
        .await;
    let run = uuid(&run["run"]["id"]);
    app.check(
        Method::GET,
        &format!("/api/runs/{run}/inputs?status=bogus"),
        None,
        None,
        400,
    )
    .await;
    app.check(
        Method::GET,
        &format!("/api/runs/{run}/events?after_sequence=-1"),
        None,
        None,
        400,
    )
    .await;
    app.check(
        Method::GET,
        &format!("/api/runs/{run}/events/stream?after_sequence=-1"),
        None,
        None,
        400,
    )
    .await;
    let response = app
        .client
        .get(format!("{}/api/runs/{run}/events/stream", app.url))
        .header("Last-Event-ID", "invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    app.post(
        &format!("/api/runs/{run}/inputs"),
        "internal",
        json!({"kind":"operation_result","payload":{}}),
        422,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn stream_replay_external_commits_terminal_drain_and_disconnect(pool: PgPool) {
    let app = App::new(pool).await;
    let created = app.create("create", true).await;
    let run = uuid(&created["run"]["id"]);
    app.post(
        &format!("/api/runs/{run}/inputs"),
        "input",
        json!({"kind":"user_message","message":user("Next")}),
        201,
    )
    .await;
    let events = app.get(&format!("/api/runs/{run}/events?limit=1")).await;
    assert_eq!(events["next_after_sequence"], 1);
    let inputs = app.get(&format!("/api/runs/{run}/inputs?limit=1")).await;
    assert_eq!(inputs["next_after_sequence"], 1);
    let inputs = app
        .get(&format!(
            "/api/runs/{run}/inputs?after_sequence=1&status=pending"
        ))
        .await;
    assert_eq!(inputs["items"].as_array().unwrap().len(), 1);
    let mut response = app
        .client
        .get(format!(
            "{}/api/runs/{run}/events/stream?after_sequence=0",
            app.url
        ))
        .header("Last-Event-ID", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-accel-buffering"], "no");
    let first = tokio::time::timeout(Duration::from_secs(3), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let first = String::from_utf8(first.to_vec()).unwrap();
    assert!(!first.contains("event: run.created"), "{first}");
    assert!(first.contains("id: 2"), "{first}");
    // Simulates a write in a different worker/process (no local broadcast).
    sqlx::query("insert into run_events(id,run_id,type,source,payload) values($1,$2,'progress','harness','{}')").bind(Uuid::now_v7()).bind(run).execute(&app.pool).await.unwrap();
    let next = tokio::time::timeout(Duration::from_secs(4), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        String::from_utf8(next.to_vec())
            .unwrap()
            .contains("event: progress")
    );
    fail(&app, run).await;
    let rest = tokio::time::timeout(Duration::from_secs(4), response.text())
        .await
        .unwrap()
        .unwrap();
    assert!(rest.contains("event: run.failed"), "{rest}");
    let complete = app
        .client
        .get(format!("{}/api/runs/{run}/events/stream", app.url))
        .header("Last-Event-ID", "4")
        .send()
        .await
        .unwrap();
    assert_eq!(complete.text().await.unwrap(), "");
    let created = app.create("live", true).await;
    let run = uuid(&created["run"]["id"]);
    let response = app
        .raw(
            Method::GET,
            &format!("/api/runs/{run}/events/stream?after_sequence=1"),
            None,
            None,
        )
        .await;
    drop(response);
    assert_eq!(
        app.get(&format!("/api/runs/{run}")).await["status"],
        "ready"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn repeated_abort_repairs_legacy_unstarted_run(pool: PgPool) {
    let app = App::new(pool).await;
    let created = app.create("queued", true).await;
    let run = uuid(&created["run"]["id"]);
    let input = Uuid::now_v7();
    // Reproduce the previous server's durable state after aborting an unclaimed run.
    let mut tx = app.pool.begin().await.unwrap();
    sqlx::query("insert into run_inputs(id,project_id,run_id,kind,payload,deduplication_key) values($1,$2,$3,'abort','{}','runtime:abort')")
        .bind(input).bind(app.project).bind(run).execute(&mut *tx).await.unwrap();
    sqlx::query(
        "update runs set abort_requested_at=clock_timestamp(),version=version+1 where id=$1",
    )
    .bind(run)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("insert into run_events(id,run_id,type,source,payload) values($1,$2,'run.abort_requested','runtime',$3)")
        .bind(Uuid::now_v7()).bind(run).bind(json!({"input_id":input})).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();

    let repaired = app
        .post(
            &format!("/api/runs/{run}/abort"),
            "retry-abort",
            json!({}),
            202,
        )
        .await;
    assert_eq!(repaired["run"]["status"], "aborted");
    assert!(repaired["run"]["started_at"].is_null());
    assert_eq!(repaired["input"]["id"], input.to_string());
    assert_eq!(repaired["input"]["status"], "handled");
    let events = app.get(&format!("/api/runs/{run}/events")).await;
    assert_eq!(events["items"].as_array().unwrap().len(), 3);
    assert_eq!(events["items"][2]["type"], "run.aborted");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn active_worker_ownership_is_preserved_and_abort_does_not_cascade(pool: PgPool) {
    let app = App::new(pool).await;
    let source = app.create("parent", true).await;
    let parent = uuid(&source["run"]["id"]);
    let worker = Uuid::now_v7();
    sqlx::query("insert into workers(id,build_id,supported_harnesses,capacity) values($1,'test',array['test'],4)").bind(worker).execute(&app.pool).await.unwrap();
    sqlx::query("update runs set status='running',available_at=null,worker_id=$2,lease_epoch=1,lease_expires_at=clock_timestamp()+interval '1 hour',started_at=clock_timestamp(),version=version+1 where id=$1").bind(parent).bind(worker).execute(&app.pool).await.unwrap();
    let mut children = Vec::new();
    for index in 0..3 {
        let session = uuid(&app.create(&format!("child-{index}"), false).await["session"]["id"]);
        let child = Uuid::now_v7();
        sqlx::query("insert into runs(id,project_id,session_id,parent_run_id,config) values($1,$2,$3,$4,(select config from sessions where id=$3))")
            .bind(child)
            .bind(app.project)
            .bind(session)
            .bind(parent)
            .execute(&app.pool)
            .await
            .unwrap();
        children.push(child);
    }
    let page = app
        .get(&format!("/api/runs/{parent}/children?limit=2&status=ready"))
        .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let cursor = page["next_cursor"].as_str().unwrap();
    let next = app
        .get(&format!(
            "/api/runs/{parent}/children?limit=2&status=ready&cursor={cursor}"
        ))
        .await;
    assert_eq!(next["items"].as_array().unwrap().len(), 1);
    assert!(next["next_cursor"].is_null());
    let accepted = app
        .post(
            &format!("/api/runs/{parent}/inputs"),
            "steer",
            json!({"kind":"user_message","message":user("Mid-run update")}),
            201,
        )
        .await;
    assert_eq!(accepted["run"]["status"], "running");
    assert_eq!(accepted["run"]["version"], 2);
    let abort = app
        .post(
            &format!("/api/runs/{parent}/abort"),
            "abort",
            json!({}),
            202,
        )
        .await;
    assert_eq!(abort["run"]["status"], "running");
    let owner: (Uuid, i64) = sqlx::query_as("select worker_id,lease_epoch from runs where id=$1")
        .bind(parent)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(owner, (worker, 1));
    for child in children {
        let child = app.get(&format!("/api/runs/{child}")).await;
        assert_eq!(child["status"], "ready");
        assert!(child["abort_requested_at"].is_null());
    }
    let session = uuid(&source["session"]["id"]);
    app.check(
        Method::PATCH,
        &format!("/api/sessions/{session}"),
        None,
        Some(json!({"archived":true})),
        200,
    )
    .await;
    assert_eq!(
        app.get(&format!("/api/runs/{parent}")).await["status"],
        "running"
    );
    // A previously claimed run can be ready again after yielding. It still needs
    // worker cleanup and must not take the never-started cancellation shortcut.
    sqlx::query("update runs set status='ready',available_at=clock_timestamp(),worker_id=null,lease_expires_at=null,version=version+1 where id=$1")
        .bind(parent).execute(&app.pool).await.unwrap();
    let yielded = app
        .post(
            &format!("/api/runs/{parent}/abort"),
            "abort-yielded",
            json!({}),
            202,
        )
        .await;
    assert_eq!(yielded["run"]["status"], "ready");
    assert!(yielded["run"]["finished_at"].is_null());
    assert_eq!(yielded["input"]["status"], "pending");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn invalid_requests_are_bounded_and_do_not_create_state(pool: PgPool) {
    let app = App::new(pool).await;
    let path = format!("/api/projects/{}/sessions", app.project);
    for key in ["", "contains space"] {
        app.post(&path, key, json!({"harness_id":"test"}), 400)
            .await;
    }
    let response = app
        .client
        .post(format!("{}{path}", app.url))
        .header("Idempotency-Key", "bad-json")
        .header("content-type", "application/json")
        .body("{")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let response = app
        .client
        .post(format!("{}{path}", app.url))
        .header("Idempotency-Key", "no-content-type")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 415);
    let mut request = json!({"harness_id":"test","initial_run":start(0)});
    request["initial_run"]["input"]["role"] = json!("system");
    request["initial_run"]["input"]["content"] = json!([{"content":"System instructions"}]);
    app.post(&path, "system", request, 400).await;
    let mut request = json!({"harness_id":"test","initial_run":start(0)});
    request["initial_run"]["input"]["content"] = json!([]);
    app.post(&path, "empty-content", request, 400).await;
    let mut request = json!({"harness_id":"test","initial_run":start(0)});
    request["config_override"] = json!({"large":"x".repeat(65536)});
    app.post(&path, "large-config", request, 400).await;
    app.post(
        &path,
        "large-body",
        json!({"harness_id":"test","title":"x".repeat(1024*1024)}),
        413,
    )
    .await;
    sqlx::query("update harnesses set config_schema=$1 where id='test'")
        .bind(json!({"type":"not-a-json-schema-type"}))
        .execute(&app.pool)
        .await
        .unwrap();
    app.post(
        &path,
        "broken-schema",
        json!({"harness_id":"test","initial_run":start(0)}),
        500,
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from sessions")
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from runtime_requests")
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn idle_stream_does_not_reserve_a_database_connection(pool: PgPool) {
    // A single-connection HTTP pool makes an accidental connection retained by
    // an idle SSE stream block every subsequent request.
    let limited = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let app = App::new(limited).await;
    let created = app.create("create", true).await;
    let run = uuid(&created["run"]["id"]);
    let response = app
        .raw(
            Method::GET,
            &format!("/api/runs/{run}/events/stream?after_sequence=1"),
            None,
            None,
        )
        .await;
    let record = tokio::time::timeout(Duration::from_secs(3), app.get(&format!("/api/runs/{run}")))
        .await
        .unwrap();
    assert_eq!(record["status"], "ready");
    app.post(
        &format!("/api/runs/{run}/inputs"),
        "input",
        json!({"kind":"user_message","message":user("While streaming")}),
        201,
    )
    .await;
    drop(response);
    app.pool.close().await;
}

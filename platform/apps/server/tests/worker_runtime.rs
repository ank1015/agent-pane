//! Contract tests exercise real HTTP and disposable PostgreSQL databases.

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn availability_is_authenticated_advisory_and_matches_claim_eligibility(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(1).await;
    let path = format!("/internal/workers/{}/work-available", w.id);
    app.request(Method::GET, &path, None, 0, None, None, 401)
        .await;
    app.request(
        Method::GET,
        &format!("{path}?wait_seconds=26"),
        Some(&w),
        0,
        None,
        None,
        400,
    )
    .await;
    app.request(
        Method::GET,
        &format!("{path}?unexpected=1"),
        Some(&w),
        0,
        None,
        None,
        400,
    )
    .await;
    assert_eq!(app.availability(&w, 0).await["available"], false);
    let created = app.create().await;
    let run: Uuid = serde_json::from_value(created["run"]["id"].clone()).unwrap();
    assert_eq!(app.availability(&w, 0).await["available"], true);
    assert_eq!(app.availability(&w, 0).await["available"], true);
    let owned: Option<Uuid> = sqlx::query_scalar("select worker_id from runs where id=$1")
        .bind(run)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(owned, None, "availability must never claim");

    sqlx::query("update workers set supported_harnesses=array['other'] where id=$1")
        .bind(w.id)
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(app.availability(&w, 0).await["available"], false);
    sqlx::query("update workers set supported_harnesses=array['test'] where id=$1")
        .bind(w.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let other = app.register(1).await;
    let (a, b) = tokio::join!(app.availability(&w, 0), app.availability(&other, 0));
    assert_eq!(a["available"], true);
    assert_eq!(b["available"], true);
    let (a, b) = tokio::join!(app.claim(&w, "a", 1), app.claim(&other, "b", 1));
    assert_eq!(
        a["items"].as_array().unwrap().len() + b["items"].as_array().unwrap().len(),
        1
    );
    let winner = if a["items"].as_array().unwrap().is_empty() {
        &other
    } else {
        &w
    };
    app.create().await;
    assert_eq!(
        app.availability(winner, 0).await["available"],
        false,
        "capacity is enforced"
    );
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(run)
    .execute(&app.pool)
    .await
    .unwrap();
    assert_eq!(app.availability(winner, 0).await["available"], true);
    sqlx::query("update workers set status='draining' where id=$1")
        .bind(winner.id)
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(app.availability(winner, 0).await["available"], false);
    sqlx::query("update workers set status='offline' where id=$1")
        .bind(winner.id)
        .execute(&app.pool)
        .await
        .unwrap();
    app.request(
        Method::GET,
        &format!(
            "/internal/workers/{}/work-available?wait_seconds=0",
            winner.id
        ),
        Some(winner),
        0,
        None,
        None,
        409,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn availability_wakes_on_commit_without_holding_a_database_connection(pool: PgPool) {
    use std::time::{Duration, Instant};
    // A single-connection pool would deadlock creation if the wait held it.
    let single = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(1))
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let app = App::new(single).await;
    let w = app.register(2).await;
    let start = Instant::now();
    let (available, _) = tokio::join!(app.availability(&w, 5), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        app.create().await;
    });
    assert_eq!(available["available"], true);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "must wake before timeout/fallback"
    );
    app.pool.close().await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn availability_timeout_and_missing_hints_recheck_durable_state(pool: PgPool) {
    use std::time::{Duration, Instant};
    let app = App::new(pool).await;
    let w = app.register(2).await;
    let start = Instant::now();
    assert_eq!(app.availability(&w, 1).await["available"], false);
    assert!(start.elapsed() >= Duration::from_millis(900));
    let created = app.create().await;
    let run: Uuid = serde_json::from_value(created["run"]["id"].clone()).unwrap();
    sqlx::query("update runs set available_at=clock_timestamp()+interval '250 milliseconds',version=version+1 where id=$1").bind(run).execute(&app.pool).await.unwrap();
    assert_eq!(app.availability(&w, 0).await["available"], false);
    // No notification is emitted when wall time crosses available_at.
    assert_eq!(app.availability(&w, 1).await["available"], true);
    app.claim(&w, "claim", 1).await;
    let (available, _) = tokio::join!(app.availability(&w, 1), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Simulate lease expiration without a notification.
        sqlx::query(
            "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
        )
        .bind(run)
        .execute(&app.pool)
        .await
        .unwrap();
    });
    assert_eq!(available["available"], true);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn availability_observes_notifications_from_another_platform_replica(pool: PgPool) {
    use std::time::{Duration, Instant};
    let app = App::new(pool.clone()).await;
    let w = app.register(1).await;
    let notifications = app.runtime.spawn_notification_listener();
    // Wait until the dedicated PostgreSQL listener has connected.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let listening: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where datname=current_database() and application_name='platform-runtime-notifications')").fetch_one(&pool).await.unwrap();
            if listening { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let other = router(RuntimeService::new(pool.clone()));
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/api/projects/{}/sessions",
        socket.local_addr().unwrap(),
        app.project
    );
    let server = tokio::spawn(async move { axum::serve(socket, other).await.unwrap() });
    let start = Instant::now();
    let (available, _) = tokio::join!(app.availability(&w, 5), async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        app.client.post(url).header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&json!({"harness_id":"test","initial_run":{"input":user(),"expected_session_revision":0}}))
            .send().await.unwrap().error_for_status().unwrap();
    });
    notifications.abort();
    server.abort();
    assert_eq!(available["available"], true);
    assert!(start.elapsed() < Duration::from_secs(2));
}
use platform_server::runtime::{RuntimeService, router, worker_router};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
const BOOT: &str = "bootstrap-test-credential-01234567890123456789";
struct Worker {
    id: Uuid,
    token: String,
}
struct App {
    pool: PgPool,
    runtime: RuntimeService,
    client: Client,
    url: String,
    project: Uuid,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for App {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl App {
    async fn new(pool: PgPool) -> Self {
        let project = Uuid::now_v7();
        sqlx::query("insert into projects(project_id,name) values($1,'Worker tests')")
            .bind(project)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into harnesses(id,name) values('test','Test'),('other','Other')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into project_harnesses(project_id,harness_id,enabled) values($1,'test',true),($1,'other',true)")
            .bind(project).execute(&pool).await.unwrap();
        let runtime = RuntimeService::new(pool.clone());
        let app = router(runtime.clone()).merge(worker_router(runtime.clone(), BOOT));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            pool,
            runtime,
            client: Client::new(),
            url,
            project,
            task,
        }
    }
    // Keep wire-level tests explicit about credentials, lease, key, and status.
    #[allow(clippy::too_many_arguments)]
    async fn request(
        &self,
        method: Method,
        path: &str,
        w: Option<&Worker>,
        epoch: i64,
        key: Option<&str>,
        body: Option<Value>,
        status: u16,
    ) -> Value {
        let mut req = self
            .client
            .request(method, format!("{}{path}", self.url))
            .timeout(std::time::Duration::from_secs(10));
        if let Some(w) = w {
            req = req
                .bearer_auth(&w.token)
                .header("X-Worker-Id", w.id.to_string())
                .header("X-Lease-Epoch", epoch);
        }
        if let Some(key) = key {
            req = req.header("Idempotency-Key", key);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let response = req.send().await.unwrap();
        let actual = response.status().as_u16();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let value: Value = response.json().await.unwrap();
        assert_eq!(actual, status, "{path}: {value}");
        value
    }
    async fn register(&self, capacity: i32) -> Worker {
        let w = Worker {
            id: Uuid::now_v7(),
            token: format!("{}{}", Uuid::new_v4(), Uuid::new_v4()),
        };
        let bootstrap = Worker {
            id: w.id,
            token: BOOT.into(),
        };
        let body = json!({"build_id":"test-v1","supported_harnesses":["test"],"capacity":capacity,"worker_token":w.token});
        let first = self
            .request(
                Method::PUT,
                &format!("/internal/workers/{}", w.id),
                Some(&bootstrap),
                0,
                None,
                Some(body.clone()),
                200,
            )
            .await;
        let replay = self
            .request(
                Method::PUT,
                &format!("/internal/workers/{}", w.id),
                Some(&bootstrap),
                0,
                None,
                Some(body),
                200,
            )
            .await;
        assert_eq!(first, replay);
        assert!(first.get("worker_token").is_none());
        w
    }
    async fn create(&self) -> Value {
        self.request(Method::POST,&format!("/api/projects/{}/sessions",self.project),None,0,Some(&Uuid::new_v4().to_string()),Some(json!({"harness_id":"test","initial_run":{"input":user(),"expected_session_revision":0}})),201).await
    }
    async fn availability(&self, w: &Worker, seconds: u32) -> Value {
        self.request(
            Method::GET,
            &format!(
                "/internal/workers/{}/work-available?wait_seconds={seconds}",
                w.id
            ),
            Some(w),
            0,
            None,
            None,
            200,
        )
        .await
    }
    async fn claim(&self, w: &Worker, key: &str, limit: i32) -> Value {
        self.request(
            Method::POST,
            &format!("/internal/workers/{}/claims", w.id),
            Some(w),
            0,
            Some(key),
            Some(json!({"limit":limit})),
            200,
        )
        .await
    }
    async fn context(&self, w: &Worker, run: Uuid, epoch: i64) -> Value {
        self.request(
            Method::GET,
            &format!("/internal/runs/{run}/context"),
            Some(w),
            epoch,
            None,
            None,
            200,
        )
        .await
    }
    async fn commit(
        &self,
        w: &Worker,
        run: Uuid,
        epoch: i64,
        key: &str,
        body: Value,
        status: u16,
    ) -> Value {
        self.request(
            Method::POST,
            &format!("/internal/runs/{run}/commits"),
            Some(w),
            epoch,
            Some(key),
            Some(body),
            status,
        )
        .await
    }
    async fn expire(&self, run: Uuid) {
        sqlx::query(
            "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
        )
        .bind(run)
        .execute(&self.pool)
        .await
        .unwrap();
    }
    async fn inputs(&self, w: &Worker, run: Uuid, epoch: i64) -> Value {
        self.request(
            Method::GET,
            &format!("/internal/runs/{run}/inputs"),
            Some(w),
            epoch,
            None,
            None,
            200,
        )
        .await
    }
    async fn ack(&self, w: &Worker, run: Uuid, epoch: i64) -> Value {
        let context = self.context(w, run, epoch).await;
        let inputs = self.inputs(w, run, epoch).await;
        let mut body = base(&context);
        body["input_results"] = json!(
            inputs["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| json!({"id":i["id"],"status":"handled"}))
                .collect::<Vec<_>>()
        );
        self.commit(w, run, epoch, &Uuid::new_v4().to_string(), body, 200)
            .await
    }
    async fn child(
        &self,
        w: &Worker,
        parent: Uuid,
        epoch: i64,
        key: &str,
        fork: Option<i64>,
    ) -> Value {
        self.request(Method::POST,&format!("/internal/runs/{parent}/children"),Some(w),epoch,Some(key),Some(json!({"fork_at_revision":fork,"initial_run":{"input":user(),"expected_session_revision":fork.unwrap_or(0)}})),201).await
    }
}
fn id(v: &Value) -> Uuid {
    v.as_str().unwrap().parse().unwrap()
}
fn user() -> Value {
    json!({"role":"user","id":"native-user-id","timestamp":0,"content":[{"type":"text","content":"Work"}]})
}
fn assistant() -> Value {
    json!({"role":"assistant","id":"provider-response-id","timestamp":0,"model":{"provider":"test","id":"model"},"duration_ms":1,"native_message":{},"content":[{"type":"response","response":{"content":"Done"}}],"stop_reason":"stop"})
}
fn base(context: &Value) -> Value {
    json!({"expected_run_version":context["run"]["version"],"expected_session_revision":context["session"]["current_revision"]})
}
fn message(value: Value) -> Value {
    json!({"message_id":Uuid::now_v7(),"message":value})
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn resolved_wait_overrides_delayed_ready(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(1).await;
    let run = id(&app.create().await["run"]["id"]);
    app.claim(&w, "initial", 1).await;
    app.ack(&w, run, 1).await;
    for (index, wait) in [
        json!({"wait_key":"due-timer","mode":"any","dependencies":[{"kind":"timer","wake_at":"2000-01-01T00:00:00Z"}]}),
        json!({"wait_key":"due-deadline","mode":"all","deadline_at":"2000-01-01T00:00:00Z","dependencies":[{"kind":"input","input_kind":"approval_response"}]}),
    ].into_iter().enumerate() {
        let epoch = index as i64 + 1;
        let mut body = base(&app.context(&w, run, epoch).await);
        body["waits"] = json!([wait]);
        body["disposition"] = json!({"status":"ready","available_at":"2099-01-01T00:00:00Z"});
        app.commit(&w, run, epoch, &format!("commit-{index}"), body, 200).await;
        app.runtime.reconcile_once().await.unwrap();
        let claimed = app.claim(&w, &format!("claim-{index}"), 1).await;
        assert_eq!(claimed["items"].as_array().unwrap().len(), 1);
        assert_eq!(id(&claimed["items"][0]["id"]), run);
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn follow_up_fences_ownership_project_and_session_state(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(2).await;
    let source = id(&app.create().await["run"]["id"]);
    app.claim(&w, "source", 1).await;
    let child = app.child(&w, source, 1, "child", None).await;
    let session = id(&child["session"]["id"]);
    let path = format!("/internal/runs/{source}/follow-ups");
    let body =
        json!({"target_session_id":session,"run":{"input":user(),"expected_session_revision":0}});
    let busy = app
        .request(
            Method::POST,
            &path,
            Some(&w),
            1,
            Some("busy"),
            Some(body.clone()),
            409,
        )
        .await;
    assert_eq!(busy["error"]["code"], "SESSION_STATE_CONFLICT");
    let child_run = id(&child["run"]["id"]);
    app.claim(&w, "child-claim", 1).await;
    let mut finish = base(&app.context(&w, child_run, 1).await);
    finish["disposition"] = json!({"status":"failed","error":{"message":"test"}});
    app.commit(&w, child_run, 1, "finish", finish, 200).await;
    let mut stale_revision = body.clone();
    stale_revision["run"]["expected_session_revision"] = json!(1);
    app.request(
        Method::POST,
        &path,
        Some(&w),
        1,
        Some("revision"),
        Some(stale_revision),
        409,
    )
    .await;
    sqlx::query("update sessions set archived_at=clock_timestamp() where id=$1")
        .bind(session)
        .execute(&app.pool)
        .await
        .unwrap();
    app.request(
        Method::POST,
        &path,
        Some(&w),
        1,
        Some("archived"),
        Some(body.clone()),
        409,
    )
    .await;
    sqlx::query("update sessions set archived_at=null where id=$1")
        .bind(session)
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query("update harnesses set enabled=false where id='test'")
        .execute(&app.pool)
        .await
        .unwrap();
    app.request(
        Method::POST,
        &path,
        Some(&w),
        1,
        Some("disabled"),
        Some(body.clone()),
        409,
    )
    .await;
    sqlx::query("update harnesses set enabled=true where id='test'")
        .execute(&app.pool)
        .await
        .unwrap();
    let foreign_project = Uuid::now_v7();
    let foreign_session = Uuid::now_v7();
    sqlx::query("insert into projects(project_id,name) values($1,'Foreign')")
        .bind(foreign_project)
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into sessions (id,project_id,harness_id,config) values ($1,$2,'test','{}')",
    )
    .bind(foreign_session)
    .bind(foreign_project)
    .execute(&app.pool)
    .await
    .unwrap();
    let mut cross_project = body.clone();
    cross_project["target_session_id"] = json!(foreign_session);
    app.request(
        Method::POST,
        &path,
        Some(&w),
        1,
        Some("foreign"),
        Some(cross_project),
        404,
    )
    .await;
    app.expire(source).await;
    app.request(
        Method::POST,
        &path,
        Some(&w),
        1,
        Some("expired"),
        Some(body.clone()),
        409,
    )
    .await;
    app.claim(&w, "reclaim", 1).await;
    let (a, b) = tokio::join!(
        app.request(
            Method::POST,
            &path,
            Some(&w),
            2,
            Some("follow-up"),
            Some(body.clone()),
            201
        ),
        app.request(
            Method::POST,
            &path,
            Some(&w),
            2,
            Some("follow-up"),
            Some(body.clone()),
            201
        )
    );
    assert_eq!(a, b);
    assert_eq!(a["session"]["id"], child["session"]["id"]);
    assert_eq!(a["run"]["parent_run_id"], json!(source));
    app.expire(source).await;
    let replay = app
        .request(
            Method::POST,
            &path,
            Some(&w),
            2,
            Some("follow-up"),
            Some(body.clone()),
            201,
        )
        .await;
    assert_eq!(replay, a);
    app.request(
        Method::POST,
        &path,
        Some(&w),
        2,
        Some("new-after-expiry"),
        Some(body),
        409,
    )
    .await;
    let count: i64 = sqlx::query_scalar("select count(*) from runs where session_id=$1")
        .bind(session)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn resolved_wait_invalidates_stale_park(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(1).await;
    let run = id(&app.create().await["run"]["id"]);
    app.claim(&w, "claim", 1).await;
    app.ack(&w, run, 1).await;
    let mut body = base(&app.context(&w, run, 1).await);
    let soon = (chrono::Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();
    body["waits"] = json!([
        {"wait_key":"timer","mode":"any","dependencies":[{"kind":"timer","wake_at":soon}]},
        {"wait_key":"deadline","mode":"any","deadline_at":soon,"dependencies":[{"kind":"input","input_kind":"approval_response","correlation_key":"never"}]},
        {"wait_key":"indefinite","mode":"any","dependencies":[{"kind":"input","input_kind":"approval_response","correlation_key":"later"}]}
    ]);
    app.commit(&w, run, 1, "register", body, 200).await;
    let stale = app.context(&w, run, 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    app.runtime.reconcile_once().await.unwrap();
    let mut body = base(&stale);
    body["disposition"] = json!({"status":"waiting"});
    app.commit(&w, run, 1, "stale", body, 409).await;
    let fresh = app.context(&w, run, 1).await;
    assert_eq!(fresh["run"]["status"], "running");
    assert!(fresh["run"]["version"].as_i64().unwrap() > stale["run"]["version"].as_i64().unwrap());
    for (key, status) in [("timer", "satisfied"), ("deadline", "timed_out")] {
        assert!(
            fresh["waits"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|wait| wait["wait_key"] == key && wait["status"] == status)
        );
    }
    // Once observed, historical resolutions must not prevent a legitimate wait.
    let mut body = base(&fresh);
    body["disposition"] = json!({"status":"waiting"});
    assert_eq!(
        app.commit(&w, run, 1, "fresh", body, 200).await["run"]["status"],
        "waiting"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn event_line_breaks_are_rejected_atomically(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(1).await;
    let run = id(&app.create().await["run"]["id"]);
    app.claim(&w, "claim", 1).await;
    let before = app.context(&w, run, 1).await;
    for kind in ["tool\nprogress", "tool\rprogress", "tool\r\nprogress"] {
        let events = json!([{"type":"valid.progress","payload":{}},{"type":kind,"payload":{}}]);
        app.request(
            Method::POST,
            &format!("/internal/runs/{run}/events"),
            Some(&w),
            1,
            Some(&Uuid::new_v4().to_string()),
            Some(json!({"events":events})),
            400,
        )
        .await;
        let mut body = base(&before);
        body["events"] = events;
        body["messages"] = json!([message(assistant())]);
        app.commit(&w, run, 1, &Uuid::new_v4().to_string(), body, 400)
            .await;
    }
    let after = app.context(&w, run, 1).await;
    assert_eq!(before["run"], after["run"]);
    assert_eq!(before["session"], after["session"]);
    let count: i64 =
        sqlx::query_scalar("select count(*) from run_events where run_id=$1 and source='harness'")
            .bind(run)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    app.request(
        Method::POST,
        &format!("/internal/runs/{run}/events"),
        Some(&w),
        1,
        Some("valid"),
        Some(json!({"events":[{"type":"tool.progress","payload":{}}]})),
        201,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn wait_settlement_is_independent_of_delivery_order(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(2).await;
    for input_first in [false, true] {
        let run = id(&app.create().await["run"]["id"]);
        app.claim(&w, &Uuid::new_v4().to_string(), 1).await;
        app.ack(&w, run, 1).await;
        if input_first {
            app.request(Method::POST, &format!("/api/runs/{run}/inputs"), None, 0, Some("approval"), Some(json!({"kind":"approval_response","correlation_key":"approval","approved":true})), 201).await;
        }
        let mut body = base(&app.context(&w, run, 1).await);
        body["waits"] = json!([{"wait_key":"approval","mode":"all","dependencies":[{"kind":"timer","wake_at":"2000-01-01T00:00:00Z"},{"kind":"input","input_kind":"approval_response","correlation_key":"approval"}]}]);
        app.commit(&w, run, 1, "register", body, 200).await;
        if !input_first {
            app.request(Method::POST, &format!("/api/runs/{run}/inputs"), None, 0, Some("approval"), Some(json!({"kind":"approval_response","correlation_key":"approval","approved":true})), 201).await;
        }
        app.runtime.reconcile_once().await.unwrap();
        let ctx = app.context(&w, run, 1).await;
        let wait = &ctx["waits"]["items"][0];
        assert_eq!(wait["status"], "satisfied");
        let deps = wait["result"]["dependencies"].as_array().unwrap();
        assert_eq!(deps.len(), 2);
        let input = deps.iter().find(|dep| dep["kind"] == "input").unwrap();
        assert!(input["result"]["input_id"].is_string());
        assert_eq!(input["result"]["payload"]["approved"], true);
        let count: i64 = sqlx::query_scalar(
            "select count(*) from run_events where run_id=$1 and type='run.wait_resolved'",
        )
        .bind(run)
        .fetch_one(&app.pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn lifecycle_capacity_authentication_and_takeover(pool: PgPool) {
    let app = App::new(pool).await;
    let a = app.register(1).await;
    let b = app.register(1).await;
    let created = app.create().await;
    let run = id(&created["run"]["id"]);
    app.request(
        Method::GET,
        &format!("/internal/workers/{}/assignments", a.id),
        None,
        0,
        None,
        None,
        401,
    )
    .await;
    app.request(
        Method::GET,
        &format!("/internal/workers/{}/assignments", a.id),
        Some(&b),
        0,
        None,
        None,
        401,
    )
    .await;
    let (one, two) = tokio::join!(app.claim(&a, "race-a", 1), app.claim(&b, "race-b", 1));
    assert_eq!(
        one["items"].as_array().unwrap().len() + two["items"].as_array().unwrap().len(),
        1
    );
    let (owner, other, claim) = if one["items"].as_array().unwrap().is_empty() {
        (&b, &a, two)
    } else {
        (&a, &b, one)
    };
    let key = if owner.id == a.id { "race-a" } else { "race-b" };
    assert_eq!(app.claim(owner, key, 1).await, claim);
    app.create().await;
    assert!(
        app.claim(owner, "full", 1).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let beat=app.request(Method::POST,&format!("/internal/workers/{}/heartbeat",owner.id),Some(owner),0,None,Some(json!({"leases":[{"run_id":run,"lease_epoch":1},{"run_id":Uuid::new_v4(),"lease_epoch":1}]})),200).await;
    assert_eq!(beat["renewed"].as_array().unwrap().len(), 1);
    assert_eq!(beat["lost"].as_array().unwrap().len(), 1);
    let assignments = app
        .request(
            Method::GET,
            &format!("/internal/workers/{}/assignments", owner.id),
            Some(owner),
            0,
            None,
            None,
            200,
        )
        .await;
    assert_eq!(assignments["items"][0]["id"], json!(run));
    let stale_context = app.context(owner, run, 1).await;
    app.expire(run).await;
    app.request(
        Method::POST,
        &format!("/internal/workers/{}/heartbeat", owner.id),
        Some(owner),
        0,
        None,
        Some(json!({"leases":[{"run_id":run,"lease_epoch":1}]})),
        200,
    )
    .await;
    // Expired leases cannot be renewed. Make capacity sufficient to claim both.
    app.request(
        Method::PATCH,
        &format!("/internal/workers/{}", other.id),
        Some(other),
        0,
        None,
        Some(json!({"capacity":2})),
        200,
    )
    .await;
    let reclaimed = app.claim(other, "takeover", 2).await;
    assert!(
        reclaimed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == json!(run) && r["lease_epoch"] == 2)
    );
    app.commit(owner, run, 1, "stale", base(&stale_context), 409)
        .await;
    app.request(
        Method::GET,
        &format!("/internal/runs/{run}/context"),
        Some(owner),
        1,
        None,
        None,
        409,
    )
    .await;
    app.request(
        Method::PATCH,
        &format!("/internal/workers/{}", other.id),
        Some(other),
        0,
        None,
        Some(json!({"status":"draining"})),
        200,
    )
    .await;
    app.request(
        Method::POST,
        &format!("/internal/workers/{}/claims", other.id),
        Some(other),
        0,
        Some("drained"),
        Some(json!({"limit":1})),
        409,
    )
    .await;
    app.request(
        Method::PATCH,
        &format!("/internal/workers/{}", other.id),
        Some(other),
        0,
        None,
        Some(json!({"status":"offline"})),
        409,
    )
    .await;
    let mut release = base(&app.context(other, run, 2).await);
    release["disposition"] = json!({"status":"ready"});
    app.commit(other, run, 2, "release", release, 200).await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn atomic_commits_completion_replay_and_rollback(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(1).await;
    let created = app.create().await;
    let run = id(&created["run"]["id"]);
    app.claim(&w, "start", 1).await;
    let ctx = app.context(&w, run, 1).await;
    let input = created["input"]["id"].clone();
    let user_message = message(user());
    let mut first = base(&ctx);
    first["messages"] = json!([user_message]);
    first["input_results"] = json!([{"id":input,"status":"handled"}]);
    first["checkpoint"] = json!({"expected_version":0,"state":{"step":"model"}});
    let saved = app.commit(&w, run, 1, "first", first.clone(), 200).await;
    assert_eq!(saved["session_revision"], 1);
    assert_eq!(saved["checkpoint"]["version"], 1);
    assert_eq!(
        app.commit(&w, run, 1, "first", first.clone(), 200).await,
        saved
    );
    first["checkpoint"]["state"] = json!({"different":true});
    app.commit(&w, run, 1, "first", first, 409).await;
    let ctx = app.context(&w, run, 1).await;
    let mut bad = base(&ctx);
    bad["messages"] = json!([message(user())]);
    bad["checkpoint"] = json!({"expected_version":99,"state":{}});
    app.commit(&w, run, 1, "bad", bad, 409).await;
    assert_eq!(
        app.context(&w, run, 1).await["session"]["current_revision"],
        1
    );
    let final_message = message(assistant());
    let mut complete = base(&ctx);
    complete["messages"] = json!([final_message]);
    complete["disposition"] =
        json!({"status":"completed","final_message_id":final_message["message_id"]});
    let done = app.commit(&w, run, 1, "done", complete.clone(), 200).await;
    assert_eq!(done["run"]["status"], "completed");
    assert_eq!(app.commit(&w, run, 1, "done", complete, 200).await, done);
    assert!(
        app.request(
            Method::GET,
            &format!("/internal/workers/{}/assignments", w.id),
            Some(&w),
            0,
            None,
            None,
            200
        )
        .await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let events: Vec<String> =
        sqlx::query_scalar("select type from run_events where run_id=$1 order by sequence")
            .bind(run)
            .fetch_all(&app.pool)
            .await
            .unwrap();
    assert!(events.contains(&"run.completed".into()));
    app.request(
        Method::PATCH,
        &format!("/internal/workers/{}", w.id),
        Some(&w),
        0,
        None,
        Some(json!({"status":"offline"})),
        200,
    )
    .await;
    app.request(
        Method::PATCH,
        &format!("/internal/workers/{}", w.id),
        Some(&w),
        0,
        None,
        Some(json!({"status":"accepting"})),
        409,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn durable_waits_timer_deadline_and_input_races(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(1).await;
    let c = app.create().await;
    let run = id(&c["run"]["id"]);
    app.claim(&w, "start", 1).await;
    let mut body = base(&app.context(&w, run, 1).await);
    body["waits"] = json!([{"wait_key":"input-race","mode":"any","dependencies":[{"kind":"timer","wake_at":"2099-01-01T00:00:00Z"}]}]);
    body["disposition"] = json!({"status":"waiting"});
    // Initial input is still pending: park must return ready, not lose delivery.
    assert_eq!(
        app.commit(&w, run, 1, "race", body, 200).await["run"]["status"],
        "ready"
    );
    app.claim(&w, "resume", 1).await;
    app.ack(&w, run, 2).await;
    let mut body = base(&app.context(&w, run, 2).await);
    body["waits"] = json!([{"wait_key":"already-due","mode":"any","dependencies":[{"kind":"timer","wake_at":"2000-01-01T00:00:00Z"}]}]);
    body["disposition"] = json!({"status":"waiting"});
    assert_eq!(
        app.commit(&w, run, 2, "due", body, 200).await["run"]["status"],
        "ready"
    );
    app.claim(&w, "again", 1).await;
    let mut body = base(&app.context(&w, run, 3).await);
    let soon = (chrono::Utc::now() + chrono::Duration::milliseconds(500)).to_rfc3339();
    body["waits"] = json!([{"wait_key":"timeout","mode":"all","deadline_at":soon,"dependencies":[{"kind":"input","input_kind":"approval_response","correlation_key":"approval-1"}]}]);
    body["disposition"] = json!({"status":"waiting"});
    assert_eq!(
        app.commit(&w, run, 3, "wait", body, 200).await["run"]["status"],
        "waiting"
    );
    tokio::time::sleep(std::time::Duration::from_millis(550)).await;
    app.runtime.reconcile_once().await.unwrap();
    let status: String = sqlx::query_scalar("select status from runs where id=$1")
        .bind(run)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(status, "ready");
    let timeout: String =
        sqlx::query_scalar("select status from run_waits where run_id=$1 and wait_key='timeout'")
            .bind(run)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(timeout, "timed_out");
    app.claim(&w, "input-wait", 1).await;
    let mut body = base(&app.context(&w, run, 4).await);
    body["waits"] = json!([{"wait_key":"approval","mode":"any","dependencies":[{"kind":"input","input_kind":"approval_response","correlation_key":"approve"}]}]);
    body["disposition"] = json!({"status":"waiting"});
    app.commit(&w, run, 4, "approval-wait", body, 200).await;
    app.request(
        Method::POST,
        &format!("/api/runs/{run}/inputs"),
        None,
        0,
        Some("approve"),
        Some(json!({"kind":"approval_response","correlation_key":"approve","approved":true})),
        201,
    )
    .await;
    app.claim(&w, "approved", 1).await;
    let ctx = app.context(&w, run, 5).await;
    assert!(
        ctx["waits"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["wait_key"] == "approval" && w["status"] == "satisfied")
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn children_forks_messages_abort_and_completion_wake(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(3).await;
    let c = app.create().await;
    let parent = id(&c["run"]["id"]);
    app.claim(&w, "parent", 1).await;
    app.ack(&w, parent, 1).await;
    let mut body = base(&app.context(&w, parent, 1).await);
    body["messages"] = json!([message(user())]);
    app.commit(&w, parent, 1, "history", body, 200).await;
    let child = app.child(&w, parent, 1, "child", Some(1)).await;
    let target = id(&child["run"]["id"]);
    assert_eq!(child["run"]["parent_run_id"], json!(parent));
    assert_ne!(child["session"]["id"], c["session"]["id"]);
    // Reuse the exact body (including native message ID) on child retry.
    assert_eq!(app.child(&w, parent, 1, "child", Some(1)).await, child);
    app.claim(&w, "child-start", 1).await;
    let ctx = app.context(&w, target, 1).await;
    assert_eq!(ctx["messages"]["items"].as_array().unwrap().len(), 1);
    assert!(ctx["messages"]["items"][0]["run_id"].is_null());
    let path = format!("/internal/runs/{parent}/messages");
    let msg = json!({"target_run_id":target,"kind":"agent_message","payload":{"text":"hello"}});
    let sent = app
        .request(
            Method::POST,
            &path,
            Some(&w),
            1,
            Some("send"),
            Some(msg.clone()),
            201,
        )
        .await;
    assert_eq!(sent["input"]["source_run_id"], json!(parent));
    assert_eq!(
        app.request(
            Method::POST,
            &path,
            Some(&w),
            1,
            Some("send"),
            Some(msg),
            201
        )
        .await,
        sent
    );
    app.ack(&w, target, 1).await;
    let mut body = base(&app.context(&w, parent, 1).await);
    body["waits"] = json!([{"wait_key":"join","mode":"all","dependencies":[{"kind":"run_completion","target_run_id":target}]}]);
    body["disposition"] = json!({"status":"waiting"});
    app.commit(&w, parent, 1, "join", body, 200).await;
    let mut body = base(&app.context(&w, target, 1).await);
    body["disposition"] = json!({"status":"failed","error":{"message":"model unavailable"}});
    app.commit(&w, target, 1, "failed", body, 200).await;
    app.runtime.reconcile_once().await.unwrap();
    app.claim(&w, "parent-resume", 1).await;
    let ctx = app.context(&w, parent, 2).await;
    assert_eq!(ctx["waits"]["items"][0]["status"], "satisfied");
    assert_eq!(
        ctx["waits"]["items"][0]["dependencies"][0]["result"]["status"],
        "failed"
    );
    let child = app.child(&w, parent, 2, "abort-child", None).await;
    let target = id(&child["run"]["id"]);
    let aborted = app
        .request(
            Method::POST,
            &format!("/internal/runs/{parent}/abort-requests"),
            Some(&w),
            2,
            Some("abort-child"),
            Some(json!({"target_run_id":target,"reason":"done"})),
            202,
        )
        .await;
    assert_eq!(aborted["input"]["source_run_id"], json!(parent));
    assert_eq!(aborted["run"]["status"], "ready");
    app.claim(&w, "abort-start", 1).await;
    app.ack(&w, target, 1).await;
    let mut body = base(&app.context(&w, target, 1).await);
    body["disposition"] = json!({"status":"aborted"});
    assert_eq!(
        app.commit(&w, target, 1, "abort-ack", body, 200).await["run"]["status"],
        "aborted"
    );
    assert_eq!(app.context(&w, parent, 2).await["run"]["status"], "running");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn project_harness_policy_fences_children_followups_and_live_disabling(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(3).await;
    let created = app.create().await;
    let parent = id(&created["run"]["id"]);
    app.claim(&w, "parent", 1).await;
    app.ack(&w, parent, 1).await;
    let policy = format!("/api/projects/{}/harnesses/test", app.project);
    let rejected = app
        .request(
            Method::PUT,
            &policy,
            None,
            0,
            None,
            Some(json!({"enabled":false})),
            409,
        )
        .await;
    assert_eq!(rejected["error"]["code"], "PROJECT_HARNESS_ACTIVE_RUNS");
    let target = app
        .request(
            Method::POST,
            &format!("/api/projects/{}/sessions", app.project),
            None,
            0,
            Some("other-session"),
            Some(json!({"harness_id":"other"})),
            201,
        )
        .await;
    app.request(
        Method::PUT,
        &format!("/api/projects/{}/harnesses/other", app.project),
        None,
        0,
        None,
        Some(json!({"enabled":false})),
        200,
    )
    .await;
    let child_body =
        json!({"harness_id":"other","initial_run":{"input":user(),"expected_session_revision":0}});
    let rejected = app
        .request(
            Method::POST,
            &format!("/internal/runs/{parent}/children"),
            Some(&w),
            1,
            Some("disabled-child"),
            Some(child_body.clone()),
            409,
        )
        .await;
    assert_eq!(rejected["error"]["code"], "PROJECT_HARNESS_DISABLED");
    let rejected=app.request(Method::POST,&format!("/internal/runs/{parent}/follow-ups"),Some(&w),1,Some("disabled-followup"),Some(json!({"target_session_id":target["session"]["id"],"run":{"input":user(),"expected_session_revision":0}})),409).await;
    assert_eq!(rejected["error"]["code"], "PROJECT_HARNESS_DISABLED");
    app.request(
        Method::PUT,
        &format!("/api/projects/{}/harnesses/other", app.project),
        None,
        0,
        None,
        Some(json!({"enabled":true})),
        200,
    )
    .await;
    let child = app
        .request(
            Method::POST,
            &format!("/internal/runs/{parent}/children"),
            Some(&w),
            1,
            Some("disabled-child"),
            Some(child_body),
            201,
        )
        .await;
    let child_id = id(&child["run"]["id"]);
    let mut body = base(&app.context(&w, parent, 1).await);
    body["waits"] = json!([{"wait_key":"join","mode":"all","dependencies":[{"kind":"run_completion","target_run_id":child_id}]}]);
    body["disposition"] = json!({"status":"waiting"});
    app.commit(&w, parent, 1, "park", body, 200).await;
    let rejected = app
        .request(
            Method::PUT,
            &policy,
            None,
            0,
            None,
            Some(json!({"enabled":false})),
            409,
        )
        .await;
    assert_eq!(rejected["error"]["code"], "PROJECT_HARNESS_ACTIVE_RUNS");
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn events_scope_and_failed_completion_are_atomic(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(2).await;
    let a = app.create().await;
    let run = id(&a["run"]["id"]);
    app.claim(&w, "start", 2).await;
    let path = format!("/internal/runs/{run}/events");
    let event = json!({"events":[{"type":"model.delta","payload":{"text":"hello"}}]});
    let written = app
        .request(
            Method::POST,
            &path,
            Some(&w),
            1,
            Some("event"),
            Some(event.clone()),
            201,
        )
        .await;
    assert_eq!(
        written,
        app.request(
            Method::POST,
            &path,
            Some(&w),
            1,
            Some("event"),
            Some(event),
            201
        )
        .await
    );
    app.request(
        Method::POST,
        &path,
        Some(&w),
        1,
        Some("spoof"),
        Some(json!({"events":[{"type":"run.completed"}]})),
        400,
    )
    .await;
    app.request(
        Method::POST,
        &path,
        Some(&w),
        99,
        Some("fenced"),
        Some(json!({"events":[{"type":"delta"}]})),
        409,
    )
    .await;
    let other_project = Uuid::now_v7();
    sqlx::query("insert into projects(project_id,name) values($1,'Other')")
        .bind(other_project)
        .execute(&app.pool)
        .await
        .unwrap();
    app.request(
        Method::PUT,
        &format!("/api/projects/{other_project}/harnesses/test"),
        None,
        0,
        None,
        Some(json!({"enabled":true})),
        200,
    )
    .await;
    let other=app.request(Method::POST,&format!("/api/projects/{other_project}/sessions"),None,0,Some("other"),Some(json!({"harness_id":"test","initial_run":{"input":user(),"expected_session_revision":0}})),201).await;
    let target = id(&other["run"]["id"]);
    app.request(
        Method::POST,
        &format!("/internal/runs/{run}/messages"),
        Some(&w),
        1,
        Some("cross-project"),
        Some(json!({"target_run_id":target,"kind":"agent_message"})),
        404,
    )
    .await;
    app.request(
        Method::POST,
        &format!("/internal/runs/{run}/abort-requests"),
        Some(&w),
        1,
        Some("cross-project-abort"),
        Some(json!({"target_run_id":target})),
        404,
    )
    .await;
    app.ack(&w, run, 1).await;
    let ctx = app.context(&w, run, 1).await;
    let invalid = message(user());
    let mut body = base(&ctx);
    body["messages"] = json!([invalid]);
    body["checkpoint"] = json!({"expected_version":0,"state":{"bad":true}});
    body["disposition"] = json!({"status":"completed","final_message_id":invalid["message_id"]});
    app.commit(&w, run, 1, "invalid-final", body, 409).await;
    let ctx = app.context(&w, run, 1).await;
    assert!(ctx["checkpoint"].is_null());
    assert_eq!(ctx["session"]["current_revision"], 0);
    app.expire(run).await;
    app.runtime.reconcile_once().await.unwrap();
    let claims = app.claim(&w, "recovered", 2).await;
    assert!(
        claims["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == json!(run) && r["lease_epoch"] == 2)
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn operation_receipts_survive_replacement_worker_and_reject_unrelated_workers(pool: PgPool) {
    let app = App::new(pool).await;
    let a = app.register(1).await;
    let b = app.register(1).await;
    let unrelated = app.register(1).await;
    let c = app.create().await;
    let parent = id(&c["run"]["id"]);
    app.claim(&a, "start", 1).await;
    let checkpoint_request = json!({"expected_run_version":2,"expected_session_revision":0,"checkpoint":{"expected_version":0,"state":{"child_operation_key":"stable-child"}}});
    let saved = app
        .commit(&a, parent, 1, "save-plan", checkpoint_request.clone(), 200)
        .await;
    let child = app.child(&a, parent, 1, "stable-child", None).await;
    app.expire(parent).await;
    // The child was queued first; explicitly defer it to make takeover deterministic.
    sqlx::query("update runs set available_at=clock_timestamp()+interval '1 hour',version=version+1 where id=$1").bind(id(&child["run"]["id"])).execute(&app.pool).await.unwrap();
    assert_eq!(
        app.claim(&b, "takeover", 1).await["items"][0]["id"],
        json!(parent)
    );
    assert_eq!(
        app.commit(&b, parent, 2, "save-plan", checkpoint_request.clone(), 200)
            .await,
        saved
    );
    assert_eq!(app.child(&b, parent, 2, "stable-child", None).await, child);
    let count: i64 = sqlx::query_scalar("select count(*) from runs where parent_run_id=$1")
        .bind(parent)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    app.commit(&unrelated, parent, 2, "save-plan", checkpoint_request, 409)
        .await;
    let ctx = app.context(&b, parent, 2).await;
    assert_eq!(
        ctx["checkpoint"]["state"]["child_operation_key"],
        "stable-child"
    );
    app.request(
        Method::POST,
        &format!("/internal/runs/{parent}/children"),
        Some(&a),
        1,
        Some("new-stale-child"),
        Some(json!({"initial_run":{"input":user(),"expected_session_revision":0}})),
        409,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn same_worker_claims_and_same_version_commits_serialize(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(1).await;
    app.create().await;
    app.create().await;
    let (a, b) = tokio::join!(app.claim(&w, "a", 1), app.claim(&w, "b", 1));
    assert_eq!(
        a["items"].as_array().unwrap().len() + b["items"].as_array().unwrap().len(),
        1
    );
    let claim = if a["items"].as_array().unwrap().is_empty() {
        b
    } else {
        a
    };
    let run = id(&claim["items"][0]["id"]);
    let body = base(&app.context(&w, run, 1).await);
    let make = |key: &str| {
        app.client
            .post(format!("{}/internal/runs/{run}/commits", app.url))
            .bearer_auth(&w.token)
            .header("X-Worker-Id", w.id.to_string())
            .header("X-Lease-Epoch", "1")
            .header("Idempotency-Key", key)
            .json(&body)
            .send()
    };
    let (a, b) = tokio::join!(make("first"), make("second"));
    let mut statuses = [a.unwrap().status().as_u16(), b.unwrap().status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 409]);
    app.request(
        Method::POST,
        &format!("/internal/workers/{}/claims", w.id),
        Some(&w),
        0,
        Some("a"),
        Some(json!({"limit":2})),
        409,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn operation_waits_and_consumed_input_do_not_rewake_forever(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(2).await;
    let c = app.create().await;
    let parent = id(&c["run"]["id"]);
    app.claim(&w, "start", 1).await;
    app.ack(&w, parent, 1).await;
    let child = app.child(&w, parent, 1, "child", None).await;
    let target = id(&child["run"]["id"]);
    app.claim(&w, "child-start", 1).await;
    app.ack(&w, target, 1).await;
    let mut body = base(&app.context(&w, target, 1).await);
    body["waits"] = json!([{"wait_key":"next-user","mode":"all","dependencies":[{"kind":"input","input_kind":"user_message"},{"kind":"timer","wake_at":"2000-01-01T00:00:00Z"}]},{"wait_key":"operation","mode":"any","dependencies":[{"kind":"operation","correlation_key":"op-1"},{"kind":"timer","wake_at":"2099-01-01T00:00:00Z"}]}]);
    body["disposition"] = json!({"status":"waiting"});
    assert_eq!(
        app.commit(&w, target, 1, "park", body, 200).await["run"]["status"],
        "waiting"
    );
    app.runtime.reconcile_once().await.unwrap();
    let status: String = sqlx::query_scalar("select status from runs where id=$1")
        .bind(target)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(status, "waiting");
    app.request(Method::POST,&format!("/internal/runs/{parent}/messages"),Some(&w),1,Some("operation"),Some(json!({"target_run_id":target,"kind":"operation_result","payload":{"correlation_key":"op-1","result":{"ok":true}}})),201).await;
    app.claim(&w, "resume-op", 1).await;
    let ctx = app.context(&w, target, 2).await;
    let waits = ctx["waits"]["items"].as_array().unwrap();
    assert!(
        waits
            .iter()
            .any(|w| w["wait_key"] == "operation" && w["status"] == "satisfied")
    );
    assert!(
        waits
            .iter()
            .any(|w| w["wait_key"] == "next-user" && w["status"] == "pending")
    );
    let pending = waits.iter().find(|w| w["wait_key"] == "next-user").unwrap()["id"].clone();
    let mut body = base(&ctx);
    body["cancel_wait_ids"] = json!([pending]);
    app.commit(&w, target, 2, "cancel", body, 200).await;
    app.request(
        Method::POST,
        &format!("/internal/runs/{parent}/messages"),
        Some(&w),
        1,
        Some("spoof-abort"),
        Some(json!({"target_run_id":target,"kind":"abort"})),
        400,
    )
    .await;
    app.request(
        Method::POST,
        &format!("/internal/runs/{parent}/messages"),
        Some(&w),
        1,
        Some("missing-correlation"),
        Some(json!({"target_run_id":target,"kind":"operation_result"})),
        400,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn parent_abort_does_not_cascade_and_disabled_harness_work_recovers(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(2).await;
    let c = app.create().await;
    let parent = id(&c["run"]["id"]);
    app.claim(&w, "start", 1).await;
    let child = app.child(&w, parent, 1, "child", None).await;
    let target = id(&child["run"]["id"]);
    app.claim(&w, "child-start", 1).await;
    app.request(
        Method::POST,
        &format!("/api/runs/{parent}/abort"),
        None,
        0,
        Some("stop"),
        Some(json!({"reason":"stop parent"})),
        202,
    )
    .await;
    app.ack(&w, parent, 1).await;
    let mut body = base(&app.context(&w, parent, 1).await);
    body["disposition"] = json!({"status":"aborted"});
    app.commit(&w, parent, 1, "aborted", body, 200).await;
    let ctx = app.context(&w, target, 1).await;
    assert!(ctx["run"]["abort_requested_at"].is_null());
    sqlx::query("update harnesses set enabled=false where id='test'")
        .execute(&app.pool)
        .await
        .unwrap();
    app.expire(target).await;
    let claimed = app.claim(&w, "disabled-recovery", 1).await;
    assert_eq!(claimed["items"][0]["id"], json!(target));
    assert_eq!(claimed["items"][0]["lease_epoch"], 2);
    app.request(
        Method::POST,
        &format!("/internal/runs/{target}/children"),
        Some(&w),
        2,
        Some("disabled-child"),
        Some(json!({"initial_run":{"input":user(),"expected_session_revision":0}})),
        409,
    )
    .await;
    app.ack(&w, target, 2).await;
    let ctx = app.context(&w, target, 2).await;
    let mut body = base(&ctx);
    body["disposition"] = json!({"status":"ready","available_at":"2099-01-01T00:00:00Z"});
    app.commit(&w, target, 2, "delay", body, 200).await;
    assert!(
        app.claim(&w, "not-due", 1).await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    app.request(
        Method::POST,
        &format!("/api/runs/{target}/inputs"),
        None,
        0,
        Some("wake-delay"),
        Some(json!({"kind":"user_message","message":user()})),
        201,
    )
    .await;
    assert_eq!(
        app.claim(&w, "due-now", 1).await["items"][0]["id"],
        json!(target)
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn completion_rechecks_inbox_and_internal_validation_is_bounded(pool: PgPool) {
    let app = App::new(pool).await;
    let w = app.register(1).await;
    let c = app.create().await;
    let run = id(&c["run"]["id"]);
    app.claim(&w, "start", 1).await;
    app.ack(&w, run, 1).await;
    let final_message = message(assistant());
    let ctx = app.context(&w, run, 1).await;
    let mut body = base(&ctx);
    body["messages"] = json!([final_message]);
    body["disposition"] =
        json!({"status":"completed","final_message_id":final_message["message_id"]});
    let input = app
        .request(
            Method::POST,
            &format!("/api/runs/{run}/inputs"),
            None,
            0,
            Some("late-input"),
            Some(json!({"kind":"user_message","message":user()})),
            201,
        )
        .await;
    app.commit(&w, run, 1, "complete", body.clone(), 409).await;
    assert_eq!(
        app.context(&w, run, 1).await["session"]["current_revision"],
        0
    );
    body["input_results"] = json!([{"id":input["input"]["id"],"status":"rejected","handling":{"reason":"task already finished"}}]);
    app.commit(&w, run, 1, "complete", body, 200).await;
    let worker_path = format!("/internal/workers/{}/claims", w.id);
    app.request(
        Method::POST,
        &worker_path,
        Some(&w),
        0,
        None,
        Some(json!({"limit":1})),
        400,
    )
    .await;
    app.request(
        Method::POST,
        &worker_path,
        Some(&w),
        0,
        Some("zero"),
        Some(json!({"limit":0})),
        400,
    )
    .await;
    app.request(
        Method::POST,
        &worker_path,
        Some(&w),
        0,
        Some("unknown"),
        Some(json!({"limit":1,"extra":true})),
        422,
    )
    .await;
    app.request(
        Method::GET,
        &format!("/internal/runs/{run}/inputs?limit=201"),
        Some(&w),
        1,
        None,
        None,
        400,
    )
    .await;
    let large = json!({"events":[{"type":"delta","payload":{"text":"x".repeat(1024*1024)}}]});
    app.request(
        Method::POST,
        &format!("/internal/runs/{run}/events"),
        Some(&w),
        1,
        Some("large"),
        Some(large),
        413,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn background_reconciler_marks_idle_processes_offline_and_wakes_timers(pool: PgPool) {
    let app = App::new(pool).await;
    let idle = app.register(1).await;
    sqlx::query("update workers set started_at=clock_timestamp()-interval '5 minutes',last_seen_at=clock_timestamp()-interval '3 minutes' where id=$1").bind(idle.id).execute(&app.pool).await.unwrap();
    let w = app.register(1).await;
    let c = app.create().await;
    let run = id(&c["run"]["id"]);
    app.claim(&w, "start", 1).await;
    app.ack(&w, run, 1).await;
    let mut body = base(&app.context(&w, run, 1).await);
    body["waits"] = json!([{"wait_key":"timer","mode":"all","dependencies":[{"kind":"timer","wake_at":(chrono::Utc::now()+chrono::Duration::milliseconds(500)).to_rfc3339()}]}]);
    body["disposition"] = json!({"status":"waiting"});
    app.commit(&w, run, 1, "park", body, 200).await;
    let reconciler = app.runtime.spawn_reconciler();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status: String = sqlx::query_scalar("select status from runs where id=$1")
                .bind(run)
                .fetch_one(&app.pool)
                .await
                .unwrap();
            if status == "ready" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    reconciler.abort();
    result.unwrap();
    let status: String = sqlx::query_scalar("select status from workers where id=$1")
        .bind(idle.id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(status, "offline");
    app.request(
        Method::POST,
        &format!("/internal/workers/{}/heartbeat", idle.id),
        Some(&idle),
        0,
        None,
        Some(json!({"leases":[]})),
        409,
    )
    .await;
    assert_eq!(
        app.claim(&w, "resume", 1).await["items"][0]["lease_epoch"],
        2
    );
}

//! Administrative HTTP boundaries and replica-safe maintenance against real DBs.
use platform_server::runtime::{RuntimeService, admin_router, router, worker_router};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

const ADMIN: &str = "admin-test-credential-01234567890123456789";
const BOOT: &str = "bootstrap-test-credential-01234567890123456789";
const WORKER: &str = "worker-test-credential-01234567890123456789";

#[tokio::test]
async fn disabled_administration_fails_closed_without_database_access() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://localhost/unused_runtime_admin_test")
        .unwrap();
    let app = App::new(pool, "").await;
    app.request(
        Method::PUT,
        "/internal/harnesses/test",
        ADMIN,
        Some(json!({"name":"Test"})),
        401,
    )
    .await;
    app.request(
        Method::PATCH,
        "/internal/harnesses/test",
        ADMIN,
        Some(json!({"enabled":false})),
        401,
    )
    .await;
    app.request(Method::GET, "/internal/workers", ADMIN, None, 401)
        .await;
    app.request(
        Method::GET,
        &format!("/internal/workers/{}", Uuid::now_v7()),
        ADMIN,
        None,
        401,
    )
    .await;
}
struct App {
    pool: PgPool,
    runtime: RuntimeService,
    client: Client,
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for App {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl App {
    async fn new(pool: PgPool, token: &str) -> Self {
        let runtime = RuntimeService::new(pool.clone());
        let routes = router(runtime.clone())
            .merge(worker_router(runtime.clone(), BOOT))
            .merge(admin_router(runtime.clone(), token));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        Self {
            pool,
            runtime,
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            url,
            task,
        }
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<Value>,
        status: u16,
    ) -> Value {
        let mut request = self.client.request(method, format!("{}{path}", self.url));
        if !token.is_empty() {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let actual = response.status().as_u16();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let value: Value = response.json().await.unwrap();
        assert_eq!(actual, status, "{path}: {value}");
        value
    }
    async fn harness(&self) -> Value {
        self.request(
            Method::PUT,
            "/internal/harnesses/test",
            ADMIN,
            Some(json!({"name":"Test"})),
            200,
        )
        .await
    }
    async fn worker(&self) -> Uuid {
        let id = Uuid::now_v7();
        self.request(Method::PUT,&format!("/internal/workers/{id}"),BOOT,Some(json!({"build_id":"test-build","supported_harnesses":["test"],"capacity":2,"worker_token":WORKER})),200).await;
        id
    }
    async fn run(&self) -> (Uuid, Uuid) {
        let project = Uuid::now_v7();
        let session = Uuid::now_v7();
        let run = Uuid::now_v7();
        sqlx::query("insert into projects(project_id,name) values($1,'Admin tests')")
            .bind(project)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("insert into sessions(id,project_id,harness_id) values($1,$2,'test')")
            .bind(session)
            .bind(project)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("insert into runs(id,project_id,session_id) values($1,$2,$3)")
            .bind(run)
            .bind(project)
            .bind(session)
            .execute(&self.pool)
            .await
            .unwrap();
        (project, run)
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn harness_upsert_patch_validation_and_authentication(pool: PgPool) {
    let app = App::new(pool, ADMIN).await;
    for token in ["", BOOT, WORKER, "wrong"] {
        app.request(
            Method::PUT,
            "/internal/harnesses/test",
            token,
            Some(json!({"name":"Test"})),
            401,
        )
        .await;
        app.request(Method::GET, "/internal/workers", token, None, 401)
            .await;
    }
    let first = app.harness().await;
    assert_eq!(first["enabled"], true);
    let patched=app.request(Method::PATCH,"/internal/harnesses/test",ADMIN,Some(json!({"enabled":false,"description":"Example","default_config":{"keep":1,"remove":2},"config_schema":{"type":"object","required":["model"]}})),200).await;
    assert_eq!(patched["name"], "Test");
    assert_eq!(patched["enabled"], false);
    let patched = app
        .request(
            Method::PATCH,
            "/internal/harnesses/test",
            ADMIN,
            Some(json!({"description":null,"config_schema":null,"default_config":{"keep":3}})),
            200,
        )
        .await;
    assert_eq!(patched["default_config"], json!({"keep":3}));
    assert!(patched["config_schema"].is_null());
    assert!(patched["description"].is_null());
    assert_eq!(patched["enabled"], false);
    for body in [
        json!({}),
        json!({"id":"other"}),
        json!({"name":null}),
        json!({"enabled":null}),
        json!({"default_config":null}),
        json!({"name":" bad "}),
        json!({"config_schema":{"type":123}}),
        json!({"config_schema":{"$ref":"https://example.invalid/schema"}}),
        json!({"default_config":{"large":"x".repeat(65536)}}),
    ] {
        app.request(
            Method::PATCH,
            "/internal/harnesses/test",
            ADMIN,
            Some(body),
            400,
        )
        .await;
    }
    for id in ["Bad", "0bad", "a.b", "aé", &"a".repeat(129)] {
        app.request(
            Method::PUT,
            &format!("/internal/harnesses/{id}"),
            ADMIN,
            Some(json!({"name":"Test"})),
            400,
        )
        .await;
    }
    // Accepted shared spellings must also satisfy the database constraint.
    for id in ["a", "test_harness-09", &"a".repeat(128)] {
        app.request(
            Method::PUT,
            &format!("/internal/harnesses/{id}"),
            ADMIN,
            Some(json!({"name":"Valid spelling"})),
            200,
        )
        .await;
    }
    app.request(
        Method::PATCH,
        "/internal/harnesses/missing",
        ADMIN,
        Some(json!({"enabled":false})),
        404,
    )
    .await;
    let replaced = app.harness().await;
    assert_eq!(replaced["created_at"], first["created_at"]);
    assert_eq!(replaced["default_config"], json!({}));
    assert_eq!(replaced["enabled"], true);
    let response = app
        .client
        .get(format!("{}/internal/workers", app.url))
        .header("authorization", format!("Bearer {ADMIN}"))
        .header("authorization", format!("Bearer {ADMIN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let disabled = App::new(app.pool.clone(), "").await;
    disabled
        .request(Method::GET, "/internal/workers", ADMIN, None, 401)
        .await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn worker_routes_capacity_pagination_and_liveness(pool: PgPool) {
    let app = App::new(pool, ADMIN).await;
    app.harness().await;
    let worker = app.worker().await;
    let second = app.worker().await;
    let path = format!("/internal/workers/{worker}");
    app.request(Method::GET, &path, WORKER, None, 401).await;
    app.request(
        Method::PATCH,
        &path,
        ADMIN,
        Some(json!({"capacity":3})),
        401,
    )
    .await;
    let (_, run) = app.run().await;
    sqlx::query("update runs set status='running',available_at=null,worker_id=$2,lease_epoch=1,lease_expires_at=clock_timestamp()+interval '1 hour',started_at=clock_timestamp(),version=version+1 where id=$1").bind(run).bind(worker).execute(&app.pool).await.unwrap();
    let view = app.request(Method::GET, &path, ADMIN, None, 200).await;
    assert_eq!(view["active_assignments"], 1);
    assert_eq!(view["available_capacity"], 1);
    assert!(!view.to_string().contains(WORKER));
    assert!(view.get("token_hash").is_none());
    app.request(
        Method::PATCH,
        &path,
        WORKER,
        Some(json!({"status":"draining"})),
        200,
    )
    .await;
    let view = app.request(Method::GET, &path, ADMIN, None, 200).await;
    assert_eq!(view["available_capacity"], 0);
    let page = app
        .request(Method::GET, "/internal/workers?limit=1", ADMIN, None, 200)
        .await;
    let next = app
        .request(
            Method::GET,
            &format!(
                "/internal/workers?limit=1&cursor={}",
                page["next_cursor"].as_str().unwrap()
            ),
            ADMIN,
            None,
            200,
        )
        .await;
    assert_ne!(page["items"][0]["id"], next["items"][0]["id"]);
    assert!(next["next_cursor"].is_null());
    app.request(
        Method::GET,
        &format!(
            "/internal/workers?status=draining&cursor={}",
            page["next_cursor"].as_str().unwrap()
        ),
        ADMIN,
        None,
        400,
    )
    .await;
    let filtered = app
        .request(
            Method::GET,
            "/internal/workers?status=draining&harness_id=test",
            ADMIN,
            None,
            200,
        )
        .await;
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    for query in [
        "limit=0",
        "limit=201",
        "status=bad",
        "cursor=bad",
        "unknown=1",
    ] {
        app.request(
            Method::GET,
            &format!("/internal/workers?{query}"),
            ADMIN,
            None,
            400,
        )
        .await;
    }
    app.request(
        Method::GET,
        &format!("/internal/workers/{}", Uuid::now_v7()),
        ADMIN,
        None,
        404,
    )
    .await;
    app.request(Method::GET, "/internal/workers/bad-id", ADMIN, None, 400)
        .await;
    sqlx::query("update workers set started_at=clock_timestamp()-interval '5 minutes',last_seen_at=clock_timestamp()-interval '3 minutes' where id=$1").bind(worker).execute(&app.pool).await.unwrap();
    app.runtime.reconcile_once().await.unwrap();
    let view = app.request(Method::GET, &path, ADMIN, None, 200).await;
    assert_eq!(view["status"], "draining");
    assert_eq!(view["stale"], true); // A valid lease prevents the offline transition.
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(run)
    .execute(&app.pool)
    .await
    .unwrap();
    let other = RuntimeService::new(app.pool.clone());
    let (a, b) = tokio::join!(app.runtime.reconcile_once(), other.reconcile_once());
    a.unwrap();
    b.unwrap();
    let view = app.request(Method::GET, &path, ADMIN, None, 200).await;
    assert_eq!(view["status"], "offline");
    assert_eq!(view["active_assignments"], 0);
    let fresh = app
        .request(
            Method::GET,
            &format!("/internal/workers/{second}"),
            ADMIN,
            None,
            200,
        )
        .await;
    assert_eq!(fresh["status"], "accepting");
    let events: i64 = sqlx::query_scalar(
        "select count(*) from run_events where run_id=$1 and type='run.lease_expired'",
    )
    .bind(run)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn receipt_cleanup_is_bounded_skips_locks_and_preserves_durable_receipts(pool: PgPool) {
    let app = App::new(pool, ADMIN).await;
    app.harness().await;
    let worker = app.worker().await;
    let (project, run) = app.run().await;
    sqlx::query("insert into runtime_requests(project_id,scope_kind,scope_id,key,request_hash,result,created_at,expires_at) select $1,'project.test',$1,n::text,repeat('a',64),'{}',clock_timestamp()-interval '2 days',clock_timestamp()-interval '1 day' from generate_series(1,502) n").bind(project).execute(&app.pool).await.unwrap();
    sqlx::query("insert into runtime_requests(project_id,scope_kind,scope_id,key,request_hash,result,expires_at) values($1,'project.test',$1,'future',repeat('a',64),'{}',clock_timestamp()+interval '1 day'),($1,'project.test',$1,'permanent',repeat('a',64),'{}',null),($1,'run.test',$2,'run-receipt',repeat('a',64),'{}',null)").bind(project).bind(run).execute(&app.pool).await.unwrap();
    sqlx::query("insert into worker_claim_requests(worker_id,key,request_hash,result) values($1,'claim',repeat('a',64),'{}')").bind(worker).execute(&app.pool).await.unwrap();
    let mut lock = app.pool.begin().await.unwrap();
    sqlx::query("select key from runtime_requests where key='1' for update")
        .execute(&mut *lock)
        .await
        .unwrap();
    app.runtime.reconcile_once().await.unwrap();
    let left: i64 = sqlx::query_scalar(
        "select count(*) from runtime_requests where expires_at<clock_timestamp()",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(left, 2); // Maximum 500 per pass; locked receipt is untouched.
    let other = RuntimeService::new(app.pool.clone());
    let (a, b) = tokio::join!(app.runtime.reconcile_once(), other.reconcile_once());
    a.unwrap();
    b.unwrap();
    lock.rollback().await.unwrap();
    app.runtime.reconcile_once().await.unwrap();
    let left: i64 = sqlx::query_scalar("select count(*) from runtime_requests")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(left, 3);
    let claims: i64 = sqlx::query_scalar("select count(*) from worker_claim_requests")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(claims, 1);
}

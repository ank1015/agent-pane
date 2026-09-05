use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use platform_server::{
    projects::{
        bootstrap::{ProjectBootstrapService, router},
        environments::{EnvironmentGateway, EnvironmentService},
    },
    providers::{LlmGatewayClient, ProviderService},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(app: Router) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { url, task }
}
async fn app(pool: &PgPool, gateway: &Server) -> Server {
    let providers = ProviderService::new(
        LlmGatewayClient::new(
            gateway.url.parse().unwrap(),
            "test-admin",
            Duration::from_secs(2),
        )
        .unwrap(),
    );
    // Deliberately unreachable: bootstrap must not use execution gateway.
    let environments = EnvironmentService::new(
        pool.clone(),
        EnvironmentGateway::new(
            "http://127.0.0.1:1/".parse().unwrap(),
            "unused",
            Duration::from_secs(1),
        )
        .unwrap(),
    );
    serve(router(ProjectBootstrapService::new(
        pool.clone(),
        providers,
        environments,
    )))
    .await
}
async fn project(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("insert into projects(project_id,name) values($1,'Test')")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    id
}
async fn request(server: &Server, id: &str, status: u16) -> Value {
    let response = reqwest::get(format!("{}/api/projects/{id}/bootstrap", server.url))
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), status);
    assert_eq!(response.headers()["cache-control"], "no-store");
    response.json().await.unwrap()
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn bootstrap_returns_safe_catalog_accounts_and_project_scoped_environments(pool: PgPool) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let gateway = serve(Router::new().route("/v1/admin/accounts", get(move |headers: HeaderMap| {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(headers["authorization"], "Bearer test-admin");
            Json(json!({"accounts":(["active","disabled","reauth_required"].iter().map(|status| json!({
                "id":Uuid::now_v7(),"name":status,"provider":"openai","status":status,
                "is_default":false,"created_at":"2026-09-05T00:00:00Z","updated_at":"2026-09-05T00:00:00Z",
                "runtime_revision":1,"config":{"secret":"secret-marker"},"credential":{"version":1,"encryption_key_version":1,"updated_at":"2026-09-05T00:00:00Z"}
            })).collect::<Vec<_>>())}))
        }
    }))).await;
    let app = app(&pool, &gateway).await;
    request(&app, "invalid", 400).await;
    request(&app, &Uuid::now_v7().to_string(), 404).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let id = project(&pool).await;
    let other = project(&pool).await;
    for (owner, name) in [(id, "Desktop test"), (other, "Other project secret")] {
        sqlx::query("insert into project_environments(id,project_id,name,type,machine_id,workspace_root,workspace_root_path,path) values($1,$2,$3,'machine',$4,'work','/work','test')")
            .bind(Uuid::now_v7()).bind(owner).bind(name).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    }
    sqlx::query("insert into harnesses(id,name,enabled) values('hidden','Hidden',false)")
        .execute(&pool)
        .await
        .unwrap();
    let body = request(&app, &id.to_string(), 200).await;
    assert_eq!(body.as_object().unwrap().len(), 3);
    assert_eq!(body["provider_accounts"].as_array().unwrap().len(), 3);
    assert_eq!(body["project_environments"].as_array().unwrap().len(), 1);
    assert_eq!(body["project_environments"][0]["name"], "Desktop test");
    assert_eq!(
        body["project_environments"][0]["workspace_root_path"],
        "/work"
    );
    let harness = body["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"] == "environments")
        .unwrap();
    assert!(
        !harness["supported_models"]["openai"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(harness["config_schema"].is_object());
    assert!(harness["default_config"].is_object());
    for forbidden in [
        "secret-marker",
        "Other project secret",
        "\"credential\"",
        "\"hidden\"",
    ] {
        assert!(!body.to_string().contains(forbidden));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn bootstrap_empty_and_gateway_failure_are_explicit(pool: PgPool) {
    let empty = serve(Router::new().route(
        "/v1/admin/accounts",
        get(|| async { Json(json!({"accounts":[]})) }),
    ))
    .await;
    sqlx::query("update harnesses set enabled=false")
        .execute(&pool)
        .await
        .unwrap();
    let id = project(&pool).await;
    let server = app(&pool, &empty).await;
    assert_eq!(
        request(&server, &id.to_string(), 200).await,
        json!({"harnesses":[],"provider_accounts":[],"project_environments":[]})
    );
    let failing = serve(Router::new().route(
        "/v1/admin/accounts",
        get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "secret-marker") }),
    ))
    .await;
    let server = app(&pool, &failing).await;
    let body = request(&server, &id.to_string(), 502).await;
    assert!(body.get("error").is_some());
    assert!(body.get("harnesses").is_none());
    assert!(!body.to_string().contains("secret-marker"));
}

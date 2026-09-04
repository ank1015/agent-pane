use axum::{
    Json, Router,
    extract::Path,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use platform_server::projects::environments::{
    self, CreateEnvironment, EnvironmentGateway, EnvironmentService, UpdateEnvironment,
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
use tokio::{net::TcpListener, task::JoinHandle};
use uuid::Uuid;

struct Server {
    url: String,
    task: JoinHandle<()>,
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; runs in an isolated SQLx database"]
async fn gateway_failures_are_sanitized_without_writes(pool: PgPool) {
    use axum::response::IntoResponse;
    let project = Uuid::new_v4();
    let target = Uuid::new_v4();
    sqlx::query("insert into projects (project_id,name) values ($1,'Test')")
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
    let client = reqwest::Client::new();
    for (status, body, kind, expected) in [
        (404, "secret-marker".into(), "machine", 422),
        (401, "secret-marker".into(), "machine", 502),
        (500, "secret-marker".into(), "machine", 502),
        (302, "secret-marker".into(), "machine", 502),
        (200, "malformed secret-marker".into(), "machine", 502),
        (200, "x".repeat(1024 * 1024 + 1), "machine", 502),
        (200, json!({"id":target,"kind":"e2b","desired_state":"ready","deleted_at":null}).to_string(), "machine", 422),
        (200, json!({"id":target,"kind":"registered","desired_state":"ready","deleted_at":null,"descriptor":null}).to_string(), "machine", 422),
        (200, json!({"id":target,"state":"failed","desired_state":"ready","deleted_at":null}).to_string(), "sandbox", 422),
        (200, json!({"id":target,"state":"ready","desired_state":"deleted","deleted_at":null}).to_string(), "sandbox", 422),
    ] {
        let calls = Arc::new(AtomicUsize::new(0)); let counter = calls.clone();
        let gateway = serve(Router::new().fallback(move || {
            counter.fetch_add(1, Ordering::SeqCst); let body = body.clone();
            async move { (StatusCode::from_u16(status).unwrap(), [("location", "/redirected")], body).into_response() }
        })).await;
        let app = serve(environments::router(EnvironmentService::new(pool.clone(), EnvironmentGateway::new(gateway.url.parse().unwrap(), "mock-token", Duration::from_secs(1)).unwrap()))).await;
        let result = client.post(format!("{}/api/projects/{project}/environments", app.url)).json(&payload(kind, target)).send().await.unwrap();
        assert_eq!(result.status().as_u16(), expected);
        assert_eq!(result.headers()["cache-control"], "no-store");
        assert!(!result.text().await.unwrap().contains("secret-marker"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    let count: i64 = sqlx::query_scalar("select count(*) from project_environments")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; runs in an isolated SQLx database"]
async fn concurrent_patches_do_not_lose_updates(pool: PgPool) {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    let target = Uuid::new_v4();
    sqlx::query("insert into projects (project_id,name) values ($1,'Test')")
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into project_environments (id,project_id,name,type,snapshot_id,workspace_root,path) values ($1,$2,'Test','sandbox',$3,'workspace','.')")
        .bind(id).bind(project).bind(target).execute(&pool).await.unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let gateway = serve(Router::new().fallback(move || {
        let barrier = barrier.clone();
        async move {
            barrier.wait().await;
            Json(json!({"id":target,"state":"ready","desired_state":"ready","deleted_at":null}))
        }
    }))
    .await;
    let app = serve(environments::router(EnvironmentService::new(
        pool.clone(),
        EnvironmentGateway::new(
            gateway.url.parse().unwrap(),
            "mock-token",
            Duration::from_secs(2),
        )
        .unwrap(),
    )))
    .await;
    let client = reqwest::Client::new();
    let url = format!("{}/api/projects/{project}/environments/{id}", app.url);
    let (a, b) = tokio::join!(
        client.patch(&url).json(&json!({"path":"a"})).send(),
        client.patch(&url).json(&json!({"path":"b"})).send()
    );
    let mut statuses = [a.unwrap().status().as_u16(), b.unwrap().status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 409]);
    let changed = sqlx::query(
        "update project_environments set type='machine',machine_id=$2,snapshot_id=null where id=$1",
    )
    .bind(id)
    .bind(target)
    .execute(&pool)
    .await;
    assert!(
        changed
            .unwrap_err()
            .as_database_error()
            .unwrap()
            .is_check_violation()
    );
    sqlx::query("delete from projects where project_id=$1")
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from project_environments")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "project deletion cascades to records only");
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(app: Router) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    Server {
        url,
        task: tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    }
}
fn payload(kind: &str, target: Uuid) -> Value {
    json!({"name":"Development", "type":kind, "machine_id":if kind == "machine" {Some(target)} else {None},
        "snapshot_id":if kind == "sandbox" {Some(target)} else {None}, "workspace_root":"workspace", "path":"repo"})
}

#[test]
fn validates_targets_paths_and_patch_semantics() {
    let id = Uuid::new_v4();
    for kind in ["machine", "sandbox"] {
        for path in [".", "repo", "repo/src", "some directory/repo"] {
            let mut value = payload(kind, id);
            value["path"] = json!(path);
            assert!(
                serde_json::from_value::<CreateEnvironment>(value)
                    .unwrap()
                    .validate()
                    .is_ok()
            );
        }
        for path in [
            "",
            "/absolute",
            "../escape",
            "repo/../escape",
            "repo//dir",
            "C:/repo",
            "repo\\src",
            "repo/",
            "repo/./src",
        ] {
            let mut value = payload(kind, id);
            value["path"] = json!(path);
            assert!(
                serde_json::from_value::<CreateEnvironment>(value)
                    .unwrap()
                    .validate()
                    .is_err(),
                "{path}"
            );
        }
    }
    for (field, value) in [
        ("machine_id", Value::Null),
        ("snapshot_id", json!(id)),
        ("name", json!(" ")),
        ("workspace_root", json!("")),
    ] {
        let mut input = payload("machine", id);
        input[field] = value;
        assert!(
            serde_json::from_value::<CreateEnvironment>(input)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    for patch in [
        json!({"type":"sandbox"}),
        json!({"project_id":id}),
        json!({"name":null}),
        json!({"path":null}),
        json!({"extra":true}),
    ] {
        assert!(serde_json::from_value::<UpdateEnvironment>(patch).is_err());
    }
    let patch: UpdateEnvironment = serde_json::from_value(json!({"snapshot_id":null})).unwrap();
    assert_eq!(patch.snapshot_id, Some(None));
    assert_eq!(patch.machine_id, None);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL for a local PostgreSQL role with CREATEDB; SQLx creates an isolated test database"]
async fn crud_scoping_validation_and_database_constraints(pool: PgPool) {
    let calls = Arc::new(AtomicUsize::new(0));
    let host_calls = calls.clone();
    let snapshot_calls = calls.clone();
    let gateway = serve(Router::new()
        .route("/v1/hosts/{id}", get(move |Path(id): Path<Uuid>, headers: HeaderMap| {
            host_calls.fetch_add(1, Ordering::SeqCst);
            async move {
                assert_eq!(headers["authorization"], "Bearer mock-control-token");
                Json(json!({"id":id,"kind":"registered","desired_state":"ready","state":"disconnected","deleted_at":null,
                    "descriptor":{"roots":[{"id":"other","native_path":"C:\\workspace"},{"id":"workspace","native_path":"/Users/test/workspace"}]}}))
            }
        }))
        .route("/v1/snapshots/{id}", get(move |Path(id): Path<Uuid>| {
            snapshot_calls.fetch_add(1, Ordering::SeqCst);
            async move { Json(json!({"id":id,"state":"ready","desired_state":"ready","deleted_at":null})) }
        }))).await;
    let app = serve(environments::router(EnvironmentService::new(
        pool.clone(),
        EnvironmentGateway::new(
            gateway.url.parse().unwrap(),
            "mock-control-token",
            Duration::from_secs(1),
        )
        .unwrap(),
    )))
    .await;
    let client = reqwest::Client::new();
    let project = Uuid::new_v4();
    let other = Uuid::new_v4();
    for id in [project, other] {
        sqlx::query("insert into projects (project_id,name) values ($1,'Test project')")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let base = format!("{}/api/projects/{project}/environments", app.url);
    assert_eq!(
        client
            .get(&base)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        json!([])
    );
    let missing = format!("{}/api/projects/{}/environments", app.url, Uuid::new_v4());
    assert_eq!(
        client.get(missing).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .get(format!("{}/api/projects/invalid/environments", app.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    for kind in ["machine", "sandbox"] {
        let target = Uuid::new_v4();
        let response = client
            .post(&base)
            .json(&payload(kind, target))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let created: Value = response.json().await.unwrap();
        assert_eq!(created["type"], kind);
        assert_eq!(created["project_id"], project.to_string());
        assert_eq!(
            created["workspace_root_path"],
            if kind == "machine" {
                json!("/Users/test/workspace")
            } else {
                Value::Null
            }
        );
        let id = created["id"].as_str().unwrap();
        let url = format!("{base}/{id}");
        let scoped = format!("{}/api/projects/{other}/environments/{id}", app.url);
        assert_eq!(
            client
                .get(&url)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap(),
            created
        );
        assert_eq!(
            client.get(&scoped).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .patch(&scoped)
                .json(&json!({"name":"Other"}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client.delete(&scoped).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );

        if kind == "machine" {
            // Simulate a pre-migration record; the same root ID can resolve its missing path.
            sqlx::query("update project_environments set workspace_root_path=null where id=$1")
                .bind(Uuid::parse_str(id).unwrap())
                .execute(&pool)
                .await
                .unwrap();
            for (root, native_path) in [
                ("workspace", "/Users/test/workspace"),
                ("other", "C:\\workspace"),
                ("workspace", "/Users/test/workspace"),
            ] {
                let response = client
                    .patch(&url)
                    .json(&json!({"workspace_root":root}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let refreshed: Value = response.json().await.unwrap();
                assert_eq!(refreshed["workspace_root"], root);
                assert_eq!(refreshed["workspace_root_path"], native_path);
            }
            let spoofed = client
                .patch(&url)
                .json(&json!({"workspace_root_path":"/forged"}))
                .send()
                .await
                .unwrap();
            assert_eq!(spoofed.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
        let before_calls = calls.load(Ordering::SeqCst);
        let renamed = client
            .patch(&url)
            .json(&json!({"name":"Renamed"}))
            .send()
            .await
            .unwrap();
        assert_eq!(renamed.status(), StatusCode::OK);
        let renamed: Value = renamed.json().await.unwrap();
        assert_eq!(renamed["name"], "Renamed");
        assert_eq!(renamed["created_at"], created["created_at"]);
        assert_eq!(
            renamed["workspace_root_path"],
            created["workspace_root_path"]
        );
        assert_ne!(renamed["updated_at"], created["updated_at"]);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            before_calls,
            "rename must not depend on gateway availability"
        );
        for bad in [
            json!({}),
            json!({"path":"../escape"}),
            json!({"machine_id":null,"snapshot_id":null}),
        ] {
            assert_eq!(
                client.patch(&url).json(&bad).send().await.unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            client
                .patch(&url)
                .json(&json!({"type":"machine"}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let moved = client
            .patch(&url)
            .json(&json!({"path":"another/repo"}))
            .send()
            .await
            .unwrap();
        assert_eq!(moved.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), before_calls + 1);
        assert_eq!(
            client.delete(&url).send().await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            before_calls + 1,
            "delete must not contact gateway"
        );
    }

    let mut invalid_root = payload("machine", Uuid::new_v4());
    invalid_root["workspace_root"] = json!("unknown");
    assert_eq!(
        client
            .post(&base)
            .json(&invalid_root)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let oversized = "x".repeat(17 * 1024);
    assert_eq!(
        client
            .post(&base)
            .header("content-type", "application/json")
            .body(oversized)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let invalid = sqlx::query("insert into project_environments (id,project_id,name,type,workspace_root,path) values ($1,$2,'Bad','machine','workspace','.')")
        .bind(Uuid::new_v4()).bind(project).execute(&pool).await;
    assert!(
        invalid
            .unwrap_err()
            .as_database_error()
            .unwrap()
            .is_check_violation()
    );
    assert_eq!(
        client
            .get(&base)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        json!([])
    );
}

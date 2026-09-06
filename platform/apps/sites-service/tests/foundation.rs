use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use platform_sites_service::{DesiredStatus, Site, SiteService, Status};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

const TOKEN: &str = "test-only-sites-service-token-0123456789";

struct Server {
    child: Child,
    base: String,
    client: Client,
}

impl Server {
    async fn start(root: &Path) -> Self {
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let child = command(root)
            .env("SITES_BIND_ADDRESS", address.to_string())
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            base: format!("http://{address}"),
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "service exited before becoming ready"
            );
            if let Ok(response) = server
                .client
                .get(format!("{}/readyz", server.base))
                .bearer_auth(TOKEN)
                .send()
                .await
            {
                if response.status() == StatusCode::OK {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "service failed to become ready");
            sleep(Duration::from_millis(25)).await;
        }
        server
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(TOKEN);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(response.headers().contains_key("x-request-id"));
        (status, response.json().await.unwrap())
    }

    async fn provision(&self, site: Uuid, project: Uuid) {
        let (status, _) = self
            .call(
                Method::PUT,
                &route(site),
                Some(json!({"project_id":project})),
            )
            .await;
        assert!(matches!(status, StatusCode::OK | StatusCode::ACCEPTED));
    }

    async fn wait_status(&self, site: Uuid, expected: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (status, body) = self.call(Method::GET, &route(site), None).await;
            assert_eq!(status, StatusCode::OK);
            if body["status"] == expected {
                return body;
            }
            assert!(Instant::now() < deadline, "unexpected site state: {body}");
            sleep(Duration::from_millis(25)).await;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Deliberately test abrupt process termination, not just pool.close().
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_platform-sites-service"));
    command
        .env("SITES_DATA_DIR", root)
        .env("SITES_API_TOKEN", TOKEN)
        .env("RUST_LOG", "error")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    command
}

fn route(site: Uuid) -> String {
    format!("/internal/sites/{site}")
}
fn database(root: &Path, site: Uuid) -> PathBuf {
    root.join("sites")
        .join(site.to_string())
        .join("data/site.sqlite")
}
async fn connect(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .busy_timeout(Duration::from_secs(5)),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn two_sites_persist_across_process_restart_and_lifecycle_replays() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let project = Uuid::new_v4();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    server.provision(a, project).await;
    server.provision(b, project).await;
    let original = server.wait_status(a, "ready").await;
    server.wait_status(b, "ready").await;

    let mut db = connect(&database(root.path(), a)).await;
    sqlx::query("CREATE TABLE results (score INTEGER NOT NULL)")
        .execute(&mut db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO results VALUES (42)")
        .execute(&mut db)
        .await
        .unwrap();
    db.close().await.unwrap();
    let mut db = connect(&database(root.path(), b)).await;
    let absent: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE name='results'")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(absent, 0);
    db.close().await.unwrap();

    let (status, suspended) = server
        .call(
            Method::PATCH,
            &route(a),
            Some(json!({"status":"suspended"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(suspended["status"], "suspended");
    server.provision(a, project).await;
    let replay = server.wait_status(a, "suspended").await;
    assert_eq!(replay["created_at"], original["created_at"]);
    let (_, repeated) = server
        .call(
            Method::PATCH,
            &route(a),
            Some(json!({"status":"suspended"})),
        )
        .await;
    assert_eq!(repeated["updated_at"], suspended["updated_at"]);
    let (status, conflict) = server
        .call(
            Method::PUT,
            &route(a),
            Some(json!({"project_id":Uuid::new_v4()})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "SITE_PROJECT_CONFLICT");

    drop(server);
    let server = Server::start(root.path()).await;
    server.wait_status(a, "suspended").await;
    server.wait_status(b, "ready").await;
    let mut db = connect(&database(root.path(), a)).await;
    let score: i64 = sqlx::query_scalar("SELECT score FROM results")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(score, 42);
    db.close().await.unwrap();
    let (status, resumed) = server
        .call(Method::PATCH, &route(a), Some(json!({"status":"ready"})))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resumed["status"], "ready");
}

#[tokio::test]
async fn api_authentication_validation_and_health_are_bounded_and_sanitized() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    for path in ["/healthz", "/readyz", "/internal/sites/bad-id", "/unknown"] {
        let response = server
            .client
            .get(format!("{}{path}", server.base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let response = server
        .client
        .get(format!("{}/healthz", server.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = server
        .client
        .get(format!("{}/healthz", server.base))
        .bearer_auth("incorrect")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let path = route(Uuid::new_v4());
    for (method, target, body, expected) in [
        (
            Method::GET,
            "/internal/sites/bad-id",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (Method::GET, path.as_str(), None, StatusCode::NOT_FOUND),
        (
            Method::GET,
            "/healthz?unexpected=1",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            path.as_str(),
            None,
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            Method::PUT,
            path.as_str(),
            Some(json!({"project_id":"not-a-uuid"})),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            Method::PUT,
            path.as_str(),
            Some(json!({"project_id":Uuid::new_v4(),"path":"/tmp"})),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            Method::PATCH,
            path.as_str(),
            Some(json!({"status":"failed"})),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            Method::PATCH,
            path.as_str(),
            Some(json!({})),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            Method::PATCH,
            path.as_str(),
            Some(json!({"status":"ready"})),
            StatusCode::NOT_FOUND,
        ),
        (
            Method::PUT,
            path.as_str(),
            Some(json!({"project_id":"x".repeat(17000)})),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ] {
        let (status, body) = server.call(method, target, body).await;
        assert_eq!(status, expected, "{target}: {body}");
        assert!(body["error"]["code"].is_string());
    }
    // Readiness validates storage, whereas liveness remains a process check.
    fs::rename(root.path().join("sites"), root.path().join("sites-away")).unwrap();
    let (status, body) = server.call(Method::GET, "/readyz", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body.to_string().contains(root.path().to_str().unwrap()));
    assert_eq!(
        server.call(Method::GET, "/healthz", None).await.0,
        StatusCode::OK
    );
    fs::rename(root.path().join("sites-away"), root.path().join("sites")).unwrap();
    assert_eq!(
        server.call(Method::GET, "/readyz", None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn concurrent_provisioning_has_one_immutable_binding() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let site = Uuid::new_v4();
    let project = Uuid::new_v4();
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let client = server.client.clone();
        let url = format!("{}{}", server.base, route(site));
        requests.spawn(async move {
            let response = client
                .put(url)
                .bearer_auth(TOKEN)
                .json(&json!({"project_id":project}))
                .send()
                .await
                .unwrap();
            assert!(matches!(
                response.status(),
                StatusCode::OK | StatusCode::ACCEPTED
            ));
            response.json::<Site>().await.unwrap()
        });
    }
    let mut created_at = None;
    while let Some(result) = requests.join_next().await {
        let site = result.unwrap();
        assert_eq!(site.project_id, project.to_string());
        if let Some(ref timestamp) = created_at {
            assert_eq!(&site.created_at, timestamp);
        }
        created_at = Some(site.created_at);
    }
    server.wait_status(site, "ready").await;
    let mut metadata = connect(&root.path().join("service.sqlite")).await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sites")
        .fetch_one(&mut metadata)
        .await
        .unwrap();
    assert_eq!(count, 1);
    metadata.close().await.unwrap();
}

#[tokio::test]
async fn startup_recovers_each_provisioning_boundary_and_preserves_existing_data() {
    let root = TempDir::new().unwrap();
    let project = Uuid::new_v4();
    let ids: Vec<_> = (0..4).map(|_| Uuid::new_v4()).collect();
    let service = SiteService::open(root.path().to_path_buf()).await.unwrap();
    for id in &ids {
        assert_eq!(
            service.provision(*id, project).await.unwrap().status,
            Status::Provisioning
        );
    }
    // Last site represents a crash after SQLite initialization but before the
    // service's ready transition. Preserve an application table through recovery.
    service
        .set_status(ids[3], DesiredStatus::Suspended)
        .await
        .unwrap();
    service.close().await;
    drop(service);
    fs::create_dir_all(database(root.path(), ids[1]).parent().unwrap()).unwrap();
    fs::create_dir_all(database(root.path(), ids[2]).parent().unwrap()).unwrap();
    fs::File::create(database(root.path(), ids[2])).unwrap();
    let mut metadata = connect(&root.path().join("service.sqlite")).await;
    sqlx::query("UPDATE sites SET storage_initialized=0,status='provisioning' WHERE id=?")
        .bind(ids[3].to_string())
        .execute(&mut metadata)
        .await
        .unwrap();
    metadata.close().await.unwrap();
    let mut db = connect(&database(root.path(), ids[3])).await;
    sqlx::query("CREATE TABLE saved (value TEXT)")
        .execute(&mut db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO saved VALUES ('keep')")
        .execute(&mut db)
        .await
        .unwrap();
    db.close().await.unwrap();

    let service = SiteService::open(root.path().to_path_buf()).await.unwrap();
    for id in &ids[..3] {
        assert_eq!(service.get(*id).await.unwrap().status, Status::Ready);
    }
    assert_eq!(service.get(ids[3]).await.unwrap().status, Status::Suspended);
    let mut db = connect(&database(root.path(), ids[3])).await;
    let value: String = sqlx::query_scalar("SELECT value FROM saved")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(value, "keep");
    db.close().await.unwrap();
    service.close().await;
}

#[tokio::test]
async fn failed_provisioning_retries_but_initialized_storage_is_never_recreated() {
    let root = TempDir::new().unwrap();
    let service = SiteService::open(root.path().to_path_buf()).await.unwrap();
    let site = Uuid::new_v4();
    let project = Uuid::new_v4();
    service.provision(site, project).await.unwrap();
    let site_dir = root.path().join("sites").join(site.to_string());
    fs::write(&site_dir, "blocks directory creation").unwrap();
    service.reconcile_all().await.unwrap();
    let failed = service.get(site).await.unwrap();
    assert_eq!(failed.status, Status::Failed);
    assert_eq!(
        failed.error_code.as_deref(),
        Some("SITE_PROVISIONING_FAILED")
    );
    assert_eq!(
        fs::read_to_string(&site_dir).unwrap(),
        "blocks directory creation"
    );
    fs::remove_file(site_dir).unwrap();
    service.reconcile_all().await.unwrap();
    assert_eq!(service.get(site).await.unwrap().status, Status::Ready);
    service.close().await;
    drop(service);

    let path = database(root.path(), site);
    let backup = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let service = SiteService::open(root.path().to_path_buf()).await.unwrap();
    assert_eq!(
        service.get(site).await.unwrap().error_code.as_deref(),
        Some("SITE_STORAGE_UNAVAILABLE")
    );
    service.provision(site, project).await.unwrap();
    service
        .set_status(site, DesiredStatus::Ready)
        .await
        .unwrap();
    assert!(
        !path.exists(),
        "lost site data must not be replaced with an empty database"
    );
    fs::write(path, backup).unwrap();
    service.reconcile_all().await.unwrap();
    assert_eq!(service.get(site).await.unwrap().status, Status::Ready);
    service.close().await;
}

#[tokio::test]
async fn wrong_database_identity_is_rejected_without_overwriting_it() {
    let root = TempDir::new().unwrap();
    let service = SiteService::open(root.path().to_path_buf()).await.unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let project = Uuid::new_v4();
    service.provision(a, project).await.unwrap();
    service.reconcile_all().await.unwrap();
    service.provision(b, project).await.unwrap();
    let b_path = database(root.path(), b);
    fs::create_dir_all(b_path.parent().unwrap()).unwrap();
    fs::copy(database(root.path(), a), &b_path).unwrap();
    service.reconcile_all().await.unwrap();
    assert_eq!(service.get(b).await.unwrap().status, Status::Failed);
    let mut db = connect(&b_path).await;
    let actual: String = sqlx::query_scalar("SELECT site_id FROM __sites_identity")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(actual, a.to_string());
    db.close().await.unwrap();
    service.close().await;
}

#[tokio::test]
async fn volume_has_one_owner_and_metadata_loss_fails_closed() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let mut second = command(root.path());
    second.env("SITES_BIND_ADDRESS", "127.0.0.1:0");
    second.stderr(Stdio::piped());
    let output = second.output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Locked"));
    let site = Uuid::new_v4();
    server.provision(site, Uuid::new_v4()).await;
    server.wait_status(site, "ready").await;
    drop(server);
    fs::remove_file(root.path().join("service.sqlite")).unwrap();
    assert!(SiteService::open(root.path().to_path_buf()).await.is_err());
    assert!(database(root.path(), site).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn managed_storage_rejects_symlinks_and_uses_private_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let service = SiteService::open(root.path().to_path_buf()).await.unwrap();
    let site = Uuid::new_v4();
    service.provision(site, Uuid::new_v4()).await.unwrap();
    symlink(
        outside.path(),
        root.path().join("sites").join(site.to_string()),
    )
    .unwrap();
    service.reconcile_all().await.unwrap();
    assert_eq!(service.get(site).await.unwrap().status, Status::Failed);
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    fs::remove_file(root.path().join("sites").join(site.to_string())).unwrap();
    service.reconcile_all().await.unwrap();
    assert_eq!(service.get(site).await.unwrap().status, Status::Ready);
    assert_eq!(
        fs::metadata(database(root.path(), site))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    service.close().await;
}

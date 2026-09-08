use std::{
    fs,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, Method, Response, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

const TOKEN: &str = "publication-test-token-01234567890123456789";

struct Server {
    child: Child,
    api: String,
    content: String,
    client: Client,
}
impl Server {
    async fn start(root: &Path) -> Self {
        let a = TcpListener::bind("127.0.0.1:0").unwrap();
        let b = TcpListener::bind("127.0.0.1:0").unwrap();
        let api_address = a.local_addr().unwrap();
        let content_address = b.local_addr().unwrap();
        drop((a, b));
        let api = format!("http://{api_address}");
        let content = format!("http://{content_address}");
        let child = Command::new(env!("CARGO_BIN_EXE_platform-sites-service"))
            .env("SITES_DATA_DIR", root)
            .env("SITES_API_TOKEN", TOKEN)
            .env("SITES_BIND_ADDRESS", api_address.to_string())
            .env("SITES_CONTENT_BIND_ADDRESS", content_address.to_string())
            .env("SITES_CONTENT_ORIGIN", &content)
            .env("SITES_DASHBOARD_ORIGIN", "http://127.0.0.1:5173")
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            api,
            content,
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap(),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "service exited during startup"
            );
            if let Ok(response) = server
                .client
                .get(format!("{}/readyz", server.api))
                .bearer_auth(TOKEN)
                .send()
                .await
            {
                if response.status().is_success() {
                    break;
                }
            }
            assert!(Instant::now() < deadline);
            sleep(Duration::from_millis(25)).await;
        }
        server
    }
    async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Response {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.api))
            .bearer_auth(TOKEN);
        if let Some(body) = body {
            request = request.json(body);
        }
        request.send().await.unwrap()
    }
    async fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        expected: StatusCode,
    ) -> Value {
        let response = self.request(method, path, body).await;
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        assert_eq!(status, expected, "{path}: {value}");
        value
    }
    async fn site(&self) -> Uuid {
        let id = Uuid::new_v4();
        self.json(
            Method::PUT,
            &site_path(id),
            Some(&json!({"project_id":Uuid::new_v4()})),
            StatusCode::ACCEPTED,
        )
        .await;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let site = self
                .json(Method::GET, &site_path(id), None, StatusCode::OK)
                .await;
            if site["status"] == "ready" {
                return id;
            }
            assert!(Instant::now() < deadline);
            sleep(Duration::from_millis(25)).await;
        }
    }
    async fn upload_revision(&self, site: Uuid, revision: Uuid) -> Value {
        self.json(
            Method::POST,
            &format!("{}/revisions", site_path(site)),
            Some(&source(revision)),
            StatusCode::OK,
        )
        .await
    }
    async fn upload_release(&self, site: Uuid, release: &Value) -> Value {
        self.json(
            Method::POST,
            &format!("{}/releases", site_path(site)),
            Some(release),
            StatusCode::OK,
        )
        .await
    }
    async fn activate(
        &self,
        site: Uuid,
        release: Uuid,
        generation: i64,
        key: &str,
    ) -> (StatusCode, Value) {
        let response = self
            .client
            .put(format!("{}{}/active-release", self.api, site_path(site)))
            .bearer_auth(TOKEN)
            .header("Idempotency-Key", key)
            .json(&json!({"release_id":release,"expected_generation":generation}))
            .send()
            .await
            .unwrap();
        (response.status(), response.json().await.unwrap())
    }
    async fn access(&self, site: Uuid, release: Uuid) -> String {
        self.json(
            Method::POST,
            &format!("{}/releases/{release}/content-access", site_path(site)),
            Some(&json!({})),
            StatusCode::OK,
        )
        .await["url"]
            .as_str()
            .unwrap()
            .into()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn site_path(site: Uuid) -> String {
    format!("/internal/sites/{site}")
}
fn file(path: &str, bytes: &[u8]) -> Value {
    json!({"path":path,"content_base64":STANDARD.encode(bytes),"sha256":format!("{:x}",Sha256::digest(bytes))})
}
fn source(id: Uuid) -> Value {
    // Larger than the normal 16 KiB body cap: upload routes must have their own cap.
    json!({"id":id,"files":[file("backend.ts",b"export default () => ({ok:true})"),file("frontend/index.html",b"<html>source</html>"),file("frontend/large.js",&vec![b' ';20000])]})
}
fn release(id: Uuid, source: Uuid, title: &str) -> Value {
    json!({"id":id,"manifest":{"source_revision_id":source,"frontend_entrypoint":"public/index.html","backend_entrypoint":"backend.js","sdk_version":"1"},
        "files":[file("public/index.html",format!("<!doctype html><title>{title}</title><link rel=stylesheet href=styles.css><script type=module src=app.js></script>").as_bytes()),
                 file("public/app.js",b"document.body.dataset.loaded='yes';"),file("public/styles.css",b"body { color: red; }"),file("public/icon.png",b"\x89PNG\r\n\x1a\n"),
                 file("backend.js",b"export default () => 'private-backend';")]})
}
async fn connect(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn publish_switch_rollback_restart_and_keep_site_data() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let site = server.site().await;
    let revision = Uuid::new_v4();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let original_source = server.upload_revision(site, revision).await;
    let source_file = server
        .request(
            Method::GET,
            &format!("{}/revisions/{revision}/files/backend.ts", site_path(site)),
            None,
        )
        .await;
    assert_eq!(source_file.headers()["content-disposition"], "attachment");
    assert!(source_file.text().await.unwrap().contains("export default"));
    assert_eq!(
        server.upload_revision(site, revision).await,
        original_source
    );
    let a_input = release(a, revision, "Release A");
    let a_record = server.upload_release(site, &a_input).await;
    let mut reordered = a_input.clone();
    reordered["files"].as_array_mut().unwrap().reverse();
    assert_eq!(server.upload_release(site, &reordered).await, a_record);
    server
        .upload_release(site, &release(b, revision, "Release B"))
        .await;
    let database = root
        .path()
        .join("sites")
        .join(site.to_string())
        .join("data/site.sqlite");
    let mut db = connect(&database).await;
    sqlx::query("CREATE TABLE results (score INTEGER)")
        .execute(&mut db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO results VALUES (99)")
        .execute(&mut db)
        .await
        .unwrap();
    db.close().await.unwrap();
    let (status, activated_a) = server.activate(site, a, 0, "activate-a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(activated_a["release_generation"], 1);
    let a_url = server.access(site, a).await;
    assert!(
        server
            .client
            .get(&a_url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
            .contains("Release A")
    );
    assert_eq!(
        server.activate(site, b, 1, "activate-b").await.0,
        StatusCode::OK
    );
    assert!(
        server
            .client
            .get(server.access(site, b).await)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
            .contains("Release B")
    );
    // An old frontend's immutable URL remains valid after activation changes.
    assert!(
        server
            .client
            .get(&a_url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
            .contains("Release A")
    );
    assert_eq!(
        server.activate(site, a, 0, "activate-a").await.1,
        activated_a
    );
    assert_eq!(
        server
            .json(Method::GET, &site_path(site), None, StatusCode::OK)
            .await["active_release_id"],
        b.to_string()
    );
    assert_eq!(
        server.activate(site, a, 1, "stale-new-command").await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        server.activate(site, a, 2, "rollback-a").await.0,
        StatusCode::OK
    );
    assert_eq!(
        server.activate(site, b, 1, "aba-stale-command").await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        server.activate(site, b, 3, "activate-a").await.0,
        StatusCode::CONFLICT
    );
    let page = server
        .json(
            Method::GET,
            &format!("{}/releases?limit=1", site_path(site)),
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let cursor = page["next_after"].as_str().unwrap();
    let next = server
        .json(
            Method::GET,
            &format!("{}/releases?after={cursor}&limit=1", site_path(site)),
            None,
            StatusCode::OK,
        )
        .await;
    assert_ne!(page["items"][0]["id"], next["items"][0]["id"]);
    drop(server);
    let server = Server::start(root.path()).await;
    let state = server
        .json(Method::GET, &site_path(site), None, StatusCode::OK)
        .await;
    assert_eq!(state["active_release_id"], a.to_string());
    assert_eq!(state["release_generation"], 3);
    assert_eq!(
        server
            .json(
                Method::GET,
                &format!("{}/releases/{a}", site_path(site)),
                None,
                StatusCode::OK
            )
            .await,
        a_record
    );
    assert!(
        server
            .client
            .get(server.access(site, a).await)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
            .contains("Release A")
    );
    let mut db = connect(&database).await;
    let score: i64 = sqlx::query_scalar("SELECT score FROM results")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(score, 99);
    db.close().await.unwrap();
}

#[tokio::test]
async fn reject_invalid_bundles_and_cross_site_sources() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let site = server.site().await;
    let source_id = Uuid::new_v4();
    server.upload_revision(site, source_id).await;
    let id = Uuid::new_v4();
    let valid = release(id, source_id, "valid");
    server.upload_release(site, &valid).await;
    let changed = release(id, source_id, "changed");
    let path = format!("{}/releases", site_path(site));
    server
        .json(Method::POST, &path, Some(&changed), StatusCode::CONFLICT)
        .await;
    let mut variants = Vec::new();
    for bad_path in [
        "../escape",
        "/escape",
        "public/../../escape",
        "public\\escape",
        "public/%2e%2e/x",
        "public/.env",
        "public/con.txt",
    ] {
        let mut bad = valid.clone();
        bad["id"] = json!(Uuid::new_v4());
        bad["files"][0]["path"] = json!(bad_path);
        variants.push(bad);
    }
    let mut bad = valid.clone();
    bad["manifest"]["sdk_version"] = json!("2");
    variants.push(bad);
    let mut bad = valid.clone();
    bad["manifest"]["frontend_entrypoint"] = json!("public/missing.html");
    variants.push(bad);
    let mut bad = valid.clone();
    bad["files"][0]["sha256"] = json!("0".repeat(64));
    variants.push(bad);
    let mut bad = valid.clone();
    bad["files"][0]["content_base64"] = json!("invalid!");
    variants.push(bad);
    let mut bad = valid.clone();
    bad["files"]
        .as_array_mut()
        .unwrap()
        .push(file("public/APP.js", b"collision"));
    variants.push(bad);
    let mut bad = valid.clone();
    bad["files"].as_array_mut().unwrap().extend([
        file("public/Assets/first.js", b"first"),
        file("public/assets/second.js", b"second"),
    ]);
    variants.push(bad);
    let mut bad = valid.clone();
    bad["files"]
        .as_array_mut()
        .unwrap()
        .push(file("public", b"file-dir conflict"));
    variants.push(bad);
    let mut bad = valid.clone();
    bad["files"]
        .as_array_mut()
        .unwrap()
        .push(file("secrets.txt", b"not public or backend"));
    variants.push(bad);
    for bad in variants {
        server
            .json(Method::POST, &path, Some(&bad), StatusCode::BAD_REQUEST)
            .await;
    }
    let other = server.site().await;
    server
        .json(
            Method::POST,
            &format!("{}/releases", site_path(other)),
            Some(&valid),
            StatusCode::NOT_FOUND,
        )
        .await;
    server
        .json(
            Method::GET,
            &format!("{}/releases?unknown=1", site_path(site)),
            None,
            StatusCode::BAD_REQUEST,
        )
        .await;
    server
        .json(
            Method::GET,
            &format!("{}/releases?limit=101", site_path(site)),
            None,
            StatusCode::BAD_REQUEST,
        )
        .await;
    let oversized = server
        .client
        .post(format!("{}{path}", server.api))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .body(vec![b' '; 24 * 1024 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!root.path().join("escape").exists());
}

#[tokio::test]
async fn content_is_scoped_isolated_and_never_serves_backend_or_storage() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let site = server.site().await;
    let source = Uuid::new_v4();
    let id = Uuid::new_v4();
    server.upload_revision(site, source).await;
    server
        .upload_release(site, &release(id, source, "isolated"))
        .await;
    let url = server.access(site, id).await;
    assert!(url.starts_with(&server.content));
    assert!(!url.contains(TOKEN));
    let response = server.client.get(&url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(csp.contains("sandbox allow-scripts"));
    assert!(!csp.contains("allow-same-origin"));
    assert!(csp.contains("frame-ancestors http://127.0.0.1:5173"));
    assert!(csp.contains("connect-src 'none'"));
    assert_eq!(response.headers()["cache-control"], "private, no-cache");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert!(!response.headers().contains_key("set-cookie"));
    let etag = response.headers()["etag"].clone();
    assert_eq!(
        server
            .client
            .get(&url)
            .header("if-none-match", etag.clone())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_MODIFIED
    );
    let base = url::Url::parse(&url).unwrap();
    let js = server
        .client
        .get(base.join("app.js").unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(
        js.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
    assert_eq!(js.headers()["access-control-allow-origin"], "null");
    for name in [
        "backend.js",
        "site.sqlite",
        "service.sqlite",
        "backend.ts",
        "missing.html",
    ] {
        assert_eq!(
            server
                .client
                .get(base.join(name).unwrap())
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    let traversal = url.replace("index.html", "%2e%2e%2fbackend.js");
    assert_eq!(
        server.client.get(traversal).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let wrong_release = url.replace(&id.to_string(), &Uuid::new_v4().to_string());
    assert_eq!(
        server
            .client
            .get(wrong_release)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut segments: Vec<_> = url.split('/').collect();
    segments[4] = "invalid-ticket";
    assert_eq!(
        server
            .client
            .get(segments.join("/"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        server
            .client
            .get(format!("{}/internal/sites/{site}", server.content))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        server
            .client
            .get(&url)
            .header("host", "wrong-origin.test")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let without_auth = server
        .client
        .post(format!(
            "{}{}/releases/{id}/content-access",
            server.api,
            site_path(site)
        ))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(without_auth.status(), StatusCode::UNAUTHORIZED);
    server
        .json(
            Method::PATCH,
            &site_path(site),
            Some(&json!({"status":"suspended"})),
            StatusCode::OK,
        )
        .await;
    assert_eq!(
        server
            .client
            .get(&url)
            .header("if-none-match", etag)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        server.activate(site, id, 0, "suspended").await.0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn recovery_finishes_complete_staging_and_requires_retry_for_partial_uploads() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let site = server.site().await;
    let source = Uuid::new_v4();
    server.upload_revision(site, source).await;
    let ids: Vec<_> = (0..3).map(|_| Uuid::new_v4()).collect();
    let inputs: Vec<_> = ids
        .iter()
        .map(|id| release(*id, source, "recover"))
        .collect();
    for input in &inputs {
        server.upload_release(site, input).await;
    }
    drop(server);
    let mut metadata = connect(&root.path().join("service.sqlite")).await;
    sqlx::query("UPDATE bundles SET status='staging' WHERE kind='release'")
        .execute(&mut metadata)
        .await
        .unwrap();
    metadata.close().await.unwrap();
    // First release stays in its final directory (rename committed before DB).
    // Second has a complete staging directory; third has incomplete staging.
    for id in &ids[1..] {
        let final_dir = root
            .path()
            .join("sites")
            .join(site.to_string())
            .join("releases")
            .join(id.to_string());
        let staged = root
            .path()
            .join("staging")
            .join(format!("{site}-release-{id}"));
        fs::rename(final_dir, staged).unwrap();
    }
    fs::remove_file(
        root.path()
            .join("staging")
            .join(format!("{site}-release-{}", ids[2]))
            .join(".bundle.json"),
    )
    .unwrap();
    let server = Server::start(root.path()).await;
    for id in &ids[..2] {
        let record = server
            .json(
                Method::GET,
                &format!("{}/releases/{id}", site_path(site)),
                None,
                StatusCode::OK,
            )
            .await;
        assert_eq!(record["status"], "ready");
    }
    let incomplete = server
        .json(
            Method::GET,
            &format!("{}/releases/{}", site_path(site), ids[2]),
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(incomplete["status"], "failed");
    assert_eq!(
        server.activate(site, ids[2], 0, "partial").await.0,
        StatusCode::CONFLICT
    );
    server.upload_release(site, &inputs[2]).await;
    assert_eq!(
        server.activate(site, ids[2], 0, "partial").await.0,
        StatusCode::OK
    );
    // Verify before switching: altered/missing files cannot become active.
    let corrupt = root
        .path()
        .join("sites")
        .join(site.to_string())
        .join("releases")
        .join(ids[0].to_string())
        .join("public/app.js");
    fs::write(corrupt, "tampered").unwrap();
    assert_eq!(
        server.activate(site, ids[0], 1, "corrupt").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        server
            .json(Method::GET, &site_path(site), None, StatusCode::OK)
            .await["active_release_id"],
        ids[2].to_string()
    );
}

#[tokio::test]
async fn concurrent_activation_has_one_winner() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let site = server.site().await;
    let source = Uuid::new_v4();
    server.upload_revision(site, source).await;
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    server.upload_release(site, &release(a, source, "A")).await;
    server.upload_release(site, &release(b, source, "B")).await;
    let (first, second) = tokio::join!(
        server.activate(site, a, 0, "first"),
        server.activate(site, b, 0, "second")
    );
    assert!(matches!(
        (first.0, second.0),
        (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
    ));
    assert_eq!(
        server
            .json(Method::GET, &site_path(site), None, StatusCode::OK)
            .await["release_generation"],
        1
    );
}

#[path = "support/authoring.rs"]
mod authoring;

//! End-to-end tests launch the real service and OS-sandboxed QuickJS child.
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};
use uuid::Uuid;
const TOKEN: &str = "backend-tests-internal-01234567890123456789";
struct Server {
    child: Child,
    url: String,
    client: Client,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Server {
    async fn start(root: &Path) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let child = Command::new(env!("CARGO_BIN_EXE_platform-sites-service"))
            .env("SITES_DATA_DIR", root)
            .env("SITES_API_TOKEN", TOKEN)
            .env("SITES_BIND_ADDRESS", address.to_string())
            .env("SITES_CONTENT_ORIGIN", "")
            .env("SITES_DASHBOARD_ORIGIN", "")
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            url: format!("http://{address}"),
            client: Client::builder()
                .timeout(Duration::from_secs(40))
                .build()
                .unwrap(),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(server.child.try_wait().unwrap().is_none(), "service exited");
            if server
                .client
                .get(format!("{}/readyz", server.url))
                .bearer_auth(TOKEN)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            assert!(Instant::now() < deadline);
            sleep(Duration::from_millis(25)).await;
        }
        server
    }
    async fn call(&self, method: Method, path: &str, input: Option<Value>) -> (StatusCode, Value) {
        let mut r = self
            .client
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(TOKEN);
        if let Some(input) = input {
            r = r.json(&input);
        }
        let response = r.send().await.unwrap();
        let status = response.status();
        let body = response.json().await.unwrap();
        (status, body)
    }
    async fn ok(&self, method: Method, path: &str, input: Option<Value>) -> Value {
        let (status, body) = self.call(method, path, input).await;
        assert!(status.is_success(), "{path}: {status} {body}");
        body
    }
    async fn site(&self) -> Uuid {
        let id = Uuid::new_v4();
        self.ok(
            Method::PUT,
            &format!("/internal/sites/{id}"),
            Some(json!({"project_id":Uuid::new_v4()})),
        )
        .await;
        for _ in 0..100 {
            let record = self
                .ok(Method::GET, &format!("/internal/sites/{id}"), None)
                .await;
            if record["status"] == "ready" {
                return id;
            }
            sleep(Duration::from_millis(25)).await;
        }
        panic!("site not ready")
    }
    async fn status(&self, site: Uuid, status: &str) {
        self.ok(
            Method::PATCH,
            &format!("/internal/sites/{site}"),
            Some(json!({"status":status})),
        )
        .await;
    }
    async fn publish(
        &self,
        site: Uuid,
        code: &str,
        migrations: Vec<&str>,
        min: u32,
        max: u32,
    ) -> Uuid {
        let source = Uuid::new_v4();
        let release = Uuid::new_v4();
        let mut files = vec![
            file("backend.ts", code),
            file("frontend/index.html", "<h1>Test</h1>"),
        ];
        let mut manifest = Vec::new();
        for (i, sql) in migrations.iter().enumerate() {
            let path = format!("migrations/{:03}.sql", i + 1);
            files.push(file(&path, sql));
            manifest.push(json!({"version":i+1,"path":path}));
        }
        self.ok(
            Method::POST,
            &format!("/internal/sites/{site}/revisions"),
            Some(json!({"id":source,"files":files})),
        )
        .await;
        self.ok(Method::POST,&format!("/internal/sites/{site}/releases"),Some(json!({"id":release,"manifest":{
            "source_revision_id":source,"frontend_entrypoint":"public/index.html","backend_entrypoint":"backend.js","sdk_version":"1",
            "schema":{"min":min,"max":max},"migrations":manifest
        },"files":[file("backend.js",code),file("public/index.html","<h1>Test</h1>")]}))).await;
        release
    }
    async fn migrate(&self, site: Uuid, release: Uuid) -> (StatusCode, Value) {
        self.call(
            Method::POST,
            &format!("/internal/sites/{site}/schema/migrations"),
            Some(json!({"release_id":release})),
        )
        .await
    }
    async fn invoke(&self, site: Uuid, release: Uuid, path: &str, body: Value) -> Value {
        self.ok(
            Method::POST,
            &format!("/internal/sites/{site}/invocations"),
            Some(invocation(Uuid::new_v4(), release, path, body, 5000)),
        )
        .await
    }
}
fn file(path: &str, text: &str) -> Value {
    json!({"path":path,"content_base64":STANDARD.encode(text),"sha256":format!("{:x}",Sha256::digest(text.as_bytes()))})
}
fn invocation(id: Uuid, release: Uuid, path: &str, body: Value, timeout: u64) -> Value {
    json!({"id":id,"release_id":release,"timeout_ms":timeout,"request":{"method":"POST","path":path,"body":body}})
}
const MIGRATION: &str = "CREATE TABLE items(id TEXT PRIMARY KEY, value INTEGER NOT NULL);";
const CODE: &str = r#"
export default async function handle(request,ctx) {
  const {db}=ctx;
  if(request.path==='/write') {
    await db.execute('INSERT INTO items VALUES (?,?)',[request.body.id,request.body.value]);
    ctx.log.info('saved',{id:request.body.id});
  }
  if(request.path==='/sql') {
    try { const result = await db[request.body.method](request.body.sql,request.body.params ?? []); return {status:200,body:result}; }
    catch(e) { return {status:400,body:{code:e.code}}; }
  }
  if(request.path==='/transaction') {
    await db.transaction(async tx => {
      const rows=await tx.query('SELECT value FROM items WHERE id=?',['a']);
      await tx.execute('UPDATE items SET value=? WHERE id=?',[rows[0].value+1,'a']);
    });
  }
  if(request.path==='/rollback') {
    try { await db.transaction(async tx => { await tx.execute('INSERT INTO items VALUES (?,?)',['rollback',42]); throw Error('stop'); }); } catch {}
  }
  if(request.path==='/batch') {
    try { await db.batch([{sql:'INSERT INTO items VALUES (?,?)',params:['batch',1]}, {sql:'INSERT INTO items VALUES (?,?)',params:['a',9]}]); } catch {}
  }
  if(request.path==='/nested') {
    try { await db.transaction(async tx => { await tx.execute('INSERT INTO items VALUES (?,?)',['nested',1]); await db.transaction(async () => {}); }); } catch {}
  }
  if(request.path==='/capabilities') return {status:200,body:{site:ctx.site,invocation:ctx.invocation,process:typeof process,fetch:typeof fetch,require:typeof require,platform:typeof ctx.platform,host:typeof __sitesHost}};
  return {status:200,body:await db.query('SELECT * FROM items ORDER BY id')};
}
"#;
#[tokio::test]
async fn real_backend_sql_transactions_isolation_receipts_and_backup() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let site = server.site().await;
    let other = server.site().await;
    let release = server.publish(site, CODE, vec![MIGRATION], 1, 1).await;
    assert_eq!(server.migrate(site, release).await.0, StatusCode::CONFLICT);
    server.status(site, "suspended").await;
    assert_eq!(server.migrate(site, release).await.0, StatusCode::OK);
    assert_eq!(server.migrate(site, release).await.0, StatusCode::OK);
    server.status(site, "ready").await;
    let first = server
        .invoke(site, release, "/write", json!({"id":"a","value":4}))
        .await;
    assert_eq!(first["status"], "succeeded", "{first}");
    assert_eq!(first["logs"][0]["message"], "saved");
    for path in ["/transaction", "/rollback", "/batch", "/nested"] {
        let result = server.invoke(site, release, path, Value::Null).await;
        assert_eq!(result["status"], "succeeded", "{path}: {result}");
        assert_eq!(result["response"]["body"], json!([{"id":"a","value":5}]));
    }
    let caps = server
        .invoke(site, release, "/capabilities", Value::Null)
        .await;
    let body = &caps["response"]["body"];
    for key in ["process", "fetch", "require", "host"] {
        assert_eq!(body[key], "undefined", "{caps}");
    }
    assert_eq!(body["platform"], "object");
    assert_eq!(body["site"]["id"], site.to_string());
    let malicious = [
        "SELECT * FROM __sites_identity",
        "SELECT * FROM sqlite_master",
        "ATTACH DATABASE ':memory:' AS other",
        "PRAGMA writable_schema=ON",
        "CREATE TABLE stolen(x)",
        "DELETE FROM __sites_migrations",
        "SELECT load_extension('bad')",
        "SELECT * FROM pragma_table_info('items')",
        "SELECT 1; DELETE FROM items",
        "BEGIN",
        "SELECT 9223372036854775807",
        "SELECT randomblob(10)",
    ];
    for sql in malicious {
        let result = server
            .invoke(site, release, "/sql", json!({"method":"execute","sql":sql}))
            .await;
        assert_eq!(result["response"]["status"], 400, "{sql}: {result}");
    }
    let readonly = server
        .invoke(
            site,
            release,
            "/sql",
            json!({"method":"query","sql":"DELETE FROM items"}),
        )
        .await;
    assert_eq!(readonly["response"]["body"]["code"], "READ_ONLY_QUERY");
    let values=server.invoke(site,release,"/sql",json!({"method":"query","sql":"SELECT ? AS value","params":["'; DROP TABLE items; --"]})).await;
    assert_eq!(
        values["response"]["body"][0]["value"],
        "'; DROP TABLE items; --"
    );
    let rows=server.invoke(site,release,"/sql",json!({"method":"query","sql":"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1001) SELECT x FROM n"})).await;
    assert_eq!(rows["response"]["body"]["code"], "RESULT_LIMIT");
    let request = invocation(
        Uuid::new_v4(),
        release,
        "/write",
        json!({"id":"receipt","value":1}),
        5000,
    );
    let a = server
        .ok(
            Method::POST,
            &format!("/internal/sites/{site}/invocations"),
            Some(request.clone()),
        )
        .await;
    let b = server
        .ok(
            Method::POST,
            &format!("/internal/sites/{site}/invocations"),
            Some(request.clone()),
        )
        .await;
    assert_eq!(a, b);
    let mut changed = request;
    changed["request"]["body"]["value"] = json!(99);
    assert_eq!(
        server
            .call(
                Method::POST,
                &format!("/internal/sites/{site}/invocations"),
                Some(changed)
            )
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        server
            .call(
                Method::POST,
                &format!("/internal/sites/{other}/invocations"),
                Some(invocation(
                    Uuid::new_v4(),
                    release,
                    "/read",
                    Value::Null,
                    1000
                ))
            )
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        server
            .call(
                Method::GET,
                &format!(
                    "/internal/sites/{other}/invocations/{}",
                    a["id"].as_str().unwrap()
                ),
                None
            )
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let backup = Uuid::new_v4();
    let url = format!("/internal/sites/{site}/backups/{backup}");
    let saved = server.ok(Method::PUT, &url, None).await;
    assert_eq!(saved, server.ok(Method::PUT, &url, None).await);
    let bytes = server
        .client
        .get(format!("{}{url}", server.url))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let backup_file = root.path().join("download.sqlite");
    std::fs::write(&backup_file, bytes).unwrap();
    let conn = rusqlite::Connection::open(&backup_file).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM items", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(conn);
    drop(server);
    let server = Server::start(root.path()).await;
    let result = server.invoke(site, release, "/read", Value::Null).await;
    assert_eq!(result["response"]["body"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn limits_errors_migration_rollback_and_release_compatibility() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let site = server.site().await;
    let old = server
        .publish(
            site,
            "export default () => ({status:200,body:'old'});",
            vec![],
            0,
            0,
        )
        .await;
    let failing = server
        .publish(
            site,
            CODE,
            vec![
                MIGRATION,
                "CREATE TABLE good(x); INSERT INTO absent VALUES(1);",
            ],
            2,
            2,
        )
        .await;
    server.status(site, "suspended").await;
    assert_eq!(server.migrate(site, failing).await.0, StatusCode::CONFLICT);
    assert_eq!(
        server
            .ok(Method::GET, &format!("/internal/sites/{site}/schema"), None)
            .await["version"],
        0
    );
    server.status(site, "ready").await;
    let release = server.publish(site, CODE, vec![MIGRATION], 1, 1).await;
    server.status(site, "suspended").await;
    assert_eq!(server.migrate(site, release).await.0, StatusCode::OK);
    server.status(site, "ready").await;
    let (status, body) = server
        .call(
            Method::POST,
            &format!("/internal/sites/{site}/invocations"),
            Some(invocation(Uuid::new_v4(), old, "/", Value::Null, 1000)),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "SCHEMA_INCOMPATIBLE");
    let incompatible = server
        .publish(site, CODE, vec!["CREATE TABLE different(x);"], 1, 1)
        .await;
    server.status(site, "suspended").await;
    assert_eq!(
        server.migrate(site, incompatible).await.1["error"]["code"],
        "MIGRATION_CONFLICT"
    );
    server.status(site, "ready").await;
    for (code, error, timeout) in [
        (
            "export default () => { while(true) {} };",
            "INVOCATION_TIMEOUT",
            200,
        ),
        (
            "export default () => { throw Error('private failure'); };",
            "BACKEND_ERROR",
            1000,
        ),
        (
            "export default () => ({status:200, body:'x'.repeat(300000)});",
            "RESPONSE_LIMIT",
            1000,
        ),
        (
            "import fs from 'fs'; export default () => ({});",
            "BACKEND_ERROR",
            1000,
        ),
        (
            "export default () => ({status:101, body:null});",
            "INVALID_RESPONSE",
            1000,
        ),
        (
            "export default () => { const x=[]; for(;;) x.push(new Uint8Array(1048576)); };",
            "BACKEND_ERROR",
            3000,
        ),
    ] {
        let r = server.publish(site, code, vec![MIGRATION], 1, 1).await;
        let result = server
            .ok(
                Method::POST,
                &format!("/internal/sites/{site}/invocations"),
                Some(invocation(Uuid::new_v4(), r, "/", Value::Null, timeout)),
            )
            .await;
        assert_eq!(result["error_code"], error, "{code}: {result}");
    }
    let code = "export default async (r,ctx) => { await ctx.db.transaction(async tx => { await tx.execute(\"INSERT INTO items VALUES ('timeout',1)\"); while(true) {} }); return {status:200,body:null}; };";
    let r = server.publish(site, code, vec![MIGRATION], 1, 1).await;
    let result = server
        .ok(
            Method::POST,
            &format!("/internal/sites/{site}/invocations"),
            Some(invocation(Uuid::new_v4(), r, "/", Value::Null, 200)),
        )
        .await;
    assert_eq!(result["status"], "timed_out");
    assert_eq!(
        server.invoke(site, release, "/read", Value::Null).await["response"]["body"],
        json!([])
    );
}

#[tokio::test]
async fn restart_reports_interrupted_without_replaying_effects() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let site = server.site().await;
    let code = "export default async (r,ctx) => { await ctx.db.execute(\"INSERT INTO items VALUES ('saved',1)\"); while(true) {} };";
    let release = server.publish(site, code, vec![MIGRATION], 1, 1).await;
    server.status(site, "suspended").await;
    assert_eq!(server.migrate(site, release).await.0, StatusCode::OK);
    server.status(site, "ready").await;
    let id = Uuid::new_v4();
    let request = invocation(id, release, "/", Value::Null, 10000);
    let url = format!("{}/internal/sites/{site}/invocations", server.url);
    let client = server.client.clone();
    let payload = request.clone();
    let running = tokio::spawn(async move {
        client
            .post(url)
            .bearer_auth(TOKEN)
            .json(&payload)
            .send()
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let file = root
            .path()
            .join("sites")
            .join(site.to_string())
            .join("data/site.sqlite");
        let conn = rusqlite::Connection::open(file).unwrap();
        let saved = conn
            .query_row("SELECT count(*) FROM items", [], |r| r.get::<_, i64>(0))
            .unwrap();
        if saved == 1 {
            break;
        }
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(25)).await;
    }
    drop(server);
    let _ = running.await;
    let server = Server::start(root.path()).await;
    let result = server
        .ok(
            Method::POST,
            &format!("/internal/sites/{site}/invocations"),
            Some(request),
        )
        .await;
    assert_eq!(result["status"], "interrupted");
    assert_eq!(result["error_code"], "SERVICE_RESTARTED");
}

#[tokio::test]
async fn concurrent_calls_disconnect_and_bounded_database_failures() {
    let root = TempDir::new().unwrap();
    let server = Server::start(root.path()).await;
    let site = server.site().await;
    let release = server.publish(site, CODE, vec![MIGRATION], 1, 1).await;
    server.status(site, "suspended").await;
    assert_eq!(server.migrate(site, release).await.0, StatusCode::OK);
    server.status(site, "ready").await;
    server
        .invoke(site, release, "/write", json!({"id":"a","value":0}))
        .await;
    let (a, b) = tokio::join!(
        server.invoke(site, release, "/transaction", Value::Null),
        server.invoke(site, release, "/transaction", Value::Null)
    );
    assert_eq!(a["status"], "succeeded");
    assert_eq!(b["status"], "succeeded");
    assert_eq!(
        server.invoke(site, release, "/read", Value::Null).await["response"]["body"][0]["value"],
        2
    );
    let oversized=server.invoke(site,release,"/sql",json!({"method":"execute","sql":"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1001) INSERT INTO items SELECT CAST(x AS TEXT), x FROM n RETURNING id"})).await;
    assert_eq!(oversized["response"]["body"]["code"], "RESULT_LIMIT");
    assert_eq!(
        server.invoke(site, release, "/read", Value::Null).await["response"]["body"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let slow_sql=server.invoke(site,release,"/sql",json!({"method":"query","sql":"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT SUM(x) FROM n"})).await;
    assert_eq!(slow_sql["response"]["status"], 400, "{slow_sql}");
    let tx_code = r#"export default async (r,ctx) => {
      let code;
      try { await ctx.db.transaction(async tx => {
        await tx.execute("INSERT INTO items VALUES ('expired',9)");
        const until=Date.now()+2100; while(Date.now()<until) {}
      }); } catch(e) { code=e.code; }
      return {status:200,body:{code,rows:await ctx.db.query('SELECT * FROM items')}};
    }"#;
    let tx_release = server.publish(site, tx_code, vec![MIGRATION], 1, 1).await;
    let expired = server.invoke(site, tx_release, "/", Value::Null).await;
    assert_eq!(
        expired["response"]["body"]["code"], "TRANSACTION_TIMEOUT",
        "{expired}"
    );
    assert_eq!(
        expired["response"]["body"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let code = r#"export default async (r,ctx) => {
        const until=Date.now()+500; while(Date.now()<until) {}
        await ctx.db.execute("INSERT INTO items VALUES ('disconnected',3)");
        return {status:200,body:null};
    }"#;
    let delayed = server.publish(site, code, vec![MIGRATION], 1, 1).await;
    let id = Uuid::new_v4();
    let payload = invocation(id, delayed, "/", Value::Null, 5000);
    let url = format!("{}/internal/sites/{site}/invocations", server.url);
    let client = server.client.clone();
    let request = tokio::spawn(async move {
        client
            .post(url)
            .bearer_auth(TOKEN)
            .json(&payload)
            .send()
            .await
    });
    let record_url = format!("/internal/sites/{site}/invocations/{id}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, record) = server.call(Method::GET, &record_url, None).await;
        if status == StatusCode::OK && record["status"] == "running" {
            break;
        }
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(10)).await;
    }
    request.abort();
    loop {
        let record = server.ok(Method::GET, &record_url, None).await;
        if record["status"] != "running" {
            assert_eq!(record["status"], "succeeded", "{record}");
            break;
        }
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(25)).await;
    }
    let read = server.invoke(site, release, "/read", Value::Null).await;
    assert_eq!(read["response"]["body"].as_array().unwrap().len(), 2);
}

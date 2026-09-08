use super::*;
async fn value(s: &Server, method: Method, path: &str, body: Option<&Value>) -> Value {
    let reply = s.request(method, path, body).await;
    let status = reply.status();
    let body = reply.text().await.unwrap();
    let value: Value =
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("{path}: {status}: {body}"));
    assert!(status.is_success(), "{status}: {value}");
    value
}
async fn site(s: &Server) -> Uuid {
    let id = Uuid::now_v7();
    value(
        s,
        Method::PUT,
        &format!("/internal/sites/{id}"),
        Some(&json!({"project_id":Uuid::now_v7()})),
    )
    .await;
    value(
        s,
        Method::PATCH,
        &format!("/internal/sites/{id}"),
        Some(&json!({"status":"ready"})),
    )
    .await;
    id
}
fn patch(old: &str, new: &str) -> Value {
    json!({"id":Uuid::now_v7(),"type":"patch","patch":format!("*** Begin Patch\n*** Update File: backend.js\n@@\n-  return {{status: 200, body: {{message: '{old}'}}}};\n+  return {{status: 200, body: {{message: '{new}'}}}};\n*** End Patch")})
}
async fn author(s: &Server, id: Uuid, input: &Value) -> Value {
    value(
        s,
        Method::POST,
        &format!("/internal/sites/{id}/authoring"),
        Some(input),
    )
    .await
}
async fn source(s: &Server, id: Uuid) -> Value {
    value(
        s,
        Method::GET,
        &format!("/internal/sites/{id}/source"),
        None,
    )
    .await
}
#[tokio::test]
async fn live_edits_validate_pair_and_replay_without_overwriting_newer_code() {
    let root = TempDir::new().unwrap();
    let s = Server::start(root.path()).await;
    let id = site(&s).await;
    let first = patch("New site", "First");
    let accepted = author(&s, id, &first).await;
    assert_eq!(accepted["status"], "succeeded");
    let original = source(&s, id).await;
    for text in [
        "*** Begin Patch\n*** Update File: ../backend.js\n@@\n-x\n+y\n*** End Patch",
        "*** Begin Patch\n*** Delete File: index.html\n*** End Patch",
        "*** Begin Patch\n*** Update File: backend.js\n@@\n-export default async function handle(request, ctx) {\n+export default async function ( {\n*** End Patch",
    ] {
        let response = s
            .request(
                Method::POST,
                &format!("/internal/sites/{id}/authoring"),
                Some(&json!({"id":Uuid::now_v7(),"type":"patch","patch":text})),
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(source(&s, id).await, original);
    }
    author(&s, id, &patch("First", "Second")).await;
    assert_eq!(author(&s, id, &first).await, accepted);
    assert!(
        source(&s, id).await["files"]["backend"]
            .as_str()
            .unwrap()
            .contains("Second")
    );
    let invocation = value(
        &s,
        Method::POST,
        &format!("/internal/sites/{id}/invocations"),
        Some(&json!({"id":Uuid::now_v7(),"request":{"method":"GET","path":"/"}})),
    )
    .await;
    assert_eq!(invocation["response"]["body"]["message"], "Second");
}
#[tokio::test]
async fn user_snapshots_restore_code_and_preserve_live_data() {
    let root = TempDir::new().unwrap();
    let s = Server::start(root.path()).await;
    let id = site(&s).await;
    let a = author(&s, id, &patch("New site", "A")).await;
    let snapshot = json!({"id":Uuid::now_v7(),"type":"snapshot","name":"Keep A"});
    author(&s, id, &snapshot).await;
    author(&s, id, &patch("A", "B")).await;
    let sql_path = format!("/internal/sites/{id}/sql/{}", Uuid::now_v7());
    value(
        &s,
        Method::POST,
        &sql_path,
        Some(&json!({"sql":"CREATE TABLE notes(value TEXT)"})),
    )
    .await;
    let write_path = format!("/internal/sites/{id}/sql/{}", Uuid::now_v7());
    let write = json!({"sql":"INSERT INTO notes VALUES (?)","params":["keep me"]});
    let receipt = value(&s, Method::POST, &write_path, Some(&write)).await;
    assert_eq!(
        value(&s, Method::POST, &write_path, Some(&write)).await,
        receipt
    );
    let restore = json!({"id":Uuid::now_v7(),"type":"restore","snapshot_id":snapshot["id"]});
    assert_eq!(author(&s, id, &restore).await["releaseId"], a["releaseId"]);
    let rows = value(
        &s,
        Method::POST,
        &format!("/internal/sites/{id}/sql/query"),
        Some(&json!({"sql":"SELECT value FROM notes"})),
    )
    .await;
    assert_eq!(rows, json!([{"value":"keep me"}]));
    let list = value(
        &s,
        Method::GET,
        &format!("/internal/sites/{id}/snapshots?limit=1"),
        None,
    )
    .await;
    assert_eq!(list["items"][0]["id"], snapshot["id"]);
    let old=value(&s,Method::POST,&format!("/internal/sites/{id}/invocations"),Some(&json!({"id":Uuid::now_v7(),"release_id":a["releaseId"],"request":{"method":"GET","path":"/"}}))).await;
    assert_eq!(old["response"]["body"]["message"], "A");
    let forbidden = s
        .request(
            Method::POST,
            &format!("/internal/sites/{id}/sql/{}", Uuid::now_v7()),
            Some(&json!({"sql":"DROP TABLE __sites_authoring_receipts"})),
        )
        .await;
    assert_eq!(forbidden.status(), StatusCode::BAD_REQUEST);
}
#[tokio::test]
async fn pending_edit_recovers_after_restart_and_cannot_cross_a_rollback() {
    let root = TempDir::new().unwrap();
    let mut s = Server::start(root.path()).await;
    let id = site(&s).await;
    let first = patch("New site", "First");
    let a = author(&s, id, &first).await;
    let snapshot = json!({"id":Uuid::now_v7(),"type":"snapshot","name":"First"});
    author(&s, id, &snapshot).await;
    let second = patch("First", "Second");
    author(&s, id, &second).await;
    // Simulate activation having committed while the final authoring receipt
    // write was lost. Existing activation receipt must survive a later rollback.
    let mut db = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(root.path().join("service.sqlite")),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE authoring_operations SET response=NULL WHERE site_id=? AND id=?")
        .bind(id.to_string())
        .bind(second["id"].as_str().unwrap())
        .execute(&mut db)
        .await
        .unwrap();
    db.close().await.unwrap();
    author(
        &s,
        id,
        &json!({"id":Uuid::now_v7(),"type":"restore","snapshot_id":snapshot["id"]}),
    )
    .await;
    s.child.kill().unwrap();
    s.child.wait().unwrap();
    drop(s);
    let s = Server::start(root.path()).await;
    assert_eq!(author(&s, id, &second).await["status"], "succeeded");
    assert_eq!(source(&s, id).await["releaseId"], a["releaseId"]);
    assert_eq!(author(&s, id, &first).await, a);
}
#[tokio::test]
async fn simultaneous_sessions_can_edit_different_files_without_a_session_lock() {
    let root = TempDir::new().unwrap();
    let s = Server::start(root.path()).await;
    let id = site(&s).await;
    let current = source(&s, id).await;
    let html = current["files"]["frontend"].as_str().unwrap();
    let edit = json!({"id":Uuid::now_v7(),"type":"patch","patch":format!("*** Begin Patch\n*** Update File: index.html\n@@\n-{}\n+{}\n*** End Patch",html.lines().last().unwrap(),html.lines().last().unwrap().replace("New site","Updated"))});
    let backend = patch("New site", "Other session");
    let (a, b) = tokio::join!(author(&s, id, &edit), author(&s, id, &backend));
    assert_eq!(a["status"], "succeeded");
    assert_eq!(b["status"], "succeeded");
    let files = source(&s, id).await;
    assert!(
        files["files"]["frontend"]
            .as_str()
            .unwrap()
            .contains("Updated")
    );
    assert!(
        files["files"]["backend"]
            .as_str()
            .unwrap()
            .contains("Other session")
    );
}

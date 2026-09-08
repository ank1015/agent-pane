use super::*;
use platform_agent_code_mode::{
    AgentCodeMode,
    code_mode::{CellStatus, Input},
};
use platform_runtime_client::{ClientConfig, PlatformClient, RunClient, WorkerRegistration};
use tokio::sync::watch;
pub(super) async fn setup(f: &Fixture) {
    let activated = f
        .client
        .put(format!(
            "{}/internal/sites/{}/active-release",
            f.sites_url, f.site
        ))
        .bearer_auth(SERVICE)
        .header("Idempotency-Key", "authoring-setup")
        .json(&json!({"release_id":f.release,"expected_generation":0}))
        .send()
        .await
        .unwrap();
    assert!(activated.status().is_success());
    sqlx::query("insert into harnesses(id,name,default_config,config_schema) values('sites','Sites',$1,$2) on conflict(id) do update set default_config=excluded.default_config,config_schema=excluded.config_schema").bind(json!({"model":{"provider":"openai","id":"test-model"}})).bind(json!({"type":"object","properties":{"siteId":{"type":["string","null"]},"model":{"type":"object"},"account_id":{"type":"string"}},"additionalProperties":false})).execute(&f.pool).await.unwrap();
    sqlx::query("update harnesses set supported_models=$1 where id='sites'")
        .bind(json!({"openai":["test-model"]}))
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into project_harnesses(project_id,harness_id,enabled) values($1,'sites',true)",
    )
    .bind(f.project)
    .execute(&f.pool)
    .await
    .unwrap();
    f.request(Method::PUT,&format!("/internal/site-access/{}",f.project),ADMIN,json!({"token":TOKEN,"enabled":true,"harnesses":[{"id":"sites","configurableFields":["model","siteId"]},{"id":HARNESS,"environmentMode":"single"}],"accountIds":[f.account]}),200).await;
}
pub(super) async fn worker(f: &mut Fixture) -> (PlatformClient, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = platform_server::runtime::worker_router(f.runtime.clone(), ADMIN);
    f.tasks.push(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let client =
        PlatformClient::new(&url, Uuid::now_v7(), SERVICE, ClientConfig::default()).unwrap();
    client
        .register(
            ADMIN,
            &WorkerRegistration {
                build_id: "sites-authoring".into(),
                supported_harnesses: vec!["sites".into(), HARNESS.into()],
                capacity: 10,
            },
        )
        .await
        .unwrap();
    (client, url)
}
async fn session(f: &Fixture, client: &PlatformClient, site: Option<Uuid>) -> RunClient {
    let started=f.sdk("sessions.create",json!([{"harnessId":"sites","accountId":f.account,"config":{"siteId":site},"initialInput":{"role":"user","id":Uuid::now_v7().to_string(),"timestamp":1,"content":[{"type":"text","content":"Build"}]}},{"idempotencyKey":Uuid::now_v7().to_string()}])).await;
    assert_eq!(started["status"], 200, "{started}");
    super::capabilities::claim(
        client,
        serde_json::from_value(started["body"]["run"]["id"].clone()).unwrap(),
    )
    .await
}
#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires PostgreSQL and built sites-service/code-mode-runtime binaries"]
async fn sites_authoring_code_mode_live_edits_user_rollback_and_scope(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    setup(&f).await;
    let (client, _) = worker(&mut f).await;
    let first = session(&f, &client, Some(f.site)).await;
    let second = session(&f, &client, Some(f.site)).await;
    // Explicit selections appear before the first authoring call, including
    // concurrent conversations. Other project credentials cannot enumerate them.
    let chats = f
        .request(
            Method::GET,
            &format!("/api/projects/{}/sites/{}/sessions", f.project, f.site),
            TOKEN,
            json!(null),
            200,
        )
        .await;
    assert_eq!(chats["items"].as_array().unwrap().len(), 2);
    f.request(
        Method::GET,
        &format!("/api/projects/{}/sites/{}/sessions", f.other, f.site),
        TOKEN,
        json!(null),
        401,
    )
    .await;
    let runtime = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/code-mode-runtime");
    let mode = AgentCodeMode::new(first.clone(), runtime)
        .unwrap()
        .with_sites()
        .unwrap();
    let (_send, signals) = watch::channel(Default::default());
    let patch = "*** Begin Patch\n*** Update File: index.html\n@@\n-<h1>Sites</h1>\n+<h1>Live editor</h1>\n*** End Patch";
    let cell=mode.execute(Input{id:Uuid::now_v7(),source:format!("const before=await ctx.sites.read(); const result=await ctx.sites.applyPatch({{patch:{}}}); return {{before:before.siteId,result,hasSnapshots:typeof ctx.sites.snapshots}};",json!(patch))},signals.clone()).await.unwrap();
    assert_eq!(cell.status, CellStatus::Completed, "{cell:?}");
    let diagnostics = f
        .request(
            Method::GET,
            &format!("/api/projects/{}/sites/{}/diagnostics", f.project, f.site),
            TOKEN,
            json!(null),
            200,
        )
        .await;
    assert!(diagnostics["items"].is_array());
    f.request(
        Method::GET,
        &format!("/api/projects/{}/sites/{}/diagnostics", f.other, f.site),
        TOKEN,
        json!(null),
        401,
    )
    .await;
    let first_cell_id = cell.id;
    let v = cell.value.unwrap();
    assert_eq!(v["before"], f.site.to_string());
    assert_eq!(v["result"]["status"], "succeeded");
    assert_eq!(v["hasSnapshots"], "undefined");
    let seen = second
        .platform()
        .call("sites.read", json!({}))
        .await
        .unwrap();
    assert!(
        seen["files"]["frontend"]
            .as_str()
            .unwrap()
            .contains("Live editor")
    );
    assert!(
        second
            .platform()
            .call("sites.read", json!({"siteId":f.other}))
            .await
            .is_err()
    );
    assert!(
        second
            .platform()
            .call("sites.snapshots.create", json!({}))
            .await
            .is_err()
    );
    let snapshot = Uuid::now_v7();
    let base = format!("/api/projects/{}/sites/{}", f.project, f.site);
    f.request(
        Method::POST,
        &format!("{base}/snapshots"),
        TOKEN,
        json!({"id":snapshot,"name":"Keep live editor"}),
        200,
    )
    .await;
    let result=second.platform().call("sites.applyPatch",json!({"input":{"patch":"*** Begin Patch\n*** Update File: index.html\n@@\n-<h1>Live editor</h1>\n+<h1>Second agent</h1>\n*** End Patch"},"options":{"idempotencyKey":"second-edit"}})).await.unwrap();
    assert_eq!(result["status"], "succeeded");
    f.request(
        Method::POST,
        &format!("{base}/snapshots/{snapshot}/restore"),
        TOKEN,
        json!({"id":Uuid::now_v7()}),
        200,
    )
    .await;
    let cell=mode.execute(Input{id:Uuid::now_v7(),source:"await ctx.sites.execute({sql:'INSERT INTO flags(id) VALUES (?)',params:['authoring']},{idempotencyKey:'write-once'}); return await ctx.sites.query({sql:'SELECT id FROM flags'});".into()},signals).await.unwrap();
    assert_eq!(cell.status, CellStatus::Completed, "{cell:?}");
    assert_eq!(cell.value.unwrap(), json!([{"id":"authoring"}]));
    // A normal backend has no authoring namespace.
    assert_eq!(f.sdk("sites.read", json!([])).await["status"], 400);
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(first.lease().run_id)
    .execute(&f.pool)
    .await
    .unwrap();
    assert!(
        first
            .platform()
            .call("sites.read", json!({}))
            .await
            .is_err()
    );
    let (replacement_client, _) = worker(&mut f).await;
    let replacement = super::capabilities::claim(&replacement_client, first.lease().run_id).await;
    let replay=replacement.platform().call("sites.applyPatch",json!({"input":{"patch":patch},"options":{"idempotencyKey":format!("cm:{first_cell_id}:2")}})).await.unwrap();
    assert_eq!(
        replay, v["result"],
        "replacement recovers receipt, not patch source"
    );
    sqlx::query("update site_project_access set enabled=false where project_id=$1")
        .bind(f.project)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(
        second
            .platform()
            .call("sites.read", json!({}))
            .await
            .is_err()
    );
}
#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires PostgreSQL and built sites-service binary"]
async fn sites_authoring_null_binding_is_durable_and_foreign_sites_are_rejected(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    setup(&f).await;
    let (client, _) = worker(&mut f).await;
    let run = session(&f, &client, None).await;
    // Provisioning may be asynchronous; all attempts resolve the same binding.
    let mut resolved = None;
    for _ in 0..100 {
        if let Ok(value) = run.platform().call("sites.read", json!({})).await {
            resolved = Some(value);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let value = resolved.expect("site ready");
    let again = run.platform().call("sites.read", json!({})).await.unwrap();
    assert_eq!(value, again);
    assert_ne!(value["siteId"], f.site.to_string());
    let count:i64=sqlx::query_scalar("select count(*) from site_authoring_bindings where session_id=(select session_id from runs where id=$1)").bind(run.lease().run_id).fetch_one(&f.pool).await.unwrap();
    assert_eq!(count, 1);
    let session_id: Uuid = sqlx::query_scalar("select session_id from runs where id=$1")
        .bind(run.lease().run_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    let projection = f
        .request(
            Method::GET,
            &format!("/api/sessions/{session_id}"),
            TOKEN,
            json!(null),
            200,
        )
        .await;
    assert_eq!(projection["site_id"], value["siteId"]);
    assert!(
        projection["config"]["siteId"].is_null(),
        "binding must not mutate frozen config"
    );
    let chats = f
        .request(
            Method::GET,
            &format!(
                "/api/projects/{}/sites/{}/sessions",
                f.project,
                value["siteId"].as_str().unwrap()
            ),
            TOKEN,
            json!(null),
            200,
        )
        .await;
    assert_eq!(chats["items"].as_array().unwrap().len(), 1);
    assert_eq!(chats["items"][0]["id"], session_id.to_string());
    // No unrelated explicitly selected sessions leak into the new site's list.
    let original = f
        .request(
            Method::GET,
            &format!("/api/projects/{}/sites/{}/sessions", f.project, f.site),
            TOKEN,
            json!(null),
            200,
        )
        .await;
    assert!(original["items"].as_array().unwrap().is_empty());
    let foreign = Uuid::now_v7();
    sqlx::query("insert into project_sites(id,project_id,name) values($1,$2,'Foreign')")
        .bind(foreign)
        .bind(f.other)
        .execute(&f.pool)
        .await
        .unwrap();
    let run = session(&f, &client, Some(foreign)).await;
    assert!(run.platform().call("sites.read", json!({})).await.is_err());
}

//! Real Platform/Postgres, leased adapters and both isolated JavaScript guests.
use super::*;
use futures_util::future::BoxFuture;
use platform_agent_code_mode::{AgentCodeMode, PlatformDispatcher};
use platform_runtime_client::{ClientConfig, PlatformClient, RunClient, WorkerRegistration};
use std::path::PathBuf;
use tokio::sync::{Notify, watch};
use tool_code_mode::{Call, CellStatus, Dispatcher, Engine, Input, Tool, ToolError};

const HELPER: &str =
    include_str!("../../../../packages/platform-javascript-sdk/examples/project-helper.js");
fn runtime() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/code-mode-runtime")
}
fn ok(value: Value) -> Value {
    assert_eq!(value["status"], 200, "{value}");
    value["body"].clone()
}
async fn source(f: &mut Fixture) -> (RunClient, String) {
    let started = ok(f
        .sdk(
            "sessions.start",
            json!([f.start(),{"idempotencyKey":"code-source"}]),
        )
        .await);
    let (client, url) = super::capabilities::worker(f).await;
    let run = super::capabilities::claim(
        &client,
        serde_json::from_value(started["runId"].clone()).unwrap(),
    )
    .await;
    (run, url)
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires PostgreSQL and built sites-service/code-mode-runtime binaries"]
async fn code_mode_shared_helper_and_partial_failure_preserve_accepted_operations(pool: PgPool) {
    let backend = format!(
        "{HELPER}\nexport default async (r,ctx)=>{{if(r.body.method==='helper')return {{status:200,body:await inspectProject(ctx.platform)}}; if(r.body.method==='createHelper')return {{status:200,body:await createTrial(ctx.platform,...r.body.args)}}; const [group,name]=r.body.method.split('.');return {{status:200,body:await ctx.platform[group][name](...r.body.args)}};}}"
    );
    let mut f = Fixture::with_code(pool, Some(&backend)).await;
    let (run, _) = source(&mut f).await;
    let mode = AgentCodeMode::new(run, runtime()).unwrap();
    let (_send, signals) = watch::channel(Default::default());
    let input = Input {
        id: Uuid::now_v7(),
        source: format!("{HELPER}\nreturn await inspectProject(ctx.platform);"),
    };
    let cell = mode.execute(input, signals.clone()).await.unwrap();
    assert_eq!(cell.status, CellStatus::Completed, "{cell:?}");
    assert_eq!(cell.value.unwrap(), ok(f.sdk("helper", json!([])).await));

    super::capabilities::declared(&f).await;
    let mut create = json!({"harnessId":HARNESS,"accountId":f.account,"title":"Shared helper","config":{"model":{"provider":"openai","id":"test-model"}}});
    let helper_input = Input {
        id: Uuid::now_v7(),
        source: format!(
            "{HELPER}\nreturn await createTrial(ctx.platform, {create}, 'shared-helper');"
        ),
    };
    let helper_cell = mode.execute(helper_input, signals.clone()).await.unwrap();
    assert_eq!(helper_cell.status, CellStatus::Completed, "{helper_cell:?}");
    let backend_created = ok(f
        .sdk("createHelper", json!([create, "shared-helper"]))
        .await);
    assert_eq!(
        helper_cell.value.unwrap()["session"]["config"],
        backend_created["session"]["config"]
    );
    assert_eq!(
        ok(f.sdk("createHelper", json!([create, "shared-helper"]))
            .await),
        backend_created
    );
    create["title"] = json!("Cell effect");
    let input = Input {
        id: Uuid::now_v7(),
        source: format!(
            "const accepted=await ctx.platform.sessions.create({create}); text({{sessionId:accepted.session.id}}); throw new Error('after acceptance');"
        ),
    };
    let cell = mode.execute(input.clone(), signals.clone()).await.unwrap();
    assert_eq!(cell.status, CellStatus::Failed, "{cell:?}");
    assert_eq!(cell.error.as_ref().unwrap().message, "after acceptance");
    let child: Uuid = serde_json::from_value(cell.output[0]["sessionId"].clone()).unwrap();
    assert_eq!(
        ok(f.sdk("sessions.get", json!([child])).await)["title"],
        "Cell effect"
    );
    let trace = Engine::inspect(&mode.journal, input.id, None, 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trace.calls.items[0].value["status"], "succeeded");
    assert_eq!(
        trace.calls.items[0].value["input"]["options"]["idempotencyKey"],
        format!("cm:{}:1", input.id)
    );
    let saved = serde_json::to_string(&trace).unwrap();
    for credential in [ADMIN, TOKEN, SERVICE, CAP] {
        assert!(!saved.contains(credential));
    }
    assert_eq!(
        mode.execute(input, signals).await.unwrap().status,
        CellStatus::Failed
    );
    let count: i64 = sqlx::query_scalar(
        "select count(*) from sessions where project_id=$1 and title='Cell effect'",
    )
    .bind(f.project)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

struct Hold {
    platform: PlatformDispatcher,
    accepted: Notify,
}
impl Dispatcher for Hold {
    fn prepare(
        &self,
        tool: &Tool,
        key: &str,
        input: Value,
    ) -> std::result::Result<Value, ToolError> {
        self.platform.prepare(tool, key, input)
    }
    fn invoke<'a>(
        &'a self,
        call: &'a Call,
    ) -> BoxFuture<'a, std::result::Result<Value, ToolError>> {
        Box::pin(async move {
            let result = self.platform.invoke(call).await;
            assert!(result.is_ok(), "{result:?}");
            self.accepted.notify_one();
            std::future::pending::<()>().await;
            result
        })
    }
}
#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires PostgreSQL and built sites-service/code-mode-runtime binaries"]
async fn code_mode_takeover_reconciles_saved_call_without_replaying_cell(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let (run, url) = source(&mut f).await;
    let source = run.lease().run_id;
    let mode = AgentCodeMode::new(run.clone(), runtime()).unwrap();
    let hold = Hold {
        platform: mode.dispatcher.clone(),
        accepted: Notify::new(),
    };
    let (_send, cancel) = watch::channel(false);
    let create = json!({"harnessId":HARNESS,"accountId":f.account,"title":"One accepted mutation","config":{"environment":{"type":"machine","machine_id":Uuid::new_v4(),"workspace_root":"/work","path":"."},"model":{"provider":"openai","id":"test-model"}}});
    let input = Input {
        id: Uuid::now_v7(),
        source: format!(
            "await ctx.platform.sessions.create({create}); throw new Error('source must never run again');"
        ),
    };
    {
        let work = mode
            .engine
            .execute(input.clone(), &mode.registry, &mode.journal, &hold, cancel);
        tokio::pin!(work);
        tokio::select! {_=hold.accepted.notified()=>{},r=&mut work=>panic!("unexpected completion {r:?}"),_=tokio::time::sleep(Duration::from_secs(10))=>panic!("mutation not accepted")}
    }
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(source)
    .execute(&f.pool)
    .await
    .unwrap();
    let client =
        PlatformClient::new(&url, Uuid::new_v4(), SERVICE, ClientConfig::default()).unwrap();
    client
        .register(
            ADMIN,
            &WorkerRegistration {
                build_id: "code-replacement".into(),
                supported_harnesses: vec![HARNESS.into()],
                capacity: 10,
            },
        )
        .await
        .unwrap();
    let replacement = super::capabilities::claim(&client, source).await;
    let mode = AgentCodeMode::new(replacement, runtime()).unwrap();
    let (_send, signals) = watch::channel(Default::default());
    assert_eq!(
        mode.execute(input.clone(), signals).await.unwrap().status,
        CellStatus::Interrupted
    );
    let call = mode.reconcile_call(input.id, 1).await.unwrap();
    assert_eq!(call.status, tool_code_mode::CallStatus::Succeeded);
    let count: i64 = sqlx::query_scalar(
        "select count(*) from sessions where project_id=$1 and title='One accepted mutation'",
    )
    .bind(f.project)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    assert!(
        run.platform().get_run(source).await.is_err(),
        "stale lease has no new authority"
    );
}

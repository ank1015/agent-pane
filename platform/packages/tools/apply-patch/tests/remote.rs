use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
use execution_client::{ExecutionClient, ExecutionClientConfig, GatewayHostRuntime};
use execution_core::*;
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use execution_wire::{Operation, RequestEnvelope, dispatch_request};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::RwLock;
use tool_apply_patch::{ApplyPatchConfig, ApplyPatchState, ApplyPatchTool, RunningPatch};

struct Host {
    temp: tempfile::TempDir,
    config: SupervisorConfig,
    target: Arc<RwLock<Arc<SupervisorRuntime>>>,
    client: GatewayHostRuntime,
    lose_reply: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Host {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work");
        std::fs::create_dir(&root).unwrap();
        let config = SupervisorConfig {
            host_id: ExecutionHostId::generate(),
            state_directory: temp.path().join("state"),
            roots: vec![SupervisorRoot {
                id: RootId::new("work").unwrap(),
                name: "Work".into(),
                path: root,
                read_only: false,
            }],
            limits: SupervisorLimits::default(),
        };
        let target = Arc::new(RwLock::new(Arc::new(
            SupervisorRuntime::new(config.clone()).await.unwrap(),
        )));
        let server_target = target.clone();
        let lose_reply = Arc::new(AtomicBool::new(false));
        let drop_reply = lose_reply.clone();
        let app = Router::new().route(
            "/v1/hosts/{host}/operations",
            post(move |Json(request): Json<RequestEnvelope>| {
                let target = server_target.clone();
                let drop_reply = drop_reply.clone();
                async move {
                    let mutation = matches!(
                        request.operation,
                        Operation::FilesystemWrite(_) | Operation::FilesystemRemove(_)
                    );
                    let target = target.read().await.clone();
                    let reply =
                        dispatch_request(target.as_ref(), &OperationContext::new(), request).await;
                    if mutation && drop_reply.swap(false, Ordering::SeqCst) {
                        (StatusCode::SERVICE_UNAVAILABLE, "reply lost after mutation")
                            .into_response()
                    } else {
                        Json(reply).into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut client_config =
            ExecutionClientConfig::new(url.parse().unwrap(), "apply-patch-test");
        client_config.allow_insecure_http = true;
        let client = ExecutionClient::new(client_config)
            .unwrap()
            .connect_host(&OperationContext::new(), config.host_id.clone())
            .await
            .unwrap();
        Self {
            temp,
            config,
            target,
            client,
            lose_reply,
            server,
        }
    }
    fn path(&self, path: &str) -> std::path::PathBuf {
        self.temp.path().join("work").join(path)
    }
    fn tool(&self) -> ApplyPatchTool<'_> {
        ApplyPatchTool::new(
            &self.client,
            ExecutionPath::root(RootId::new("work").unwrap()),
            ApplyPatchConfig::default(),
        )
        .unwrap()
    }
    async fn prepare(&self, patch: &str) -> RunningPatch {
        let tool = self.tool();
        let prepared = tool
            .prepare(
                &OperationContext::new(),
                patch,
                ApplyPatchState {
                    operation_id: OperationId::generate(),
                },
            )
            .await
            .unwrap();
        tool.start(&prepared).unwrap()
    }
}

#[tokio::test]
async fn lost_write_and_remove_replies_replay_persisted_requests_over_http() {
    let host = Host::new().await;
    let context = OperationContext::new();
    std::fs::write(host.path("a"), "old\n").unwrap();
    let mut running = host
        .prepare(
            "*** Begin Patch\n*** Update File: a\n*** Move to: b\n@@\n-old\n+new\n*** End Patch",
        )
        .await;
    let tool = host.tool();
    tool.step(&context, &mut running).await.unwrap();
    let saved = serde_json::to_vec(&running).unwrap();
    host.lose_reply.store(true, Ordering::SeqCst);
    tool.step(&context, &mut running).await.unwrap_err();
    assert_eq!(std::fs::read(host.path("b")).unwrap(), b"new\n");
    std::fs::write(host.path("b"), "external\n").unwrap();
    let mut running: RunningPatch = serde_json::from_slice(&saved).unwrap();
    tool.step(&context, &mut running).await.unwrap();
    assert_eq!(std::fs::read(host.path("b")).unwrap(), b"external\n");
    let saved = serde_json::to_vec(&running).unwrap();
    host.lose_reply.store(true, Ordering::SeqCst);
    tool.step(&context, &mut running).await.unwrap_err();
    assert!(!host.path("a").exists());
    std::fs::write(host.path("a"), "recreated\n").unwrap();
    let mut running: RunningPatch = serde_json::from_slice(&saved).unwrap();
    tool.apply(&context, &mut running).await.unwrap();
    assert_eq!(std::fs::read(host.path("a")).unwrap(), b"recreated\n");
}

#[tokio::test]
async fn supervisor_restart_rejects_mutation_with_stale_http_descriptor() {
    let host = Host::new().await;
    let context = OperationContext::new();
    for patch in [
        "*** Begin Patch\n*** Add File: a\n+new\n*** End Patch",
        "*** Begin Patch\n*** Delete File: a\n*** End Patch",
    ] {
        std::fs::write(host.path("a"), "external\n").unwrap();
        let mut running = host.prepare(patch).await;
        host.tool().step(&context, &mut running).await.unwrap();
        let restarted = Arc::new(SupervisorRuntime::new(host.config.clone()).await.unwrap());
        assert_ne!(
            host.client.descriptor().supervisor_generation_id,
            restarted.descriptor().supervisor_generation_id
        );
        *host.target.write().await = restarted;
        let failure = host.tool().step(&context, &mut running).await.unwrap_err();
        assert_eq!(failure.code, ExecutionErrorCode::ExecutionLost);
        assert_eq!(std::fs::read(host.path("a")).unwrap(), b"external\n");
    }
}

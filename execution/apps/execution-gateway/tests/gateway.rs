use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_api::{DesiredHostState, E2bAccount, E2bSnapshot, ExecutionHost, ExecutionHostState};
use execution_core::{
    ExecutionFeatures, ExecutionHostDescriptor, ExecutionHostId, ExecutionLimits, ExecutionRoot,
    OperatingSystem, PathConvention, RootId, SupervisorGenerationId,
};
use execution_e2b::E2bError;
use execution_gateway::{
    AppState, CredentialVault, Database, DynE2bProvider, E2bProvider, HostConnections, HostFailure,
    LifecycleReconciler, ProviderExecutionResponse, ProviderHostDetails, ProviderHostState,
    SecurityControls, router,
};
use execution_wire::{Operation, OperationResult, RequestEnvelope, RequestId, ResponseEnvelope};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::net::TcpListener;
use uuid::Uuid;

#[derive(Default)]
struct FakeProvider {
    snapshot_sources: Mutex<Vec<String>>,
}

#[async_trait]
impl E2bProvider for FakeProvider {
    async fn verify_credentials(&self, api_key: &str) -> Result<(), E2bError> {
        assert_eq!(api_key, "e2b-secret");
        Ok(())
    }

    async fn create_base(&self, _api_key: &str, _timeout_seconds: u64) -> Result<String, E2bError> {
        Ok(format!("sandbox-{}", Uuid::now_v7()))
    }

    async fn create_from_snapshot(
        &self,
        _api_key: &str,
        snapshot_id: &str,
        _timeout_seconds: u64,
    ) -> Result<String, E2bError> {
        self.snapshot_sources
            .lock()
            .unwrap()
            .push(snapshot_id.to_owned());
        Ok(format!("sandbox-from-{snapshot_id}"))
    }

    async fn get_host(
        &self,
        _api_key: &str,
        _sandbox_id: &str,
        _timeout_seconds: u64,
    ) -> Result<ProviderHostDetails, E2bError> {
        Ok(ProviderHostDetails {
            state: ProviderHostState::Running,
        })
    }

    async fn pause_host(
        &self,
        _api_key: &str,
        _sandbox_id: &str,
        _timeout_seconds: u64,
    ) -> Result<(), E2bError> {
        Ok(())
    }

    async fn resume_host(
        &self,
        _api_key: &str,
        _sandbox_id: &str,
        _timeout_seconds: u64,
    ) -> Result<(), E2bError> {
        Ok(())
    }

    async fn delete_host(
        &self,
        _api_key: &str,
        _sandbox_id: &str,
        _timeout_seconds: u64,
    ) -> Result<(), E2bError> {
        Ok(())
    }

    async fn create_snapshot(
        &self,
        _api_key: &str,
        _sandbox_id: &str,
        _timeout_seconds: u64,
    ) -> Result<String, E2bError> {
        Ok(format!("snapshot-{}", Uuid::now_v7()))
    }

    async fn delete_snapshot(&self, _api_key: &str, _snapshot_id: &str) -> Result<(), E2bError> {
        Ok(())
    }

    async fn execute(
        &self,
        _api_key: &str,
        _sandbox_id: &str,
        host_id: ExecutionHostId,
        _timeout_seconds: u64,
        request: RequestEnvelope,
    ) -> Result<ProviderExecutionResponse, E2bError> {
        let descriptor = descriptor(host_id);
        Ok(ProviderExecutionResponse {
            descriptor: descriptor.clone(),
            response: ResponseEnvelope::success(
                request.request_id,
                OperationResult::HostDescriptor(descriptor),
            ),
        })
    }
}

#[tokio::test]
#[ignore = "requires EXECUTION_GATEWAY_TEST_DATABASE_URL pointing to PostgreSQL"]
async fn account_host_snapshot_and_execution_flow() {
    let database_url = std::env::var("EXECUTION_GATEWAY_TEST_DATABASE_URL")
        .expect("EXECUTION_GATEWAY_TEST_DATABASE_URL");
    let database = Database::connect(&database_url, 5).await.unwrap();
    database.migrate().await.unwrap();
    let inspection = PgPool::connect(&database_url).await.unwrap();
    sqlx::query(
        "truncate table idempotency_records, e2b_hosts, e2b_snapshots,
                        execution_hosts, e2b_accounts cascade",
    )
    .execute(&inspection)
    .await
    .unwrap();

    let vault = CredentialVault::from_base64(&STANDARD.encode([9_u8; 32])).unwrap();
    let fake = Arc::new(FakeProvider::default());
    let provider: DynE2bProvider = fake.clone();
    let reconciler = LifecycleReconciler::new(database.clone(), vault.clone(), provider.clone());
    let app = router(
        AppState {
            database: database.clone(),
            vault,
            provider,
            reconciler,
            api_token: Arc::from("gateway-token"),
            default_host_timeout_seconds: 300,
            public_base_url: "http://127.0.0.1/".parse().unwrap(),
            registration_token_ttl: Duration::from_secs(900),
            heartbeat_interval: Duration::from_secs(15),
            connections: HostConnections::new(Duration::from_secs(60), 64),
            security: SecurityControls::new(32, 30, 60),
        },
        16 * 1024 * 1024,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{address}/v1");
    let client = Client::new();

    let account_response = client
        .post(format!("{base}/e2b-accounts"))
        .bearer_auth("gateway-token")
        .json(&json!({"name":"personal", "api_key":"e2b-secret", "is_default":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(account_response.status(), StatusCode::CREATED);
    let account: E2bAccount = account_response.json().await.unwrap();
    assert!(account.is_default);

    let ciphertext: Vec<u8> =
        sqlx::query_scalar("select credential_ciphertext from e2b_accounts where id = $1")
            .bind(account.id)
            .fetch_one(&inspection)
            .await
            .unwrap();
    assert!(
        !ciphertext
            .windows(b"e2b-secret".len())
            .any(|value| value == b"e2b-secret")
    );

    let arbitrary_template = client
        .post(format!("{base}/hosts"))
        .bearer_auth("gateway-token")
        .header("Idempotency-Key", "bad-template")
        .json(&json!({"source":{"type":"base", "template_id":"arbitrary"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        arbitrary_template.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let create_body = json!({
        "name": "base-host",
        "source": {"type": "base", "e2b_account_id": account.id},
        "timeout_seconds": 300
    });
    let first = create_host(&client, &base, "create-base", &create_body).await;
    let replay = create_host(&client, &base, "create-base", &create_body).await;
    assert_eq!(first.id, replay.id);
    let host = wait_for_host(&client, &base, first.id, ExecutionHostState::Ready).await;
    assert!(host.descriptor.is_some());

    let operation = RequestEnvelope::new(RequestId::generate(), Operation::Describe);
    let operation_response = client
        .post(format!("{base}/hosts/{}/operations", host.id))
        .bearer_auth("gateway-token")
        .json(&operation)
        .send()
        .await
        .unwrap();
    assert_eq!(operation_response.status(), StatusCode::OK);
    let wire: ResponseEnvelope = operation_response.json().await.unwrap();
    assert!(matches!(wire, ResponseEnvelope::Success { .. }));

    let snapshot_response = client
        .post(format!("{base}/hosts/{}/snapshots", host.id))
        .bearer_auth("gateway-token")
        .header("Idempotency-Key", "snapshot-base")
        .json(&json!({"name":"installed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(snapshot_response.status(), StatusCode::ACCEPTED);
    let snapshot: E2bSnapshot = snapshot_response.json().await.unwrap();
    let snapshot = wait_for_snapshot(&client, &base, snapshot.id).await;
    let provider_snapshot_id = snapshot.e2b_snapshot_id.clone().unwrap();
    wait_for_host(&client, &base, host.id, ExecutionHostState::Paused).await;

    // A lost creation response must be replayable after snapshotting paused the
    // source. A different body must still conflict; a new snapshot needs ready.
    let replay = client
        .post(format!("{base}/hosts/{}/snapshots", host.id))
        .bearer_auth("gateway-token")
        .header("Idempotency-Key", "snapshot-base")
        .json(&json!({"name":"installed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    assert_eq!(replay.json::<E2bSnapshot>().await.unwrap().id, snapshot.id);
    for (key, name, code) in [
        ("snapshot-base", "changed", "IDEMPOTENCY_KEY_REUSED"),
        ("new-snapshot", "installed", "HOST_NOT_READY"),
    ] {
        let response = client
            .post(format!("{base}/hosts/{}/snapshots", host.id))
            .bearer_auth("gateway-token")
            .header("Idempotency-Key", key)
            .json(&json!({"name":name}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            code
        );
    }
    // Describe is the SDK's connect operation. It transparently resumes a paused
    // E2B host; no model-facing resume tool is needed. Concurrent calls coalesce.
    let connect = || {
        client
            .post(format!("{base}/hosts/{}/operations", host.id))
            .bearer_auth("gateway-token")
            .json(&RequestEnvelope::new(
                RequestId::generate(),
                Operation::Describe,
            ))
            .send()
    };
    let (one, two) = tokio::join!(connect(), connect());
    assert_eq!(one.unwrap().status(), StatusCode::OK);
    assert_eq!(two.unwrap().status(), StatusCode::OK);
    wait_for_host(&client, &base, host.id, ExecutionHostState::Ready).await;

    let snapshot_host = create_host(
        &client,
        &base,
        "create-from-snapshot",
        &json!({"source":{"type":"snapshot", "snapshot_id":snapshot.id}}),
    )
    .await;
    wait_for_host(&client, &base, snapshot_host.id, ExecutionHostState::Ready).await;
    assert_eq!(
        fake.snapshot_sources.lock().unwrap().as_slice(),
        &[provider_snapshot_id]
    );

    database
        .request_host_state(snapshot_host.id, "deleted", "deleting")
        .await
        .unwrap();
    database
        .fail_host(
            snapshot_host.id,
            Some(&DesiredHostState::Ready),
            HostFailure {
                observed_state: "lost",
                code: "STALE_READY_FAILURE",
                message: "a superseded ready reconciliation failed",
                retryable: false,
                ambiguous: false,
            },
        )
        .await
        .unwrap();
    let deleting = database
        .host(snapshot_host.id, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deleting.desired_state, DesiredHostState::Deleted);
    assert_eq!(deleting.state, ExecutionHostState::Deleting);
    assert!(deleting.status_code.is_none());

    let response = client
        .post(format!("{base}/hosts/{}/operations", snapshot_host.id))
        .bearer_auth("gateway-token")
        .json(&RequestEnvelope::new(
            RequestId::generate(),
            Operation::Describe,
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        database
            .host(snapshot_host.id, true)
            .await
            .unwrap()
            .unwrap()
            .desired_state,
        DesiredHostState::Deleted
    );

    // Receipts are also independent of a deleted source host.
    database
        .request_host_state(host.id, "deleted", "deleting")
        .await
        .unwrap();
    database.complete_host_deleted(host.id).await.unwrap();
    let replay = client
        .post(format!("{base}/hosts/{}/snapshots", host.id))
        .bearer_auth("gateway-token")
        .header("Idempotency-Key", "snapshot-base")
        .json(&json!({"name":"installed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    assert_eq!(replay.json::<E2bSnapshot>().await.unwrap().id, snapshot.id);
}

async fn create_host(client: &Client, base: &str, key: &str, body: &Value) -> ExecutionHost {
    let response = client
        .post(format!("{base}/hosts"))
        .bearer_auth("gateway-token")
        .header("Idempotency-Key", key)
        .json(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let host: ExecutionHost = response.json().await.unwrap();
    assert_eq!(host.roots.len(), 1);
    assert_eq!(host.roots[0].id.as_str(), "workspace");
    assert_eq!(host.roots[0].native_path, "/home/user");
    assert!(!host.roots[0].read_only);
    host
}

async fn wait_for_host(
    client: &Client,
    base: &str,
    id: Uuid,
    expected: ExecutionHostState,
) -> ExecutionHost {
    for _ in 0..100 {
        let host: ExecutionHost = client
            .get(format!("{base}/hosts/{id}"))
            .bearer_auth("gateway-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if host.state == expected {
            return host;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("host {id} did not reach {expected:?}");
}

async fn wait_for_snapshot(client: &Client, base: &str, id: Uuid) -> E2bSnapshot {
    for _ in 0..100 {
        let snapshot: E2bSnapshot = client
            .get(format!("{base}/snapshots/{id}"))
            .bearer_auth("gateway-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if snapshot.e2b_snapshot_id.is_some() {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("snapshot {id} did not become ready");
}

fn descriptor(host_id: ExecutionHostId) -> ExecutionHostDescriptor {
    ExecutionHostDescriptor {
        host_id,
        supervisor_generation_id: SupervisorGenerationId::generate(),
        operating_system: OperatingSystem::Linux,
        architecture: "x86_64".to_owned(),
        path_convention: PathConvention::Unix,
        roots: vec![ExecutionRoot {
            id: RootId::new("workspace").unwrap(),
            name: "Workspace".to_owned(),
            native_path: "/home/user".to_owned(),
            read_only: false,
        }],
        features: ExecutionFeatures {
            pty: true,
            process_signals: true,
            file_revisions: true,
        },
        limits: ExecutionLimits::default(),
    }
}

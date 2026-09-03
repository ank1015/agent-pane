mod support;

use std::{sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_api::{
    ClaimRegisteredHostResponse, ExecutionHost, ExecutionHostState, GatewayHostMessage,
    HostDaemonMessage, RegisteredHostRegistration,
};
use execution_conformance::{ConformanceConfig, run_all};
use execution_core::{
    CommandSpec, EnvironmentVariables, ExecutionId, ExecutionPath, ExecutionRuntime as _,
    OperationContext, OperationId, ProcessEventKind, ReadExecutionRequest, RootId,
    StartExecutionRequest, StdinMode,
};
use execution_gateway::{
    AppState, CredentialVault, Database, DynE2bProvider, E2bProviderSettings, HostConnections,
    LifecycleReconciler, RealE2bProvider, SecurityControls, router,
};
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use execution_wire::{PROTOCOL_NAME, PROTOCOL_VERSION, dispatch_request};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::json;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::header::AUTHORIZATION},
};
use url::Url;
use uuid::Uuid;

use support::HostedGatewayRuntime;

const API_TOKEN: &str = "registered-host-test-token";

#[tokio::test]
#[ignore = "requires EXECUTION_GATEWAY_TEST_DATABASE_URL pointing to PostgreSQL"]
async fn registered_host_enrollment_routing_conformance_and_reconnect() -> anyhow::Result<()> {
    let database_url = std::env::var("EXECUTION_GATEWAY_TEST_DATABASE_URL")?;
    let database = Database::connect(&database_url, 5).await?;
    database.migrate().await?;
    let inspection = PgPool::connect(&database_url).await?;
    sqlx::query(
        "truncate table idempotency_records, registered_hosts, e2b_hosts, e2b_snapshots,
                        execution_hosts, e2b_accounts cascade",
    )
    .execute(&inspection)
    .await?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let base_url = format!("http://{address}/v1");
    let public_base_url: Url = format!("http://{address}/").parse()?;
    let provider: DynE2bProvider = Arc::new(RealE2bProvider::new(E2bProviderSettings {
        control_base_url: Url::parse("https://api.e2b.app/")?,
        envd_base_url_override: None,
        request_timeout: Duration::from_secs(1),
    }));
    let vault = CredentialVault::from_base64(&STANDARD.encode([17_u8; 32]))?;
    let reconciler = LifecycleReconciler::new(database.clone(), vault.clone(), provider.clone());
    let app = router(
        AppState {
            database: database.clone(),
            vault,
            provider,
            reconciler,
            api_token: Arc::from(API_TOKEN),
            default_host_timeout_seconds: 300,
            public_base_url,
            registration_token_ttl: Duration::from_secs(900),
            heartbeat_interval: Duration::from_millis(100),
            connections: HostConnections::new(Duration::from_secs(20), 64),
            security: SecurityControls::new(32, 30, 60),
        },
        16 * 1024 * 1024,
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = Client::new();

    let registration: RegisteredHostRegistration = decode(
        client
            .post(format!("{base_url}/registered-hosts"))
            .bearer_auth(API_TOKEN)
            .header("Idempotency-Key", "registered-host")
            .json(&json!({"name":"local-test", "metadata":{"purpose":"conformance"}}))
            .send()
            .await?,
    )
    .await?;
    assert_eq!(registration.host.state, ExecutionHostState::Provisioning);
    assert!(registration.host.e2b.is_none());
    assert!(registration.host.registered.is_some());

    let replay: RegisteredHostRegistration = decode(
        client
            .post(format!("{base_url}/registered-hosts"))
            .bearer_auth(API_TOKEN)
            .header("Idempotency-Key", "registered-host")
            .json(&json!({"name":"local-test", "metadata":{"purpose":"conformance"}}))
            .send()
            .await?,
    )
    .await?;
    assert_eq!(replay.host.id, registration.host.id);
    assert_ne!(replay.registration_token, registration.registration_token);

    let installation_id = Uuid::now_v7();
    let expired_claim = client
        .post(format!("{base_url}/registered-hosts/claim"))
        .json(&json!({
            "registration_token": registration.registration_token,
            "installation_id": installation_id,
            "daemon_version": "test"
        }))
        .send()
        .await?;
    assert_eq!(expired_claim.status(), StatusCode::UNAUTHORIZED);

    let claim: ClaimRegisteredHostResponse = decode(
        client
            .post(format!("{base_url}/registered-hosts/claim"))
            .json(&json!({
                "registration_token": replay.registration_token,
                "installation_id": installation_id,
                "daemon_version": "test"
            }))
            .send()
            .await?,
    )
    .await?;
    assert_eq!(claim.host_id, registration.host.id);
    assert!(
        !String::from_utf8_lossy(
            &sqlx::query_scalar::<_, Vec<u8>>(
                "select credential_hash from registered_hosts where host_id = $1",
            )
            .bind(claim.host_id)
            .fetch_one(&inspection)
            .await?
        )
        .contains(&claim.credential)
    );
    let reused_claim = client
        .post(format!("{base_url}/registered-hosts/claim"))
        .json(&json!({
            "registration_token": replay.registration_token,
            "installation_id": installation_id,
            "daemon_version": "test"
        }))
        .send()
        .await?;
    assert_eq!(reused_claim.status(), StatusCode::UNAUTHORIZED);

    let temporary = tempfile::tempdir()?;
    let root_path = temporary.path().join("workspace");
    tokio::fs::create_dir_all(&root_path).await?;
    let runtime = Arc::new(
        SupervisorRuntime::new(SupervisorConfig {
            host_id: execution_core::ExecutionHostId::new(claim.host_id.to_string())?,
            state_directory: temporary.path().join("state"),
            roots: vec![SupervisorRoot {
                id: RootId::new("workspace")?,
                name: "Workspace".to_owned(),
                path: root_path,
                read_only: false,
            }],
            limits: SupervisorLimits::default(),
        })
        .await?,
    );

    let first_daemon = tokio::spawn(run_test_daemon(
        claim.websocket_url.clone(),
        claim.credential.clone(),
        Arc::clone(&runtime),
    ));
    let ready = wait_for_host(&client, &base_url, claim.host_id, ExecutionHostState::Ready).await?;
    let hosted = HostedGatewayRuntime::new(base_url.clone(), API_TOKEN, &ready)?;
    let conformance = ConformanceConfig::for_runtime(&hosted)?;
    let report = run_all(&hosted, &conformance).await?;
    assert!(report.skipped_checks().is_empty());

    let execution_id = ExecutionId::generate();
    let handle = hosted
        .processes()
        .start(
            &OperationContext::with_timeout(Duration::from_secs(10)),
            StartExecutionRequest {
                operation_id: OperationId::generate(),
                execution_id: execution_id.clone(),
                command: CommandSpec::Argv {
                    program: "/bin/sh".to_owned(),
                    arguments: vec![
                        "-c".to_owned(),
                        "printf before; sleep 1; printf after".to_owned(),
                    ],
                },
                cwd: ExecutionPath::root(RootId::new("workspace")?),
                environment: EnvironmentVariables::default(),
                stdin: StdinMode::Pipe,
                timeout_ms: None,
            },
        )
        .await?;
    first_daemon.abort();
    let _ = first_daemon.await;
    wait_for_host(
        &client,
        &base_url,
        claim.host_id,
        ExecutionHostState::Unavailable,
    )
    .await?;

    tokio::time::sleep(Duration::from_millis(1200)).await;
    let second_daemon = tokio::spawn(run_test_daemon(
        claim.websocket_url.clone(),
        claim.credential.clone(),
        Arc::clone(&runtime),
    ));
    wait_for_host(&client, &base_url, claim.host_id, ExecutionHostState::Ready).await?;
    let output = hosted
        .processes()
        .read(
            &OperationContext::with_timeout(Duration::from_secs(10)),
            ReadExecutionRequest {
                execution_id,
                supervisor_generation_id: handle.supervisor_generation_id,
                after_sequence: 0,
                max_bytes: 1024,
                wait_ms: Some(1000),
            },
        )
        .await?;
    let rendered = output
        .events
        .iter()
        .filter_map(|event| match &event.event {
            ProcessEventKind::Output { data, .. } => Some(data),
            _ => None,
        })
        .map(|data| String::from_utf8_lossy(data.as_slice()))
        .collect::<String>();
    assert!(rendered.contains("before"));
    assert!(rendered.contains("after"));

    let unsupported = client
        .post(format!("{base_url}/hosts/{}/pause", claim.host_id))
        .bearer_auth(API_TOKEN)
        .send()
        .await?;
    assert_eq!(unsupported.status(), StatusCode::CONFLICT);

    let deleted: ExecutionHost = decode(
        client
            .delete(format!("{base_url}/hosts/{}", claim.host_id))
            .bearer_auth(API_TOKEN)
            .send()
            .await?,
    )
    .await?;
    assert_eq!(deleted.state, ExecutionHostState::Deleted);
    second_daemon.abort();
    let mut revoked_request = claim.websocket_url.into_client_request()?;
    revoked_request.headers_mut().insert(
        AUTHORIZATION,
        format!("Bearer {}", claim.credential).parse()?,
    );
    assert!(connect_async(revoked_request).await.is_err());
    runtime.shutdown().await;
    server.abort();
    Ok(())
}

async fn run_test_daemon(
    websocket_url: String,
    credential: String,
    runtime: Arc<SupervisorRuntime>,
) -> anyhow::Result<()> {
    let mut request = websocket_url.into_client_request()?;
    request
        .headers_mut()
        .insert(AUTHORIZATION, format!("Bearer {credential}").parse()?);
    let (mut socket, _) = connect_async(request).await?;
    socket
        .send(Message::Text(
            serde_json::to_string(&HostDaemonMessage::Hello {
                protocol_name: PROTOCOL_NAME.to_owned(),
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "test".to_owned(),
                daemon_instance_id: Uuid::now_v7(),
                descriptor: Box::new(runtime.descriptor().clone()),
            })?
            .into(),
        ))
        .await?;
    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(text) => match serde_json::from_str::<GatewayHostMessage>(text.as_str())?
            {
                GatewayHostMessage::Welcome { .. } => {}
                GatewayHostMessage::Request { request } => {
                    let response = dispatch_request(
                        runtime.as_ref(),
                        &OperationContext::with_timeout(Duration::from_secs(20)),
                        *request,
                    )
                    .await;
                    socket
                        .send(Message::Text(
                            serde_json::to_string(&HostDaemonMessage::Response {
                                response: Box::new(response),
                            })?
                            .into(),
                        ))
                        .await?;
                }
                GatewayHostMessage::Disconnect { .. } => break,
            },
            Message::Ping(data) => socket.send(Message::Pong(data)).await?,
            Message::Close(_) => break,
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    Ok(())
}

async fn wait_for_host(
    client: &Client,
    base_url: &str,
    host_id: Uuid,
    expected: ExecutionHostState,
) -> anyhow::Result<ExecutionHost> {
    for _ in 0..100 {
        let host: ExecutionHost = decode(
            client
                .get(format!("{base_url}/hosts/{host_id}"))
                .bearer_auth(API_TOKEN)
                .send()
                .await?,
        )
        .await?;
        if host.state == expected {
            return Ok(host);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("host {host_id} did not reach {expected:?}")
}

async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> anyhow::Result<T> {
    let status = response.status();
    let bytes = response.bytes().await?;
    anyhow::ensure!(
        status.is_success(),
        "gateway returned {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
    Ok(serde_json::from_slice(&bytes)?)
}

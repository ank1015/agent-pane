use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use execution_e2b::{
    E2bApiKey, E2bConfig, E2bControlClient, E2bErrorKind, EXECUTION_BASE_TEMPLATE_1024_MB_ID,
    EXECUTION_BASE_TEMPLATE_2048_MB_ID, EXECUTION_BASE_TEMPLATE_4096_MB_ID,
    EXECUTION_BASE_TEMPLATE_8192_MB_ID, EXECUTION_BASE_TEMPLATE_ID,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use url::Url;

#[tokio::test]
async fn creates_base_and_snapshot_sandboxes_with_the_expected_template_ids() {
    async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(headers["x-api-key"], "secret");
        assert_eq!(body["secure"], true);
        assert_eq!(body["autoPause"], true);
        assert_eq!(body["autoPauseMemory"], true);
        assert_eq!(body["timeout"], 300);
        let template = body["templateID"].as_str().unwrap();
        let expected_network_access = template != "snapshot-42";
        assert_eq!(body["allow_internet_access"], expected_network_access);
        Json(json!({
            "sandboxID": format!("from-{template}"),
            "envdAccessToken": "token",
            "domain": "e2b.app"
        }))
    }

    let base_url = serve(Router::new().route("/sandboxes", post(create))).await;
    let client = client(base_url);

    assert_eq!(
        client.create_base_sandbox(None, None).await.unwrap(),
        format!("from-{EXECUTION_BASE_TEMPLATE_ID}")
    );
    for (ram_mb, template_id) in [
        (1024, EXECUTION_BASE_TEMPLATE_1024_MB_ID),
        (2048, EXECUTION_BASE_TEMPLATE_2048_MB_ID),
        (4096, EXECUTION_BASE_TEMPLATE_4096_MB_ID),
        (8192, EXECUTION_BASE_TEMPLATE_8192_MB_ID),
    ] {
        assert_eq!(
            client
                .create_base_sandbox(None, Some(ram_mb))
                .await
                .unwrap(),
            format!("from-{template_id}")
        );
    }
    assert_eq!(
        client
            .create_snapshot_sandbox("snapshot-42", Some(false))
            .await
            .unwrap(),
        "from-snapshot-42"
    );
}

#[tokio::test]
async fn verifies_credentials_without_mutating_provider_state() {
    use axum::routing::get;

    async fn list(headers: HeaderMap) -> Json<Value> {
        assert_eq!(headers["x-api-key"], "secret");
        Json(json!({"items": [], "nextToken": null}))
    }

    let base_url = serve(Router::new().route("/sandboxes", get(list))).await;
    client(base_url).verify_credentials().await.unwrap();
}

#[tokio::test]
async fn resumes_before_snapshotting_and_returns_the_snapshot_id() {
    #[derive(Clone)]
    struct Sequence(Arc<AtomicUsize>);

    async fn connect(State(state): State<Sequence>, Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(state.0.fetch_add(1, Ordering::SeqCst), 0);
        assert_eq!(body, json!({"timeout": 300, "memory": true}));
        Json(json!({
            "sandboxID": "sandbox-1",
            "envdAccessToken": "fresh-token",
            "domain": "e2b.app"
        }))
    }

    async fn snapshot(State(state): State<Sequence>, Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(state.0.fetch_add(1, Ordering::SeqCst), 1);
        assert_eq!(body, json!({}));
        Json(json!({"snapshotID":"snapshot-1", "names": []}))
    }

    let state = Sequence(Arc::new(AtomicUsize::new(0)));
    let app = Router::new()
        .route("/sandboxes/sandbox-1/connect", post(connect))
        .route("/sandboxes/sandbox-1/snapshots", post(snapshot))
        .with_state(state.clone());
    let client = client(serve(app).await);

    assert_eq!(
        client.snapshot_sandbox("sandbox-1").await.unwrap(),
        "snapshot-1"
    );
    assert_eq!(state.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retries_connect_and_refreshes_envd_credentials() {
    #[derive(Clone)]
    struct Attempts(Arc<AtomicUsize>);

    async fn connect(State(attempts): State<Attempts>) -> impl IntoResponse {
        let attempt = attempts.0.fetch_add(1, Ordering::SeqCst);
        if attempt < 2 {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"message":"sandbox is resuming"})),
            );
        }
        (
            StatusCode::OK,
            Json(json!({
                "sandboxID":"sandbox-1",
                "envdAccessToken":"fresh-token",
                "domain":"test.invalid"
            })),
        )
    }

    let attempts = Attempts(Arc::new(AtomicUsize::new(0)));
    let app = Router::new()
        .route("/sandboxes/sandbox-1/connect", post(connect))
        .with_state(attempts.clone());
    let mut config = config(serve(app).await);
    config.retry.initial_delay = Duration::from_millis(1);
    config.retry.max_delay = Duration::from_millis(1);
    let client = E2bControlClient::new(config).unwrap();

    let connected = client.ensure_connected("sandbox-1").await.unwrap();
    assert_eq!(connected.sandbox_id, "sandbox-1");
    assert_eq!(connected.domain.as_deref(), Some("test.invalid"));
    assert_eq!(attempts.0.load(Ordering::SeqCst), 3);
    assert!(!format!("{connected:?}").contains("fresh-token"));
}

#[tokio::test]
async fn does_not_replay_ambiguous_create_requests() {
    #[derive(Clone)]
    struct Attempts(Arc<AtomicUsize>);

    async fn unavailable(State(attempts): State<Attempts>) -> impl IntoResponse {
        attempts.0.fetch_add(1, Ordering::SeqCst);
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"message":"try again"})),
        )
    }

    let attempts = Attempts(Arc::new(AtomicUsize::new(0)));
    let app = Router::new()
        .route("/sandboxes", post(unavailable))
        .with_state(attempts.clone());
    let client = client(serve(app).await);

    let error = client.create_base_sandbox(None, None).await.unwrap_err();
    assert_eq!(attempts.0.load(Ordering::SeqCst), 1);
    assert_eq!(error.kind, E2bErrorKind::Unavailable);
    assert!(error.outcome_ambiguous);
    assert!(!error.retryable);
}

#[tokio::test]
async fn rejects_an_unsupported_base_ram_tier_before_sending_a_request() {
    let client = client(Url::parse("http://127.0.0.1:1/").unwrap());
    let error = client
        .create_base_sandbox(None, Some(4098))
        .await
        .unwrap_err();
    assert_eq!(error.kind, E2bErrorKind::Configuration);
    assert!(error.message.contains("1024, 2048, 4096, or 8192"));
}

fn client(base_url: Url) -> E2bControlClient {
    E2bControlClient::new(config(base_url)).unwrap()
}

fn config(base_url: Url) -> E2bConfig {
    let mut config = E2bConfig::new(E2bApiKey::new("secret").unwrap()).unwrap();
    config.control_base_url = base_url;
    config
}

async fn serve(app: Router) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Url::parse(&format!("http://{address}/")).unwrap()
}

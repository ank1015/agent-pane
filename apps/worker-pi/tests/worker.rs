use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_contracts::{
    AbortDirective, AbortFinalizationReason, AcknowledgeRunAbort, AgentApiError,
    AgentErrorResponse, ClaimRun, ClaimedRun, HarnessRevision, HarnessRevisionStatus, HeartbeatRun,
    Run, RunAbort, RunAbortAcknowledged, RunAbortStatus, RunHeartbeat, RunLease, RunStatus,
    WorkerDirective,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::RETRY_AFTER},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration as ChronoDuration, Utc};
use execution_runtime::OperationContext;
use serde_json::json;
use url::Url;
use uuid::Uuid;
use worker_pi::{
    clients::AgentClient,
    config::AgentServiceConfig,
    worker::{RunInterruption, WorkerService},
};

#[tokio::test]
async fn claims_with_stable_credentials_and_acknowledges_heartbeat_abort() {
    let state = Arc::new(MockAgent::new());
    let app = Router::new()
        .route("/v1/worker/runs/claim", post(claim))
        .route("/v1/worker/runs/{run_id}/heartbeat", post(heartbeat))
        .route(
            "/v1/worker/runs/{run_id}/abort/acknowledge",
            post(acknowledge_abort),
        )
        .with_state(state.clone());
    let agent = AgentClient::new(AgentServiceConfig {
        base_url: serve(app).await,
        worker_token: "worker-secret".to_owned(),
        request_timeout: Duration::from_secs(2),
    })
    .expect("Agent client");
    let service = WorkerService::new(agent, "pi-worker-a".to_owned(), vec!["pi-v1".to_owned()]);
    let shutdown = OperationContext::new();

    let active = service.claim_next(&shutdown).await.expect("claimed run");
    assert_eq!(active.run_id(), state.run_id);
    assert_eq!(active.state_version(), 2);

    tokio::time::timeout(Duration::from_secs(2), active.operation().cancelled())
        .await
        .expect("abort directive should cancel the harness operation");
    let Some(RunInterruption::Abort(abort)) = active.interruption() else {
        panic!("expected an abort interruption");
    };
    assert_eq!(abort.abort_id, state.abort_id);
    assert_eq!(abort.reason.as_deref(), Some("user_request"));
    assert_eq!(active.state_version(), 3);

    let acknowledged = active
        .acknowledge_abort(json!({ "cleanup": "done" }).as_object().unwrap().clone())
        .await
        .expect("abort acknowledgement");
    assert_eq!(acknowledged.run.status, RunStatus::Aborted);

    let claims = state.claims.lock().expect("claim capture");
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0], claims[1]);
    assert_eq!(claims[0].0.worker_instance_id, "pi-worker-a");
    assert_eq!(claims[0].0.supported_harness_revision_ids, ["pi-v1"]);
    let token = URL_SAFE_NO_PAD
        .decode(&claims[0].1)
        .expect("base64url lease token");
    assert_eq!(token.len(), 32);
    drop(claims);

    let ack = state.ack.lock().expect("ack capture").clone().unwrap();
    assert_eq!(ack.abort_id, state.abort_id);
    assert_eq!(ack.lease_version, 2);
    assert_eq!(ack.expected_state_version, 3);
    assert_eq!(ack.resume_metadata["cleanup"], "done");
    assert!(state.heartbeats.load(Ordering::Acquire) >= 1);
}

#[tokio::test]
async fn lease_rejection_cancels_the_active_run() {
    let state = Arc::new(MockAgent::new());
    let app = Router::new()
        .route("/v1/worker/runs/claim", post(claim))
        .route(
            "/v1/worker/runs/{run_id}/heartbeat",
            post(heartbeat_lease_lost),
        )
        .with_state(state);
    let agent = AgentClient::new(AgentServiceConfig {
        base_url: serve(app).await,
        worker_token: "worker-secret".to_owned(),
        request_timeout: Duration::from_secs(2),
    })
    .expect("Agent client");
    let service = WorkerService::new(agent, "pi-worker-a".to_owned(), vec!["pi-v1".to_owned()]);
    let shutdown = OperationContext::new();
    let active = service.claim_next(&shutdown).await.expect("claimed run");

    tokio::time::timeout(Duration::from_secs(2), active.operation().cancelled())
        .await
        .expect("lease rejection should cancel the harness operation");
    assert_eq!(active.interruption(), Some(RunInterruption::LeaseLost));
}

struct MockAgent {
    run_id: Uuid,
    session_id: Uuid,
    trigger_message_id: Uuid,
    abort_id: Uuid,
    claims: Mutex<Vec<(ClaimRun, String)>>,
    heartbeats: AtomicUsize,
    ack: Mutex<Option<AcknowledgeRunAbort>>,
}

impl MockAgent {
    fn new() -> Self {
        Self {
            run_id: Uuid::now_v7(),
            session_id: Uuid::now_v7(),
            trigger_message_id: Uuid::now_v7(),
            abort_id: Uuid::now_v7(),
            claims: Mutex::new(Vec::new()),
            heartbeats: AtomicUsize::new(0),
            ack: Mutex::new(None),
        }
    }

    fn run(&self, status: RunStatus, state_version: u64) -> Run {
        let now = Utc::now();
        Run {
            run_id: self.run_id,
            session_id: self.session_id,
            trigger_message_id: self.trigger_message_id,
            harness_revision_id: "pi-v1".to_owned(),
            resolved_config: serde_json::Map::new(),
            status,
            current_turn: 1,
            max_turns: 100,
            failures_in_current_turn: 0,
            max_failures_per_turn: 3,
            state_version,
            queued_at: None,
            final_message_id: None,
            failure: None,
            created_at: now,
            started_at: Some(now),
            finished_at: (status == RunStatus::Aborted).then_some(now),
        }
    }

    fn abort_directive(&self) -> AbortDirective {
        let requested_at = Utc::now() - ChronoDuration::seconds(1);
        AbortDirective {
            abort_id: self.abort_id,
            reason: Some("user_request".to_owned()),
            payload: serde_json::Map::new(),
            requested_at,
            deadline_at: requested_at + ChronoDuration::seconds(30),
        }
    }
}

async fn claim(
    State(state): State<Arc<MockAgent>>,
    headers: HeaderMap,
    Json(command): Json<ClaimRun>,
) -> Response {
    let token = header(&headers, "x-agent-lease-token");
    let mut claims = state.claims.lock().expect("claim capture");
    claims.push((command.clone(), token));
    if claims.len() == 1 {
        let mut response = StatusCode::NO_CONTENT.into_response();
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from_static("0"));
        return response;
    }
    drop(claims);

    let now = Utc::now();
    Json(ClaimedRun {
        run: state.run(RunStatus::Running, 2),
        lease: RunLease {
            lease_id: command.lease_id,
            run_id: state.run_id,
            lease_version: 2,
            worker_instance_id: command.worker_instance_id,
            acquired_at: now,
            expires_at: now + ChronoDuration::milliseconds(900),
        },
        harness_revision: HarnessRevision {
            harness_revision_id: "pi-v1".to_owned(),
            harness_id: "pi".to_owned(),
            revision: "1".to_owned(),
            contract_version: 1,
            status: HarnessRevisionStatus::Active,
            default_config: serde_json::Map::new(),
            config_schema: None,
            first_activated_at: Some(now),
            retired_at: None,
            created_at: now,
        },
        current_session_revision: 1,
        resume: None,
    })
    .into_response()
}

async fn heartbeat(
    State(state): State<Arc<MockAgent>>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
    Json(command): Json<HeartbeatRun>,
) -> Json<RunHeartbeat> {
    assert_eq!(run_id, state.run_id);
    assert_eq!(command.lease_version, 2);
    let (lease_id, claim_token) = {
        let claims = state.claims.lock().expect("claims");
        (claims[0].0.lease_id, claims[0].1.clone())
    };
    assert_eq!(header(&headers, "x-agent-lease-token"), claim_token);
    state.heartbeats.fetch_add(1, Ordering::AcqRel);
    Json(RunHeartbeat {
        lease_id,
        lease_version: 2,
        state_version: 3,
        expires_at: Utc::now() + ChronoDuration::milliseconds(900),
        directives: vec![WorkerDirective::Abort(state.abort_directive())],
    })
}

async fn heartbeat_lease_lost() -> Response {
    (
        StatusCode::CONFLICT,
        Json(AgentErrorResponse {
            error: AgentApiError {
                code: "run_lease_lost".to_owned(),
                message: "the run lease is no longer active".to_owned(),
                details: None,
            },
        }),
    )
        .into_response()
}

async fn acknowledge_abort(
    State(state): State<Arc<MockAgent>>,
    Path(run_id): Path<Uuid>,
    Json(command): Json<AcknowledgeRunAbort>,
) -> Json<RunAbortAcknowledged> {
    assert_eq!(run_id, state.run_id);
    *state.ack.lock().expect("ack capture") = Some(command.clone());
    let now = Utc::now();
    Json(RunAbortAcknowledged {
        abort: RunAbort {
            abort_id: state.abort_id,
            run_id: state.run_id,
            turn_number: 1,
            sequence: 1,
            reason: Some("user_request".to_owned()),
            payload: serde_json::Map::new(),
            status: RunAbortStatus::Finalized,
            requested_at: now - ChronoDuration::seconds(1),
            delivered_at: Some(now - ChronoDuration::milliseconds(500)),
            deadline_at: now + ChronoDuration::seconds(30),
            finalized_at: Some(now),
            finalization_reason: Some(AbortFinalizationReason::Acknowledged),
            resume_metadata: command.resume_metadata,
            resolution: None,
            resumed_at: None,
            resumed_turn_number: None,
        },
        run: state.run(RunStatus::Aborted, 4),
    })
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers[name].to_str().expect("ASCII header").to_owned()
}

async fn serve(app: Router) -> Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let address = listener.local_addr().expect("test server address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve test app");
    });
    format!("http://{address}").parse().expect("server URL")
}

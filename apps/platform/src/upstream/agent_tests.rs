use std::{collections::HashMap, time::Duration};

use agent_contracts::RunStatus;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio::{net::TcpListener, sync::mpsc};
use uuid::Uuid;

use super::{
    AgentClient, AgentHarnessSelection, AgentRunAbortRequest, AgentRunEventListQuery,
    AgentRunEventStreamQuery, AgentRunLimits, AgentSessionMessageListQuery,
    AgentSessionRunListQuery, AgentStartRun,
};

#[derive(Deserialize)]
struct ListQuery {
    limit: u32,
    cursor: Option<String>,
}

#[tokio::test]
async fn harness_detail_authenticates_and_encodes_the_harness_id() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_agent = Router::new()
        .route("/v1/harnesses/{harness_id}", get(capture_harness_detail))
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_agent).await.unwrap();
    });

    let client = AgentClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-agent-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let harness = client.get_harness("environment").await.unwrap();
    let (authorization, harness_id) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-agent-control-token");
    assert_eq!(harness_id, "environment");
    assert_eq!(harness.supported_providers[0].provider_id, "openai");
    assert_eq!(harness.supported_reasoning_levels[3], "xhigh");
}

#[tokio::test]
async fn harness_list_authenticates_and_forwards_pagination() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_agent = Router::new()
        .route("/v1/harnesses", get(capture_harness_list))
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_agent).await.unwrap();
    });

    let client = AgentClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-agent-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let page = client.list_harnesses(Some("next-page")).await.unwrap();
    let (authorization, limit, cursor) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-agent-control-token");
    assert_eq!(limit, 100);
    assert_eq!(cursor.as_deref(), Some("next-page"));
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].harness_id, "pi");
    assert_eq!(page.items[0].display_name, "Pi");
    assert_eq!(page.next_cursor.as_deref(), Some("another-page"));
}

#[tokio::test]
async fn session_creation_and_initial_run_authenticate_and_forward_the_contract() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_agent = Router::new()
        .route("/v1/sessions", post(capture_session_create))
        .route("/v1/sessions/{session_id}/runs", post(capture_run_start))
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_agent).await.unwrap();
    });

    let client = AgentClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-agent-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let session_id = Uuid::now_v7();
    let run_id = Uuid::now_v7();
    let message_id = Uuid::now_v7();
    let message = serde_json::from_value(json!({
        "role": "user",
        "id": message_id.to_string(),
        "timestamp": 1,
        "content": [{"type": "text", "content": "Build it"}]
    }))
    .unwrap();

    let session = client.create_session(session_id).await.unwrap();
    let accepted = client
        .start_run(
            session_id,
            &AgentStartRun {
                run_id,
                input: agent_contracts::NewRunMessage {
                    session_message_id: message_id,
                    message,
                },
                harness: AgentHarnessSelection::ActiveRevision {
                    harness_id: "environment".to_owned(),
                },
                config_override: serde_json::from_value(json!({"custom": true})).unwrap(),
                limits: AgentRunLimits {
                    max_turns: Some(12),
                },
                expected_session_revision: Some(0),
            },
        )
        .await
        .unwrap();

    let create = request_rx.recv().await.unwrap();
    let start = request_rx.recv().await.unwrap();
    assert_eq!(session.session_id, session_id);
    assert_eq!(accepted.run.run_id, run_id);
    assert_eq!(create.0, "Bearer platform-agent-control-token");
    assert_eq!(create.1, "/v1/sessions");
    assert_eq!(create.2["session_id"], session_id.to_string());
    assert_eq!(start.0, "Bearer platform-agent-control-token");
    assert_eq!(start.1, format!("/v1/sessions/{session_id}/runs"));
    assert_eq!(start.2["run_id"], run_id.to_string());
    assert_eq!(start.2["harness"]["selection"], "active_revision");
    assert_eq!(start.2["harness"]["harness_id"], "environment");
    assert_eq!(start.2["config_override"], json!({"custom": true}));
    assert_eq!(start.2["limits"]["max_turns"], 12);
    assert_eq!(start.2["expected_session_revision"], 0);
}

#[tokio::test]
async fn session_reads_authenticate_and_forward_pagination() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_agent = Router::new()
        .route("/v1/sessions/{session_id}", get(capture_session_get))
        .route(
            "/v1/sessions/{session_id}/messages",
            get(capture_session_messages),
        )
        .route("/v1/sessions/{session_id}/runs", get(capture_session_runs))
        .route("/v1/runs/{run_id}", get(capture_run_get))
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_agent).await.unwrap();
    });

    let client = AgentClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-agent-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let session_id = Uuid::now_v7();
    let run_id = Uuid::now_v7();

    let session = client.get_session(session_id).await.unwrap();
    let messages = client
        .list_session_messages(
            session_id,
            &AgentSessionMessageListQuery {
                after_revision: Some(3),
                limit: Some(20),
            },
        )
        .await
        .unwrap();
    let runs = client
        .list_session_runs(
            session_id,
            &AgentSessionRunListQuery {
                status: Some(RunStatus::Waiting),
                limit: Some(2),
                cursor: Some("next-run".to_owned()),
            },
        )
        .await
        .unwrap();
    let run = client.get_run(run_id).await.unwrap();

    assert_eq!(session.current_revision, 4);
    assert_eq!(messages.next_after_revision, Some(4));
    assert_eq!(runs.next_cursor.as_deref(), Some("another-run"));
    assert_eq!(run.run_id, run_id);
    let get_request = request_rx.recv().await.unwrap();
    let message_request = request_rx.recv().await.unwrap();
    let run_list_request = request_rx.recv().await.unwrap();
    let run_get_request = request_rx.recv().await.unwrap();
    for request in [
        &get_request,
        &message_request,
        &run_list_request,
        &run_get_request,
    ] {
        assert_eq!(request.0, "Bearer platform-agent-control-token");
    }
    assert_eq!(get_request.1, format!("/v1/sessions/{session_id}"));
    assert_eq!(message_request.2["after_revision"], "3");
    assert_eq!(message_request.2["limit"], "20");
    assert_eq!(run_list_request.2["status"], "waiting");
    assert_eq!(run_list_request.2["limit"], "2");
    assert_eq!(run_list_request.2["cursor"], "next-run");
    assert_eq!(run_get_request.1, format!("/v1/runs/{run_id}"));
}

#[tokio::test]
async fn run_events_stream_and_abort_authenticate_and_forward_the_contract() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_agent = Router::new()
        .route("/v1/runs/{run_id}/events", get(capture_run_events))
        .route(
            "/v1/runs/{run_id}/events/stream",
            get(capture_run_event_stream),
        )
        .route("/v1/runs/{run_id}/abort", post(capture_run_abort))
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_agent).await.unwrap();
    });

    let client = AgentClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-agent-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let run_id = Uuid::now_v7();
    let abort_id = Uuid::now_v7();

    let events = client
        .list_run_events(
            run_id,
            &AgentRunEventListQuery {
                after_sequence: Some(3),
                limit: Some(25),
            },
        )
        .await
        .unwrap();
    let stream = client
        .stream_run_events(
            run_id,
            &AgentRunEventStreamQuery {
                after_sequence: Some(4),
            },
            Some("5"),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.headers()["content-type"],
        "text/event-stream; charset=utf-8"
    );
    let stream_body = stream.text().await.unwrap();
    let aborted = client
        .abort_run(
            run_id,
            &AgentRunAbortRequest {
                abort_id,
                expected_state_version: 6,
                reason: Some("Stopped by user".to_owned()),
                payload: serde_json::Map::new(),
            },
        )
        .await
        .unwrap();

    assert_eq!(events.items[0].sequence, 4);
    assert_eq!(events.next_after_sequence, Some(4));
    assert!(stream_body.contains("event: progress"));
    assert_eq!(aborted.abort.abort_id, abort_id);
    assert_eq!(aborted.run.status, RunStatus::Aborted);

    let list_request = request_rx.recv().await.unwrap();
    let stream_request = request_rx.recv().await.unwrap();
    let abort_request = request_rx.recv().await.unwrap();
    for request in [&list_request, &stream_request, &abort_request] {
        assert_eq!(request.0, "Bearer platform-agent-control-token");
    }
    assert_eq!(list_request.1, format!("/v1/runs/{run_id}/events"));
    assert_eq!(list_request.2["after_sequence"], "3");
    assert_eq!(list_request.2["limit"], "25");
    assert_eq!(stream_request.1, format!("/v1/runs/{run_id}/events/stream"));
    assert_eq!(stream_request.2["after_sequence"], "4");
    assert_eq!(stream_request.2["last_event_id"], "5");
    assert_eq!(abort_request.1, format!("/v1/runs/{run_id}/abort"));
    assert_eq!(abort_request.2["abort_id"], abort_id.to_string());
    assert_eq!(abort_request.2["expected_state_version"], 6);
    assert_eq!(abort_request.2["reason"], "Stopped by user");
}

async fn capture_harness_list(
    State(request_tx): State<mpsc::UnboundedSender<(String, u32, Option<String>)>>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Json<serde_json::Value> {
    request_tx
        .send((
            headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned(),
            query.limit,
            query.cursor,
        ))
        .unwrap();
    Json(json!({
        "items": [{
            "harness_id": "pi",
            "slug": "pi",
            "display_name": "Pi",
            "description": "Pi coding agent",
            "enabled": true,
            "active_revision_id": "pi-v1",
            "created_at": "2026-08-27T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z"
        }],
        "next_cursor": "another-page"
    }))
}

async fn capture_harness_detail(
    State(request_tx): State<mpsc::UnboundedSender<(String, String)>>,
    Path(harness_id): Path<String>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    request_tx
        .send((
            headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned(),
            harness_id,
        ))
        .unwrap();
    Json(json!({
        "harness_id": "environment",
        "slug": "environment",
        "display_name": "Environment Harness",
        "description": "Environment harness",
        "supported_providers": [{
            "provider_id": "openai",
            "model_ids": ["gpt-5.6-sol", "gpt-5.6-terra"]
        }],
        "supported_reasoning_levels": ["low", "medium", "high", "xhigh", "max"],
        "enabled": true,
        "active_revision_id": "environment-v1",
        "created_at": "2026-08-27T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z"
    }))
}

async fn capture_session_create(
    State(request_tx): State<mpsc::UnboundedSender<(String, String, serde_json::Value)>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    request_tx
        .send((
            authorization(&headers),
            "/v1/sessions".to_owned(),
            body.clone(),
        ))
        .unwrap();
    (
        StatusCode::CREATED,
        Json(json!({
            "session_id": body["session_id"],
            "current_revision": 0,
            "created_at": "2026-09-01T00:00:00Z"
        })),
    )
}

async fn capture_run_start(
    State(request_tx): State<mpsc::UnboundedSender<(String, String, serde_json::Value)>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    request_tx
        .send((
            authorization(&headers),
            format!("/v1/sessions/{session_id}/runs"),
            body.clone(),
        ))
        .unwrap();
    let message_id = body["input"]["session_message_id"].clone();
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "trigger_message": {
                "session_message_id": message_id,
                "session_id": session_id,
                "revision": 1,
                "message": body["input"]["message"],
                "origin": "external",
                "delivery": "immediate",
                "run_id": body["run_id"],
                "turn_number": 1,
                "created_at": "2026-09-01T00:00:00Z",
                "committed_at": "2026-09-01T00:00:00Z"
            },
            "run": {
                "run_id": body["run_id"],
                "session_id": session_id,
                "trigger_message_id": message_id,
                "harness_revision_id": "environment-v1",
                "resolved_config": body["config_override"],
                "status": "active",
                "current_turn": 1,
                "max_turns": 12,
                "state_version": 1,
                "final_message_id": null,
                "failure": null,
                "created_at": "2026-09-01T00:00:00Z",
                "activated_at": "2026-09-01T00:00:00Z",
                "finished_at": null
            }
        })),
    )
}

fn authorization(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

type ReadRequest = (String, String, serde_json::Value);

async fn capture_session_get(
    State(request_tx): State<mpsc::UnboundedSender<ReadRequest>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    request_tx
        .send((
            authorization(&headers),
            format!("/v1/sessions/{session_id}"),
            json!({}),
        ))
        .unwrap();
    Json(json!({
        "session_id": session_id,
        "current_revision": 4,
        "created_at": "2026-09-01T00:00:00Z"
    }))
}

async fn capture_session_messages(
    State(request_tx): State<mpsc::UnboundedSender<ReadRequest>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    request_tx
        .send((
            authorization(&headers),
            format!("/v1/sessions/{session_id}/messages"),
            json!(query),
        ))
        .unwrap();
    Json(json!({
        "items": [],
        "next_after_revision": 4
    }))
}

async fn capture_session_runs(
    State(request_tx): State<mpsc::UnboundedSender<ReadRequest>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    request_tx
        .send((
            authorization(&headers),
            format!("/v1/sessions/{session_id}/runs"),
            json!(query),
        ))
        .unwrap();
    Json(json!({
        "items": [],
        "next_cursor": "another-run"
    }))
}

async fn capture_run_get(
    State(request_tx): State<mpsc::UnboundedSender<ReadRequest>>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    request_tx
        .send((
            authorization(&headers),
            format!("/v1/runs/{run_id}"),
            json!({}),
        ))
        .unwrap();
    Json(run_response(run_id, Uuid::now_v7(), Uuid::now_v7()))
}

async fn capture_run_events(
    State(request_tx): State<mpsc::UnboundedSender<ReadRequest>>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    request_tx
        .send((
            authorization(&headers),
            format!("/v1/runs/{run_id}/events"),
            json!(query),
        ))
        .unwrap();
    Json(json!({
        "items": [{
            "event_id": Uuid::now_v7(),
            "run_id": run_id,
            "sequence": 4,
            "turn_number": 1,
            "state_version": 2,
            "run_status": "active",
            "source": "harness",
            "occurred_at": "2026-09-01T00:00:00Z",
            "recorded_at": "2026-09-01T00:00:00Z",
            "type": "progress",
            "details": {
                "name": "building",
                "data": {}
            }
        }],
        "next_after_sequence": 4
    }))
}

async fn capture_run_event_stream(
    State(request_tx): State<mpsc::UnboundedSender<ReadRequest>>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    request_tx
        .send((
            authorization(&headers),
            format!("/v1/runs/{run_id}/events/stream"),
            json!({
                "after_sequence": query.get("after_sequence"),
                "last_event_id": headers.get("last-event-id").unwrap().to_str().unwrap()
            }),
        ))
        .unwrap();
    (
        [
            ("content-type", "text/event-stream; charset=utf-8"),
            ("cache-control", "no-cache, no-transform"),
            ("x-accel-buffering", "no"),
        ],
        "id: 6\nevent: progress\ndata: {\"sequence\":6}\n\n",
    )
}

async fn capture_run_abort(
    State(request_tx): State<mpsc::UnboundedSender<ReadRequest>>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    request_tx
        .send((
            authorization(&headers),
            format!("/v1/runs/{run_id}/abort"),
            body.clone(),
        ))
        .unwrap();
    let mut run = run_response(run_id, Uuid::now_v7(), Uuid::now_v7());
    run["status"] = json!("aborted");
    run["state_version"] = json!(7);
    run["finished_at"] = json!("2026-09-01T00:01:00Z");
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "abort": {
                "abort_id": body["abort_id"],
                "run_id": run_id,
                "turn_number": 1,
                "reason": body["reason"],
                "payload": body["payload"],
                "requested_at": "2026-09-01T00:01:00Z"
            },
            "run": run
        })),
    )
}

fn run_response(run_id: Uuid, session_id: Uuid, message_id: Uuid) -> serde_json::Value {
    json!({
        "run_id": run_id,
        "session_id": session_id,
        "trigger_message_id": message_id,
        "harness_revision_id": "environment-v1",
        "resolved_config": {},
        "status": "active",
        "current_turn": 1,
        "max_turns": 100,
        "state_version": 1,
        "final_message_id": null,
        "failure": null,
        "created_at": "2026-09-01T00:00:00Z",
        "activated_at": "2026-09-01T00:00:00Z",
        "finished_at": null
    })
}

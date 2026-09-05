//! Caller-independent execution. In-flight work is intentionally process-local.
use std::time::Duration;

use axum::extract::Path;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use tokio::time::{Instant, sleep};

use super::*;

#[derive(Serialize, FromRow)]
struct Run {
    #[serde(rename = "run_id")]
    id: Uuid,
    status: String,
    result: Option<Value>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
}

pub(super) async fn submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<CompleteRequest>, JsonRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let key = match headers.get("idempotency-key").and_then(|v| v.to_str().ok()) {
        Some(key)
            if !key.is_empty() && key.len() <= 256 && key.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            key.to_owned()
        }
        _ => {
            return error_response(
                request_id,
                GatewayError::invalid_request(
                    "Idempotency-Key must contain 1–256 visible ASCII characters".to_owned(),
                ),
            );
        }
    };
    let Json(payload) = match payload {
        Ok(payload) => payload,
        Err(error) => return error_response(request_id, GatewayError::bad_json(error.body_text())),
    };
    // Detach acceptance too: losing the HTTP connection after INSERT must not
    // leave an accepted run without its execution task.
    match tokio::spawn(async move { accept(state, key, payload, request_id).await }).await {
        Ok(Ok(run)) => run_response(run, true),
        Ok(Err(error)) => error_response(request_id, error),
        Err(error) => {
            tracing::error!(%error, %request_id, "run acceptance task failed");
            error_response(request_id, GatewayError::internal_error())
        }
    }
}

async fn accept(
    state: AppState,
    key: String,
    payload: CompleteRequest,
    id: Uuid,
) -> Result<Run, GatewayError> {
    // Round-trip through Value sorts object keys, including metadata maps.
    let value = serde_json::to_value(&payload).map_err(|_| GatewayError::internal_error())?;
    let bytes = serde_json::to_vec(&value).map_err(|_| GatewayError::internal_error())?;
    let hash = Sha256::digest(bytes).to_vec();
    let inserted = sqlx::query(
        "insert into llm_runs (id, idempotency_key, request_hash) values ($1, $2, $3) on conflict (idempotency_key) do nothing",
    ).bind(id).bind(&key).bind(&hash).execute(state.database.pool()).await.map_err(database_error)?.rows_affected() == 1;
    if inserted {
        let worker = state.clone();
        tokio::spawn(async move {
            let result = worker
                .gateway
                .complete_until_aborted(
                    id,
                    payload.request,
                    payload.account_id,
                    wait_for_abort(worker.database.clone(), id),
                )
                .await;
            let (status, value) = match result {
                Ok(completion) => (
                    "succeeded",
                    serde_json::to_value(CompleteResponse {
                        request_id: id,
                        account_id: completion.account_id,
                        message: completion.message,
                    }),
                ),
                Err(error) => (
                    "failed",
                    serde_json::to_value(ErrorResponse {
                        request_id: id,
                        account_id: error.account_id,
                        error,
                    }),
                ),
            };
            let value = value.unwrap_or_else(|_| serde_json::json!({"error": {"kind": "internal", "message": "result serialization failed", "can_retry": false}}));
            // Retry saving the existing output, never the billable provider call.
            loop {
                match sqlx::query("update llm_runs set status = $2, result = $3, completed_at = now(), expires_at = now() + interval '48 hours' where id = $1 and status = 'running'")
                    .bind(id).bind(status).bind(&value).execute(worker.database.pool()).await {
                    Ok(_) => break,
                    Err(error) => tracing::error!(%error, run_id = %id, "could not persist run result; retrying"),
                }
                sleep(Duration::from_secs(5)).await;
            }
        });
    }
    let (existing_id, existing_hash): (Uuid, Vec<u8>) =
        sqlx::query_as("select id, request_hash from llm_runs where idempotency_key = $1")
            .bind(key)
            .fetch_one(state.database.pool())
            .await
            .map_err(database_error)?;
    if existing_hash != hash {
        return Err(GatewayError::conflict(
            "Idempotency-Key was already used with a different request".to_owned(),
        ));
    }
    load(&state.database, existing_id).await
}

/// Abort intent is the terminal DB transition, so it survives a dropped caller
/// and works even when the HTTP request reaches a different gateway replica.
pub(super) async fn abort(State(state): State<AppState>, Path(id): Path<Uuid>) -> Response {
    match tokio::spawn(async move {
        sqlx::query("update llm_runs set status = 'aborted', result = null, completed_at = now(), expires_at = now() + interval '48 hours' where id = $1 and status = 'running'")
            .bind(id).execute(state.database.pool()).await.map_err(database_error)?;
        load(&state.database, id).await
    }).await {
        Ok(Ok(run)) => run_response(run, false),
        Ok(Err(error)) => error_response(id, error),
        Err(error) => {
            tracing::error!(%error, run_id = %id, "run abort task failed");
            error_response(id, GatewayError::internal_error())
        }
    }
}

async fn wait_for_abort(database: Database, id: Uuid) {
    loop {
        match sqlx::query_scalar::<_, String>("select status from llm_runs where id = $1")
            .bind(id)
            .fetch_optional(database.pool())
            .await
        {
            Ok(Some(status)) if status == "running" => {}
            // A deleted or expired run must not continue consuming provider work.
            Ok(_) => return,
            Err(error) => tracing::error!(%error, run_id = %id, "could not check run abort state"),
        }
        sleep(Duration::from_millis(250)).await;
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct WaitQuery {
    wait_seconds: u64,
}

pub(super) async fn retrieve(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    query: Result<Query<WaitQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) if query.wait_seconds <= 25 => query,
        _ => {
            return error_response(
                id,
                GatewayError::invalid_query(
                    "wait_seconds must be an integer from 0 to 25".to_owned(),
                ),
            );
        }
    };
    let deadline = Instant::now() + Duration::from_secs(query.wait_seconds);
    loop {
        match load(&state.database, id).await {
            Ok(run) if run.status == "running" && Instant::now() < deadline => {
                sleep(
                    Duration::from_millis(250)
                        .min(deadline.saturating_duration_since(Instant::now())),
                )
                .await;
            }
            Ok(run) => return run_response(run, false),
            Err(error) => return error_response(id, error),
        }
    }
}

async fn load(database: &Database, id: Uuid) -> Result<Run, GatewayError> {
    sqlx::query_as::<_, Run>(
        "select id, case when expires_at <= now() then 'expired' else status end as status, case when expires_at <= now() then null else result end as result, created_at, completed_at, expires_at from llm_runs where id = $1",
    ).bind(id).fetch_optional(database.pool()).await.map_err(database_error)?
        .ok_or_else(|| GatewayError::request_not_found(id))
}

fn run_response(run: Run, submitted: bool) -> Response {
    let status = if run.status == "expired" {
        StatusCode::GONE
    } else if submitted && run.status == "running" {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    let location = format!("/v1/llm/runs/{}", run.id);
    let running = run.status == "running";
    let mut response = json_response(status, run.id, None, run);
    response.headers_mut().insert(
        axum::http::header::LOCATION,
        HeaderValue::from_str(&location).expect("UUID location"),
    );
    if running {
        response.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            HeaderValue::from_static("1"),
        );
    }
    response
}

fn database_error(error: sqlx::Error) -> GatewayError {
    tracing::error!(%error, "run database operation failed");
    GatewayError::internal_error()
}

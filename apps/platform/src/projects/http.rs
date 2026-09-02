use axum::{
    Json, Router,
    body::Body,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use execution_protocol::ProjectEnvironment;
use uuid::Uuid;

use super::{
    ProjectService,
    model::{
        CreateProjectRequest, CreateProjectSessionRequest, Project, StartProjectSessionRunRequest,
        UpdateProjectRequest, UpdateProjectSessionRequest,
    },
};
use crate::{
    AppState,
    error::ApiError,
    upstream::agent::{AgentSessionMessageListQuery, AgentSessionRunListQuery},
};

#[derive(Clone)]
struct ProjectState {
    service: ProjectService,
}

pub(super) fn router(service: ProjectService) -> Router<AppState> {
    Router::new()
        .route("/api/projects", get(list_projects).post(create_project))
        .route(
            "/api/projects/{project_id}",
            get(get_project)
                .patch(update_project)
                .delete(delete_project),
        )
        .route(
            "/api/projects/{project_id}/environments",
            get(list_project_environments),
        )
        .route(
            "/api/projects/{project_id}/bootstrap",
            get(get_project_bootstrap),
        )
        .route(
            "/api/projects/{project_id}/sessions",
            get(list_project_sessions).post(create_project_session),
        )
        .route(
            "/api/projects/{project_id}/sessions/{session_id}",
            get(get_project_session).patch(update_project_session),
        )
        .route(
            "/api/projects/{project_id}/sessions/{session_id}/messages",
            get(list_project_session_messages),
        )
        .route(
            "/api/projects/{project_id}/sessions/{session_id}/runs",
            get(list_project_session_runs).post(start_project_session_run),
        )
        .route(
            "/api/projects/{project_id}/sessions/{session_id}/runs/{run_id}",
            get(get_project_run),
        )
        .route(
            "/api/projects/{project_id}/sessions/{session_id}/runs/{run_id}/events",
            get(list_project_run_events),
        )
        .route(
            "/api/projects/{project_id}/sessions/{session_id}/runs/{run_id}/events/stream",
            get(stream_project_run_events),
        )
        .route(
            "/api/projects/{project_id}/sessions/{session_id}/runs/{run_id}/abort",
            post(abort_project_run),
        )
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/{session_id}", get(get_session))
        .route(
            "/api/sessions/{session_id}/messages",
            get(list_session_messages),
        )
        .route("/api/sessions/{session_id}/runs", get(list_session_runs))
        .layer(middleware::map_response(add_no_store))
        .with_state(ProjectState { service })
}

async fn list_projects(State(state): State<ProjectState>) -> Result<Json<Vec<Project>>, ApiError> {
    Ok(Json(state.service.list().await?))
}

async fn list_sessions(
    State(state): State<ProjectState>,
) -> Result<Json<Vec<super::model::SessionListItem>>, ApiError> {
    Ok(Json(state.service.list_all_sessions().await?))
}

async fn get_project(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(state.service.get(project_id).await?))
}

async fn list_project_environments(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<ProjectEnvironment>>, ApiError> {
    Ok(Json(state.service.list_environments(project_id).await?))
}

async fn get_project_bootstrap(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<super::model::ProjectBootstrap>, ApiError> {
    Ok(Json(state.service.bootstrap(project_id).await?))
}

async fn list_project_sessions(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<super::model::ProjectSession>>, ApiError> {
    Ok(Json(state.service.list_sessions(project_id).await?))
}

async fn create_project_session(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
    headers: HeaderMap,
    payload: Result<Json<CreateProjectSessionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let idempotency_key = idempotency_key(&headers)?;
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let response = state
        .service
        .create_session(project_id, idempotency_key, &request)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

async fn start_project_session_run(
    State(state): State<ProjectState>,
    Path((project_id, session_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    payload: Result<Json<StartProjectSessionRunRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let idempotency_key = idempotency_key(&headers)?;
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let response = state
        .service
        .start_session_run(project_id, session_id, idempotency_key, &request)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

async fn get_project_session(
    State(state): State<ProjectState>,
    Path((project_id, session_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<super::model::ProjectSessionResponse>, ApiError> {
    Ok(Json(
        state.service.get_session(project_id, session_id).await?,
    ))
}

async fn get_session(
    State(state): State<ProjectState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<super::model::ProjectSessionResponse>, ApiError> {
    Ok(Json(state.service.get_session_by_id(session_id).await?))
}

async fn update_project_session(
    State(state): State<ProjectState>,
    Path((project_id, session_id)): Path<(Uuid, Uuid)>,
    payload: Result<Json<UpdateProjectSessionRequest>, JsonRejection>,
) -> Result<Json<super::model::ProjectSession>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(
        state
            .service
            .update_session(project_id, session_id, request)
            .await?,
    ))
}

async fn list_project_session_messages(
    State(state): State<ProjectState>,
    Path((project_id, session_id)): Path<(Uuid, Uuid)>,
    query: Result<Query<AgentSessionMessageListQuery>, QueryRejection>,
) -> Result<Json<agent_contracts::SessionMessagePage>, ApiError> {
    let Query(query) = query.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(
        state
            .service
            .list_session_messages(project_id, session_id, &query)
            .await?,
    ))
}

async fn list_session_messages(
    State(state): State<ProjectState>,
    Path(session_id): Path<Uuid>,
    query: Result<Query<AgentSessionMessageListQuery>, QueryRejection>,
) -> Result<Json<agent_contracts::SessionMessagePage>, ApiError> {
    let Query(query) = query.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(
        state
            .service
            .list_session_messages_by_id(session_id, &query)
            .await?,
    ))
}

async fn list_project_session_runs(
    State(state): State<ProjectState>,
    Path((project_id, session_id)): Path<(Uuid, Uuid)>,
    query: Result<Query<AgentSessionRunListQuery>, QueryRejection>,
) -> Result<Json<crate::upstream::agent::AgentSessionRunPage>, ApiError> {
    let Query(query) = query.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(
        state
            .service
            .list_session_runs(project_id, session_id, &query)
            .await?,
    ))
}

async fn list_session_runs(
    State(state): State<ProjectState>,
    Path(session_id): Path<Uuid>,
    query: Result<Query<AgentSessionRunListQuery>, QueryRejection>,
) -> Result<Json<crate::upstream::agent::AgentSessionRunPage>, ApiError> {
    let Query(query) = query.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(
        state
            .service
            .list_session_runs_by_id(session_id, &query)
            .await?,
    ))
}

async fn get_project_run(
    State(state): State<ProjectState>,
    Path((project_id, session_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<Json<agent_contracts::Run>, ApiError> {
    Ok(Json(
        state
            .service
            .get_run(project_id, session_id, run_id)
            .await?,
    ))
}

async fn list_project_run_events(
    State(state): State<ProjectState>,
    Path((project_id, session_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
    query: Result<Query<crate::upstream::agent::AgentRunEventListQuery>, QueryRejection>,
) -> Result<Json<agent_contracts::RunEventPage>, ApiError> {
    let Query(query) = query.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(
        state
            .service
            .list_run_events(project_id, session_id, run_id, &query)
            .await?,
    ))
}

async fn stream_project_run_events(
    State(state): State<ProjectState>,
    Path((project_id, session_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
    headers: HeaderMap,
    query: Result<Query<crate::upstream::agent::AgentRunEventStreamQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let last_event_id = headers
        .get("last-event-id")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| ApiError::invalid_request("Last-Event-ID must be valid ASCII"))
        })
        .transpose()?;
    let upstream = state
        .service
        .stream_run_events(project_id, session_id, run_id, &query, last_event_id)
        .await?;
    let status = upstream.status();
    let content_type = upstream.headers().get(CONTENT_TYPE).cloned();
    let cache_control = upstream.headers().get(CACHE_CONTROL).cloned();
    let buffering = upstream.headers().get("x-accel-buffering").cloned();
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    if let Some(value) = content_type {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    if let Some(value) = cache_control {
        response.headers_mut().insert(CACHE_CONTROL, value);
    }
    if let Some(value) = buffering {
        response.headers_mut().insert("x-accel-buffering", value);
    }
    Ok(response)
}

async fn abort_project_run(
    State(state): State<ProjectState>,
    Path((project_id, session_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
    payload: Result<Json<crate::upstream::agent::AgentRunAbortRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let response = state
        .service
        .abort_run(project_id, session_id, run_id, &request)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

async fn create_project(
    State(state): State<ProjectState>,
    payload: Result<Json<CreateProjectRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let project = state.service.create(&request).await?;
    Ok((StatusCode::CREATED, Json(project)).into_response())
}

async fn update_project(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
    payload: Result<Json<UpdateProjectRequest>, JsonRejection>,
) -> Result<Json<Project>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(state.service.update(project_id, &request).await?))
}

async fn delete_project(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.service.delete(project_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_no_store(mut response: Response) -> Response {
    if !response.headers().contains_key(CACHE_CONTROL) {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

fn idempotency_key(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("idempotency-key")
        .ok_or_else(|| ApiError::invalid_request("Idempotency-Key header is required"))?
        .to_str()
        .map_err(|_| ApiError::invalid_request("Idempotency-Key must be valid ASCII"))
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::header::CACHE_CONTROL, response::Response};

    use super::add_no_store;

    #[tokio::test]
    async fn no_store_middleware_preserves_stream_cache_headers() {
        let mut stream = Response::new(Body::empty());
        stream
            .headers_mut()
            .insert(CACHE_CONTROL, "no-cache, no-transform".parse().unwrap());
        let stream = add_no_store(stream).await;
        assert_eq!(stream.headers()[CACHE_CONTROL], "no-cache, no-transform");

        let response = add_no_store(Response::new(Body::empty())).await;
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    }
}

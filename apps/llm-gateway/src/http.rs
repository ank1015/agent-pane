use std::time::Instant;

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use llm_contracts::{
    AssistantMessage, LlmRequest, Model, ProviderId, SearchRequest, SearchRequestOptions,
    SearchResponse as ProviderSearchResponse,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    account::{AccountService, ProviderKind},
    auth::AdminToken,
    catalog,
    db::{Database, ProviderAccount},
    gateway::{Gateway, GatewayError, GatewayErrorKind},
};

mod admin;
mod auth;

const REQUEST_ID_HEADER: &str = "x-request-id";
const ACCOUNT_ID_HEADER: &str = "x-account-id";

#[derive(Clone)]
struct AppState {
    database: Database,
    gateway: Gateway,
    accounts: AccountService,
}

pub fn router(
    database: Database,
    gateway: Gateway,
    account_service: AccountService,
    admin_token: AdminToken,
    max_request_bytes: usize,
) -> Router {
    let admin_routes = Router::new()
        .route(
            "/v1/admin/accounts",
            get(admin::list_accounts).post(admin::create_account),
        )
        .route(
            "/v1/admin/accounts/{account_id}",
            get(admin::get_account)
                .patch(admin::update_account)
                .delete(admin::delete_account),
        )
        .route(
            "/v1/admin/accounts/{account_id}/default",
            put(admin::set_default_account),
        )
        .route(
            "/v1/admin/accounts/{account_id}/credentials",
            put(admin::rotate_credentials),
        )
        .route_layer(middleware::from_fn_with_state(
            admin_token,
            auth::require_admin,
        ));

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .route("/v1/models", get(models))
        .route("/v1/accounts", get(accounts))
        .route("/v1/complete", post(complete))
        .route("/v1/search", post(search))
        .merge(admin_routes)
        .layer(DefaultBodyLimit::max(max_request_bytes))
        .with_state(AppState {
            database,
            gateway,
            accounts: account_service,
        })
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn readiness(
    State(state): State<AppState>,
) -> Result<Json<HealthResponse>, (StatusCode, Json<HealthResponse>)> {
    state.database.health_check().await.map_err(|error| {
        tracing::error!(%error, "database readiness check failed");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "unavailable",
            }),
        )
    })?;

    Ok(Json(HealthResponse { status: "ready" }))
}

async fn models(query: Result<Query<ProviderQuery>, QueryRejection>) -> Response {
    let request_id = Uuid::now_v7();
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => {
            return error_response(
                request_id,
                GatewayError::invalid_query(rejection.body_text()),
            );
        }
    };
    let provider = match parse_provider(query.provider.as_deref()) {
        Ok(provider) => provider,
        Err(error) => return error_response(request_id, error),
    };
    success_response(
        request_id,
        None,
        ModelsResponse {
            models: catalog::all_models(provider),
        },
    )
}

async fn accounts(
    State(state): State<AppState>,
    query: Result<Query<ProviderQuery>, QueryRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => {
            return error_response(
                request_id,
                GatewayError::invalid_query(rejection.body_text()),
            );
        }
    };
    let provider = match parse_provider(query.provider.as_deref()) {
        Ok(provider) => provider,
        Err(error) => return error_response(request_id, error),
    };
    match state
        .database
        .list_accounts(provider.map(ProviderKind::as_str), true)
        .await
    {
        Ok(accounts) => success_response(
            request_id,
            None,
            AccountsResponse {
                accounts: accounts.iter().map(AccountSummary::from).collect(),
            },
        ),
        Err(error) => {
            tracing::error!(%error, "account discovery query failed");
            error_response(request_id, GatewayError::internal_error())
        }
    }
}

async fn complete(
    State(state): State<AppState>,
    payload: Result<Json<CompleteRequest>, JsonRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let Json(payload) = match payload {
        Ok(payload) => payload,
        Err(rejection) => {
            return error_response(request_id, GatewayError::bad_json(rejection.body_text()));
        }
    };
    let provider = payload.request.model.provider.to_string();
    let model = payload.request.model.id.to_string();
    let requested_account_id = payload.account_id;
    let started = Instant::now();

    match state
        .gateway
        .complete(request_id, payload.request, payload.account_id)
        .await
    {
        Ok(completion) => {
            tracing::info!(
                %request_id,
                account_id = %completion.account_id,
                %provider,
                %model,
                duration_ms = started.elapsed().as_millis(),
                "LLM request completed"
            );
            success_response(
                request_id,
                Some(completion.account_id),
                CompleteResponse {
                    request_id,
                    account_id: completion.account_id,
                    message: completion.message,
                },
            )
        }
        Err(error) => {
            tracing::warn!(
                %request_id,
                requested_account_id = ?requested_account_id,
                resolved_account_id = ?error.account_id,
                %provider,
                %model,
                error_kind = ?error.kind,
                can_retry = error.can_retry,
                duration_ms = started.elapsed().as_millis(),
                "LLM request failed"
            );
            error_response(request_id, error)
        }
    }
}

async fn search(
    State(state): State<AppState>,
    payload: Result<Json<SearchGatewayRequest>, JsonRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let Json(payload) = match payload {
        Ok(payload) => payload,
        Err(rejection) => {
            return error_response(request_id, GatewayError::bad_json(rejection.body_text()));
        }
    };
    let provider = payload.provider.to_string();
    let model = payload.request.model.clone();
    let requested_account_id = payload.account_id;
    let started = Instant::now();

    match state
        .gateway
        .search(
            payload.provider,
            payload.request,
            payload.request_options,
            payload.account_id,
        )
        .await
    {
        Ok(search) => {
            tracing::info!(
                %request_id,
                account_id = %search.account_id,
                %provider,
                %model,
                duration_ms = started.elapsed().as_millis(),
                "web search request completed"
            );
            success_response(
                request_id,
                Some(search.account_id),
                SearchGatewayResponse {
                    request_id,
                    account_id: search.account_id,
                    response: search.response,
                },
            )
        }
        Err(error) => {
            tracing::warn!(
                %request_id,
                requested_account_id = ?requested_account_id,
                resolved_account_id = ?error.account_id,
                %provider,
                %model,
                error_kind = ?error.kind,
                can_retry = error.can_retry,
                duration_ms = started.elapsed().as_millis(),
                "web search request failed"
            );
            error_response(request_id, error)
        }
    }
}

fn parse_provider(value: Option<&str>) -> Result<Option<ProviderKind>, GatewayError> {
    value
        .map(str::parse::<ProviderKind>)
        .transpose()
        .map_err(|error| GatewayError::invalid_query(error.to_string()))
}

fn success_response<T>(request_id: Uuid, account_id: Option<Uuid>, body: T) -> Response
where
    T: Serialize,
{
    json_response(StatusCode::OK, request_id, account_id, body)
}

fn json_response<T>(
    status: StatusCode,
    request_id: Uuid,
    account_id: Option<Uuid>,
    body: T,
) -> Response
where
    T: Serialize,
{
    let mut headers = response_headers(request_id, account_id);
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    (status, headers, Json(body)).into_response()
}

fn empty_response(status: StatusCode, request_id: Uuid) -> Response {
    let mut headers = response_headers(request_id, None);
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    (status, headers).into_response()
}

fn error_response(request_id: Uuid, error: GatewayError) -> Response {
    let status = error.status_code();
    let account_id = error.account_id;
    let mut headers = response_headers(request_id, account_id);
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    (
        status,
        headers,
        Json(ErrorResponse {
            request_id,
            account_id,
            error,
        }),
    )
        .into_response()
}

fn response_headers(request_id: Uuid, account_id: Option<Uuid>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        REQUEST_ID_HEADER,
        HeaderValue::from_str(&request_id.to_string()).expect("UUID is a valid header value"),
    );
    if let Some(account_id) = account_id {
        headers.insert(
            ACCOUNT_ID_HEADER,
            HeaderValue::from_str(&account_id.to_string()).expect("UUID is a valid header value"),
        );
    }
    headers
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteRequest {
    #[serde(default)]
    account_id: Option<Uuid>,
    request: LlmRequest,
}

#[derive(Serialize)]
struct CompleteResponse {
    request_id: Uuid,
    account_id: Uuid,
    message: AssistantMessage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchGatewayRequest {
    #[serde(default)]
    account_id: Option<Uuid>,
    provider: ProviderId,
    request: SearchRequest,
    #[serde(default)]
    request_options: SearchRequestOptions,
}

#[derive(Serialize)]
struct SearchGatewayResponse {
    request_id: Uuid,
    account_id: Uuid,
    response: ProviderSearchResponse,
}

#[derive(Serialize)]
struct ErrorResponse {
    request_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_id: Option<Uuid>,
    error: GatewayError,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProviderQuery {
    provider: Option<String>,
}

#[derive(Serialize)]
struct ModelsResponse {
    models: Vec<Model>,
}

#[derive(Serialize)]
struct AccountsResponse {
    accounts: Vec<AccountSummary>,
}

#[derive(Serialize)]
struct AccountSummary {
    id: Uuid,
    provider: String,
    name: String,
    is_default: bool,
}

impl From<&ProviderAccount> for AccountSummary {
    fn from(account: &ProviderAccount) -> Self {
        Self {
            id: account.id,
            provider: account.provider.clone(),
            name: account.name.clone(),
            is_default: account.is_default,
        }
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

impl GatewayError {
    fn status_code(&self) -> StatusCode {
        match self.kind {
            GatewayErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
            GatewayErrorKind::InvalidRequest => StatusCode::BAD_REQUEST,
            GatewayErrorKind::Conflict => StatusCode::CONFLICT,
            GatewayErrorKind::UnknownModel
            | GatewayErrorKind::AccountDisabled
            | GatewayErrorKind::AccountProviderMismatch
            | GatewayErrorKind::DefaultAccountNotFound
            | GatewayErrorKind::AuthenticationRequired => StatusCode::UNPROCESSABLE_ENTITY,
            GatewayErrorKind::AccountNotFound => StatusCode::NOT_FOUND,
            GatewayErrorKind::Overloaded => StatusCode::SERVICE_UNAVAILABLE,
            GatewayErrorKind::Provider => {
                if self.can_retry {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::BAD_GATEWAY
                }
            }
            GatewayErrorKind::ProviderTimeout => StatusCode::GATEWAY_TIMEOUT,
            GatewayErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

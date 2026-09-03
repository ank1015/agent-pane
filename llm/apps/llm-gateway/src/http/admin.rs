use std::str::FromStr;

use axum::{
    Json,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::StatusCode,
    response::Response,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{
    account::{
        AccountServiceError, AccountStatus, ProviderCredentials, ProviderKind,
        UnsupportedAccountStatus,
    },
    db::{
        AccountPatch, AccountStoreError, AdminAccount, LlmOperation, LlmRequestFilters,
        RequestQueryError, UsageGroupBy,
    },
    gateway::{GatewayError, validate_provider_config},
};

use super::{
    AppState, ProviderQuery, empty_response, error_response, json_response, parse_provider,
};

pub(super) async fn create_account(
    State(state): State<AppState>,
    payload: Result<Json<CreateAccountRequest>, JsonRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let Json(payload) = match payload {
        Ok(payload) => payload,
        Err(rejection) => {
            return error_response(request_id, GatewayError::bad_json(rejection.body_text()));
        }
    };
    if let Err(error) = validate_provider_config(payload.provider, &payload.config) {
        return error_response(request_id, GatewayError::invalid_request(error));
    }
    let credentials = match payload.credentials.for_provider(payload.provider) {
        Ok(credentials) => credentials,
        Err(error) => return error_response(request_id, error),
    };
    let created = match state
        .accounts
        .create(
            payload.provider,
            payload.name,
            &credentials,
            payload.config,
            payload.status,
            payload.make_default,
        )
        .await
    {
        Ok(account) => account,
        Err(error) => return error_response(request_id, map_service_error(error)),
    };
    let account = match find_view(&state, created.id).await {
        Ok(account) => account,
        Err(error) => return error_response(request_id, error),
    };
    tracing::info!(%request_id, account_id = %account.id, provider = %account.provider, "created provider account");
    json_response(
        StatusCode::CREATED,
        request_id,
        Some(account.id),
        AccountResponse { account },
    )
}

pub(super) async fn list_accounts(
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
        .list_accounts(provider.map(ProviderKind::as_str))
        .await
    {
        Ok(accounts) => {
            let accounts = match accounts
                .into_iter()
                .map(AdminAccountView::try_from)
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(accounts) => accounts,
                Err(error) => return error_response(request_id, error),
            };
            json_response(
                StatusCode::OK,
                request_id,
                None,
                AccountsResponse { accounts },
            )
        }
        Err(error) => error_response(request_id, map_database_error(error)),
    }
}

pub(super) async fn get_account(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match parse_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    match find_view(&state, account_id).await {
        Ok(account) => json_response(
            StatusCode::OK,
            request_id,
            Some(account_id),
            AccountResponse { account },
        ),
        Err(error) => error_response(request_id, error),
    }
}

pub(super) async fn update_account(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    payload: Result<Json<UpdateAccountRequest>, JsonRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match parse_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    let Json(payload) = match payload {
        Ok(payload) => payload,
        Err(rejection) => {
            return error_response(request_id, GatewayError::bad_json(rejection.body_text()));
        }
    };
    if payload.name.is_none() && payload.config.is_none() && payload.status.is_none() {
        return error_response(
            request_id,
            GatewayError::invalid_request("at least one account field must be provided".to_owned()),
        );
    }
    if let Some(name) = &payload.name {
        if name.trim().is_empty() || name != name.trim() {
            return error_response(
                request_id,
                GatewayError::invalid_request(
                    "account name must not be blank or contain surrounding whitespace".to_owned(),
                ),
            );
        }
    }
    if let Some(config) = &payload.config {
        if !config.is_object() {
            return error_response(
                request_id,
                GatewayError::invalid_request("account config must be a JSON object".to_owned()),
            );
        }
        let current = match state.database.find_account(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => {
                return error_response(request_id, GatewayError::account_not_found(account_id));
            }
            Err(error) => return error_response(request_id, map_database_error(error)),
        };
        let provider = match current.provider.parse::<ProviderKind>() {
            Ok(provider) => provider,
            Err(error) => {
                tracing::error!(%error, %account_id, "stored account has unsupported provider");
                return error_response(request_id, GatewayError::internal_error());
            }
        };
        if let Err(error) = validate_provider_config(provider, config) {
            return error_response(request_id, GatewayError::invalid_request(error));
        }
    }
    if let Err(error) = state
        .database
        .update_account(
            account_id,
            AccountPatch {
                name: payload.name,
                config: payload.config,
                status: payload.status.map(|status| status.as_str().to_owned()),
            },
        )
        .await
    {
        return error_response(request_id, map_store_error(error));
    }
    state.gateway.invalidate_account(account_id).await;
    match find_view(&state, account_id).await {
        Ok(account) => json_response(
            StatusCode::OK,
            request_id,
            Some(account_id),
            AccountResponse { account },
        ),
        Err(error) => error_response(request_id, error),
    }
}

pub(super) async fn set_default_account(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match parse_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    if let Err(error) = state.database.set_default_account(account_id).await {
        return error_response(request_id, map_store_error(error));
    }
    match find_view(&state, account_id).await {
        Ok(account) => json_response(
            StatusCode::OK,
            request_id,
            Some(account_id),
            AccountResponse { account },
        ),
        Err(error) => error_response(request_id, error),
    }
}

pub(super) async fn rotate_credentials(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    payload: Result<Json<CredentialsInput>, JsonRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match parse_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    let Json(payload) = match payload {
        Ok(payload) => payload,
        Err(rejection) => {
            return error_response(request_id, GatewayError::bad_json(rejection.body_text()));
        }
    };
    let account = match state.database.find_account(account_id).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            return error_response(request_id, GatewayError::account_not_found(account_id));
        }
        Err(error) => return error_response(request_id, map_database_error(error)),
    };
    let provider = match account.provider.parse::<ProviderKind>() {
        Ok(provider) => provider,
        Err(error) => {
            tracing::error!(%error, %account_id, "stored account has unsupported provider");
            return error_response(request_id, GatewayError::internal_error());
        }
    };
    let credentials = match payload.for_provider(provider) {
        Ok(credentials) => credentials,
        Err(error) => return error_response(request_id, error),
    };
    if let Err(error) = state
        .accounts
        .rotate_credentials(&account, &credentials)
        .await
    {
        return error_response(request_id, map_service_error(error));
    }
    state.gateway.invalidate_account(account_id).await;
    match find_view(&state, account_id).await {
        Ok(account) => json_response(
            StatusCode::OK,
            request_id,
            Some(account_id),
            AccountResponse { account },
        ),
        Err(error) => error_response(request_id, error),
    }
}

pub(super) async fn validate_account(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match parse_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    let account = match state.database.resolve_account("", Some(account_id)).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            return error_response(request_id, GatewayError::account_not_found(account_id));
        }
        Err(error) => return error_response(request_id, map_database_error(error)),
    };
    let provider = match account.provider.parse::<ProviderKind>() {
        Ok(provider) => provider,
        Err(error) => {
            tracing::error!(%error, %account_id, "stored account has unsupported provider");
            return error_response(request_id, GatewayError::internal_error());
        }
    };
    if let Err(error) = validate_provider_config(provider, &account.config) {
        return error_response(request_id, GatewayError::invalid_request(error));
    }
    if let Err(error) = state.accounts.decrypt(&account) {
        return error_response(request_id, map_service_error(error));
    }
    json_response(
        StatusCode::OK,
        request_id,
        Some(account_id),
        AccountValidationResponse {
            account_id,
            valid: true,
            live: false,
        },
    )
}

pub(super) async fn delete_account(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match parse_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    match state.database.remove_account(account_id).await {
        Ok(()) => {
            state.gateway.invalidate_account(account_id).await;
            empty_response(StatusCode::NO_CONTENT, request_id)
        }
        Err(error) => error_response(request_id, map_store_error(error)),
    }
}

pub(super) async fn list_requests(
    State(state): State<AppState>,
    query: Result<Query<RequestQuery>, QueryRejection>,
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
    let filters = match query.filters(None) {
        Ok(filters) => filters,
        Err(error) => return error_response(request_id, error),
    };
    match state
        .database
        .recent_llm_requests(&filters, query.cursor.as_deref(), query.limit)
        .await
    {
        Ok(page) => json_response(StatusCode::OK, request_id, None, page),
        Err(error) => error_response(request_id, map_query_error(error)),
    }
}

pub(super) async fn get_request(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let llm_request_id = match parse_id(path) {
        Ok(id) => id,
        Err(error) => return error_response(request_id, error),
    };
    match state.database.find_llm_request(llm_request_id).await {
        Ok(Some(record)) => json_response(StatusCode::OK, request_id, None, record),
        Ok(None) => error_response(request_id, GatewayError::request_not_found(llm_request_id)),
        Err(error) => error_response(request_id, map_database_error(error)),
    }
}

pub(super) async fn get_usage(
    State(state): State<AppState>,
    query: Result<Query<UsageQuery>, QueryRejection>,
) -> Response {
    usage_response(state, query, None).await
}

pub(super) async fn list_account_requests(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    query: Result<Query<RequestQuery>, QueryRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match parse_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    if let Err(error) = ensure_account_exists(&state, account_id).await {
        return error_response(request_id, error);
    }
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => {
            return error_response(
                request_id,
                GatewayError::invalid_query(rejection.body_text()),
            );
        }
    };
    let filters = match query.filters(Some(account_id)) {
        Ok(filters) => filters,
        Err(error) => return error_response(request_id, error),
    };
    match state
        .database
        .recent_llm_requests(&filters, query.cursor.as_deref(), query.limit)
        .await
    {
        Ok(page) => json_response(StatusCode::OK, request_id, Some(account_id), page),
        Err(error) => error_response(request_id, map_query_error(error)),
    }
}

pub(super) async fn get_account_usage(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    query: Result<Query<UsageQuery>, QueryRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match parse_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    if let Err(error) = ensure_account_exists(&state, account_id).await {
        return error_response(request_id, error);
    }
    usage_response_with_id(state, query, Some(account_id), request_id).await
}

async fn usage_response(
    state: AppState,
    query: Result<Query<UsageQuery>, QueryRejection>,
    account_id: Option<Uuid>,
) -> Response {
    usage_response_with_id(state, query, account_id, Uuid::now_v7()).await
}

async fn usage_response_with_id(
    state: AppState,
    query: Result<Query<UsageQuery>, QueryRejection>,
    account_id: Option<Uuid>,
    request_id: Uuid,
) -> Response {
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => {
            return error_response(
                request_id,
                GatewayError::invalid_query(rejection.body_text()),
            );
        }
    };
    let filters = match query.request.filters(account_id) {
        Ok(filters) => filters,
        Err(error) => return error_response(request_id, error),
    };
    match state.database.usage_report(&filters, query.group_by).await {
        Ok(report) => json_response(StatusCode::OK, request_id, account_id, report),
        Err(error) => error_response(request_id, map_database_error(error)),
    }
}

async fn ensure_account_exists(state: &AppState, account_id: Uuid) -> Result<(), GatewayError> {
    state
        .database
        .find_account(account_id)
        .await
        .map_err(map_database_error)?
        .map(|_| ())
        .ok_or_else(|| GatewayError::account_not_found(account_id))
}

async fn find_view(state: &AppState, account_id: Uuid) -> Result<AdminAccountView, GatewayError> {
    let account = state
        .database
        .find_admin_account(account_id)
        .await
        .map_err(map_database_error)?
        .ok_or_else(|| GatewayError::account_not_found(account_id))?;
    AdminAccountView::try_from(account)
}

fn parse_id(path: Result<Path<Uuid>, PathRejection>) -> Result<Uuid, GatewayError> {
    path.map(|Path(id)| id)
        .map_err(|rejection| GatewayError::invalid_request(rejection.body_text()))
}

fn map_service_error(error: AccountServiceError) -> GatewayError {
    match error {
        AccountServiceError::InvalidName
        | AccountServiceError::ConfigMustBeObject
        | AccountServiceError::CredentialProviderMismatch
        | AccountServiceError::InvalidCredential(_)
        | AccountServiceError::UnsupportedProvider(_) => {
            GatewayError::invalid_request(error.to_string())
        }
        AccountServiceError::Store(error) => map_store_error(error),
        AccountServiceError::Serialization(error) => {
            tracing::error!(%error, "credential serialization failed");
            GatewayError::internal_error()
        }
        AccountServiceError::Vault(error) => {
            tracing::error!(%error, "credential vault operation failed");
            GatewayError::internal_error()
        }
    }
}

fn map_store_error(error: AccountStoreError) -> GatewayError {
    match error {
        AccountStoreError::NotFound(account_id) => GatewayError::account_not_found(account_id),
        AccountStoreError::InactiveCannotBeDefault | AccountStoreError::DefaultCannotBeDisabled => {
            GatewayError::conflict(error.to_string())
        }
        AccountStoreError::Database(error) => map_database_error(error),
    }
}

fn map_database_error(error: sqlx::Error) -> GatewayError {
    if matches!(&error, sqlx::Error::Database(database) if database.is_unique_violation()) {
        return GatewayError::conflict(
            "a provider account with that provider and name already exists".to_owned(),
        );
    }
    tracing::error!(%error, "gateway database operation failed");
    GatewayError::internal_error()
}

fn map_query_error(error: RequestQueryError) -> GatewayError {
    match error {
        RequestQueryError::InvalidCursor
        | RequestQueryError::InvalidLimit
        | RequestQueryError::InvalidOperation => GatewayError::invalid_query(error.to_string()),
        RequestQueryError::Database(error) => map_database_error(error),
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RequestQuery {
    account_id: Option<Uuid>,
    provider: Option<String>,
    model: Option<String>,
    operation: Option<String>,
    status: Option<String>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    cursor: Option<String>,
    limit: Option<u32>,
}

impl RequestQuery {
    fn filters(&self, forced_account_id: Option<Uuid>) -> Result<LlmRequestFilters, GatewayError> {
        if let Some(provider) = self.provider.as_deref() {
            provider
                .parse::<ProviderKind>()
                .map_err(|error| GatewayError::invalid_query(error.to_string()))?;
        }
        let operation = self
            .operation
            .as_deref()
            .map(LlmOperation::from_str)
            .transpose()
            .map_err(|error| GatewayError::invalid_query(error.to_string()))?;
        if let Some(status) = self.status.as_deref() {
            if ![
                "running",
                "succeeded",
                "provider_error",
                "timeout",
                "cancelled",
                "internal_error",
            ]
            .contains(&status)
            {
                return Err(GatewayError::invalid_query(
                    "status filter is invalid".to_owned(),
                ));
            }
        }
        if self.from.zip(self.to).is_some_and(|(from, to)| from >= to) {
            return Err(GatewayError::invalid_query(
                "from must be earlier than to".to_owned(),
            ));
        }
        Ok(LlmRequestFilters {
            account_id: forced_account_id.or(self.account_id),
            provider: self.provider.clone(),
            model: self.model.clone(),
            operation,
            status: self.status.clone(),
            from: self.from,
            to: self.to,
        })
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct UsageQuery {
    #[serde(flatten)]
    request: RequestQuery,
    group_by: Option<UsageGroupBy>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateAccountRequest {
    provider: ProviderKind,
    name: String,
    credentials: CredentialsInput,
    #[serde(default = "empty_object")]
    config: Value,
    #[serde(default = "active_by_default")]
    status: AccountStatus,
    #[serde(default)]
    make_default: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateAccountRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    status: Option<AccountStatus>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(untagged)]
pub(super) enum CredentialsInput {
    Chatgpt(ChatGptCredentialsInput),
    ApiKey(ApiKeyCredentialsInput),
}

impl CredentialsInput {
    fn for_provider(&self, provider: ProviderKind) -> Result<ProviderCredentials, GatewayError> {
        match (provider, self) {
            (ProviderKind::Chatgpt, Self::Chatgpt(credentials)) => {
                if credentials.id_token.trim().is_empty()
                    || credentials.access_token.trim().is_empty()
                    || credentials.refresh_token.trim().is_empty()
                    || credentials.account_id.trim().is_empty()
                {
                    return Err(GatewayError::invalid_request(
                        "ChatGPT credentials require non-empty ID, access, refresh, and account tokens"
                            .to_owned(),
                    ));
                }
                Ok(ProviderCredentials::Chatgpt {
                    access_token: credentials.access_token.clone(),
                    account_id: credentials.account_id.clone(),
                    id_token: credentials.id_token.clone(),
                    refresh_token: credentials.refresh_token.clone(),
                    access_token_expires_at: Some(credentials.access_token_expires_at),
                    refreshed_at: Some(Utc::now()),
                })
            }
            (ProviderKind::Openai, Self::ApiKey(credentials)) => Ok(ProviderCredentials::Openai {
                api_key: credentials.api_key.clone(),
            }),
            (ProviderKind::Fireworks, Self::ApiKey(credentials)) => {
                Ok(ProviderCredentials::Fireworks {
                    api_key: credentials.api_key.clone(),
                })
            }
            _ => Err(GatewayError::invalid_request(format!(
                "credential fields do not match provider {provider}"
            ))),
        }
    }
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub(super) struct ApiKeyCredentialsInput {
    api_key: String,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub(super) struct ChatGptCredentialsInput {
    id_token: String,
    access_token: String,
    refresh_token: String,
    account_id: String,
    #[zeroize(skip)]
    access_token_expires_at: DateTime<Utc>,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

const fn active_by_default() -> AccountStatus {
    AccountStatus::Active
}

#[derive(Serialize)]
struct AccountResponse {
    account: AdminAccountView,
}

#[derive(Serialize)]
struct AccountsResponse {
    accounts: Vec<AdminAccountView>,
}

#[derive(Serialize)]
struct AdminAccountView {
    id: Uuid,
    provider: ProviderKind,
    name: String,
    config: Value,
    status: AccountStatus,
    is_default: bool,
    runtime_revision: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    credential: CredentialMetadata,
}

#[derive(Serialize)]
struct CredentialMetadata {
    version: i64,
    encryption_key_version: i32,
    expires_at: Option<DateTime<Utc>>,
    refreshed_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<AdminAccount> for AdminAccountView {
    type Error = GatewayError;

    fn try_from(account: AdminAccount) -> Result<Self, Self::Error> {
        let provider = account.provider.parse::<ProviderKind>().map_err(|error| {
            tracing::error!(%error, account_id = %account.id, "stored account provider is invalid");
            GatewayError::internal_error()
        })?;
        let status = account.status.parse::<AccountStatus>().map_err(
            |error: UnsupportedAccountStatus| {
                tracing::error!(%error, account_id = %account.id, "stored account status is invalid");
                GatewayError::internal_error()
            },
        )?;
        Ok(Self {
            id: account.id,
            provider,
            name: account.name,
            config: account.config,
            status,
            is_default: account.is_default,
            runtime_revision: account.runtime_revision,
            created_at: account.created_at,
            updated_at: account.updated_at,
            credential: CredentialMetadata {
                version: account.credential_version,
                encryption_key_version: account.encryption_key_version,
                expires_at: account.credential_expires_at,
                refreshed_at: account.credential_refreshed_at,
                updated_at: account.credential_updated_at,
            },
        })
    }
}

#[derive(Serialize)]
struct AccountValidationResponse {
    account_id: Uuid,
    valid: bool,
    live: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{RequestQuery, UsageQuery};
    use crate::{account::ProviderKind, db::UsageGroupBy};

    #[test]
    fn accounting_queries_accept_filters_and_grouping() {
        let query: UsageQuery = serde_json::from_value(json!({
            "provider": "openai",
            "operation": "complete",
            "status": "succeeded",
            "group_by": "model"
        }))
        .unwrap();
        let filters = query.request.filters(None).unwrap();
        assert_eq!(
            filters.provider.as_deref(),
            Some(ProviderKind::Openai.as_str())
        );
        assert_eq!(query.group_by, Some(UsageGroupBy::Model));
    }

    #[test]
    fn accounting_queries_reject_unknown_providers_and_statuses() {
        let provider: RequestQuery =
            serde_json::from_value(json!({"provider": "anthropic"})).unwrap();
        assert!(provider.filters(None).is_err());
        let status: RequestQuery = serde_json::from_value(json!({"status": "mysterious"})).unwrap();
        assert!(status.filters(None).is_err());
    }
}

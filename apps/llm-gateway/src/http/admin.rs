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
    account::{AccountServiceError, ProviderCredentials, ProviderKind},
    db::{AccountPatch, AccountStoreError, AdminAccount, CompletionAccountingQueryError},
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
            payload.enabled,
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

    tracing::info!(
        %request_id,
        account_id = %account.id,
        provider = %account.provider,
        "admin created provider account"
    );
    json_response(
        StatusCode::CREATED,
        request_id,
        Some(account.id),
        AccountResponse {
            account: account.into(),
        },
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
        .list_admin_accounts(provider.map(ProviderKind::as_str))
        .await
    {
        Ok(accounts) => json_response(
            StatusCode::OK,
            request_id,
            None,
            AccountsResponse {
                accounts: accounts.into_iter().map(Into::into).collect(),
            },
        ),
        Err(error) => error_response(request_id, map_database_error(error)),
    }
}

pub(super) async fn get_account(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match account_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    match find_view(&state, account_id).await {
        Ok(account) => json_response(
            StatusCode::OK,
            request_id,
            Some(account_id),
            AccountResponse {
                account: account.into(),
            },
        ),
        Err(error) => error_response(request_id, error),
    }
}

pub(super) async fn get_account_usage(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match account_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    if let Err(error) = find_view(&state, account_id).await {
        return error_response(request_id, error);
    }

    match state.database.completion_usage_summary(account_id).await {
        Ok(summary) => json_response(StatusCode::OK, request_id, Some(account_id), summary),
        Err(error) => error_response(request_id, map_database_error(error)),
    }
}

pub(super) async fn list_account_requests(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    query: Result<Query<CompletionRequestQuery>, QueryRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match account_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => {
            return error_response(
                request_id,
                GatewayError::invalid_query(rejection.body_text()),
            );
        }
    };
    if let Err(error) = find_view(&state, account_id).await {
        return error_response(request_id, error);
    }

    match state
        .database
        .recent_completion_requests(account_id, query.cursor.as_deref(), query.limit)
        .await
    {
        Ok(page) => json_response(StatusCode::OK, request_id, Some(account_id), page),
        Err(error) => error_response(request_id, map_accounting_query_error(error)),
    }
}

pub(super) async fn update_account(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    payload: Result<Json<UpdateAccountRequest>, JsonRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match account_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    let Json(payload) = match payload {
        Ok(payload) => payload,
        Err(rejection) => {
            return error_response(request_id, GatewayError::bad_json(rejection.body_text()));
        }
    };
    if payload.name.is_none() && payload.config.is_none() && payload.enabled.is_none() {
        return error_response(
            request_id,
            GatewayError::invalid_request("at least one account field must be provided".to_owned()),
        );
    }
    if let Some(name) = &payload.name
        && (name.trim().is_empty() || name != name.trim())
    {
        return error_response(
            request_id,
            GatewayError::invalid_request(
                "account name must not be blank or contain surrounding whitespace".to_owned(),
            ),
        );
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
                enabled: payload.enabled,
            },
        )
        .await
    {
        return error_response(request_id, map_store_error(error));
    }
    let account = match find_view(&state, account_id).await {
        Ok(account) => account,
        Err(error) => return error_response(request_id, error),
    };

    tracing::info!(%request_id, %account_id, "admin updated provider account");
    json_response(
        StatusCode::OK,
        request_id,
        Some(account_id),
        AccountResponse {
            account: account.into(),
        },
    )
}

pub(super) async fn set_default_account(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match account_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    if let Err(error) = state.database.set_default_account(account_id).await {
        return error_response(request_id, map_store_error(error));
    }
    let account = match find_view(&state, account_id).await {
        Ok(account) => account,
        Err(error) => return error_response(request_id, error),
    };

    tracing::info!(%request_id, %account_id, "admin selected default provider account");
    json_response(
        StatusCode::OK,
        request_id,
        Some(account_id),
        AccountResponse {
            account: account.into(),
        },
    )
}

pub(super) async fn rotate_credentials(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    payload: Result<Json<CredentialsInput>, JsonRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match account_id(path) {
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
    let account = match find_view(&state, account_id).await {
        Ok(account) => account,
        Err(error) => return error_response(request_id, error),
    };

    tracing::info!(%request_id, %account_id, "admin rotated provider credentials");
    json_response(
        StatusCode::OK,
        request_id,
        Some(account_id),
        AccountResponse {
            account: account.into(),
        },
    )
}

pub(super) async fn delete_account(
    State(state): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Response {
    let request_id = Uuid::now_v7();
    let account_id = match account_id(path) {
        Ok(account_id) => account_id,
        Err(error) => return error_response(request_id, error),
    };
    if let Err(error) = state.database.remove_account(account_id).await {
        return error_response(request_id, map_store_error(error));
    }
    tracing::info!(%request_id, %account_id, "admin deleted provider account");
    empty_response(StatusCode::NO_CONTENT, request_id)
}

async fn find_view(state: &AppState, account_id: Uuid) -> Result<AdminAccount, GatewayError> {
    state
        .database
        .find_admin_account(account_id)
        .await
        .map_err(map_database_error)?
        .ok_or_else(|| GatewayError::account_not_found(account_id))
}

fn account_id(path: Result<Path<Uuid>, PathRejection>) -> Result<Uuid, GatewayError> {
    path.map(|Path(account_id)| account_id)
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
        AccountStoreError::DisabledCannotBeDefault | AccountStoreError::DefaultCannotBeDisabled => {
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
    tracing::error!(%error, "admin account database operation failed");
    GatewayError::internal_error()
}

fn map_accounting_query_error(error: CompletionAccountingQueryError) -> GatewayError {
    match error {
        CompletionAccountingQueryError::InvalidCursor
        | CompletionAccountingQueryError::InvalidLimit => {
            GatewayError::invalid_query(error.to_string())
        }
        CompletionAccountingQueryError::Database(error) => map_database_error(error),
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct CompletionRequestQuery {
    cursor: Option<String>,
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateAccountRequest {
    provider: ProviderKind,
    name: String,
    credentials: CredentialsInput,
    #[serde(default = "empty_object")]
    config: Value,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
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
    enabled: Option<bool>,
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
                        "ChatGPT OAuth credentials must include non-empty ID, access, refresh, and account tokens"
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
            (ProviderKind::Anthropic, Self::ApiKey(credentials)) => {
                Ok(ProviderCredentials::Anthropic {
                    api_key: credentials.api_key.clone(),
                })
            }
            (ProviderKind::Openrouter, Self::ApiKey(credentials)) => {
                Ok(ProviderCredentials::Openrouter {
                    api_key: credentials.api_key.clone(),
                })
            }
            (ProviderKind::Deepseek, Self::ApiKey(credentials)) => {
                Ok(ProviderCredentials::Deepseek {
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

const fn enabled_by_default() -> bool {
    true
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
    provider: String,
    name: String,
    config: Value,
    enabled: bool,
    is_default: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    credential: CredentialMetadata,
}

#[derive(Serialize)]
struct CredentialMetadata {
    version: i64,
    encryption_key_version: i32,
    updated_at: DateTime<Utc>,
}

impl From<AdminAccount> for AdminAccountView {
    fn from(account: AdminAccount) -> Self {
        Self {
            id: account.id,
            provider: account.provider,
            name: account.name,
            config: account.config,
            enabled: account.enabled,
            is_default: account.is_default,
            created_at: account.created_at,
            updated_at: account.updated_at,
            credential: CredentialMetadata {
                version: account.credential_version,
                encryption_key_version: account.encryption_key_version,
                updated_at: account.credential_updated_at,
            },
        }
    }
}

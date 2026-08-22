use std::{collections::HashMap, sync::Arc, time::Duration};

use llm_contracts::{AssistantMessage, LlmError, LlmRequest, LlmTransport, Validate};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, Semaphore};
use uuid::Uuid;

use crate::{
    account::{AccountService, ProviderCredentials, ProviderKind},
    catalog,
    chatgpt_auth::{ChatGptRefreshError, ChatGptTokenRefresher},
    db::{Database, ResolvedAccount},
};

#[derive(Clone)]
pub struct Gateway {
    database: Database,
    accounts: AccountService,
    factory: ProviderFactory,
    transports: Arc<RwLock<HashMap<TransportCacheKey, Arc<dyn LlmTransport>>>>,
    concurrency: Arc<Semaphore>,
    request_timeout: Duration,
    chatgpt_tokens: ChatGptTokenRefresher,
}

impl Gateway {
    #[must_use]
    pub fn new(
        database: Database,
        accounts: AccountService,
        max_concurrent_requests: usize,
        request_timeout: Duration,
        chatgpt_oauth_client_id: String,
        chatgpt_oauth_token_url: String,
    ) -> Self {
        let chatgpt_tokens = ChatGptTokenRefresher::new(
            database.clone(),
            accounts.clone(),
            chatgpt_oauth_client_id,
            chatgpt_oauth_token_url,
        );
        Self {
            database,
            accounts,
            factory: ProviderFactory { request_timeout },
            transports: Arc::new(RwLock::new(HashMap::new())),
            concurrency: Arc::new(Semaphore::new(max_concurrent_requests)),
            request_timeout,
            chatgpt_tokens,
        }
    }

    pub async fn complete(
        &self,
        request: LlmRequest,
        requested_account_id: Option<Uuid>,
    ) -> Result<GatewayCompletion, GatewayError> {
        request
            .validate()
            .map_err(|error| GatewayError::invalid_request(error.to_string()))?;
        let provider = request
            .model
            .provider
            .as_str()
            .parse::<ProviderKind>()
            .map_err(|error| GatewayError::unknown_model(error.to_string()))?;
        if !catalog::contains(&request.model) {
            return Err(GatewayError::unknown_model(format!(
                "model `{}` is not in the curated {} catalog",
                request.model.id, provider
            )));
        }

        let _permit = self
            .concurrency
            .clone()
            .try_acquire_owned()
            .map_err(|_| GatewayError::overloaded())?;

        let account = self
            .database
            .resolve_account(provider.as_str(), requested_account_id)
            .await
            .map_err(GatewayError::database)?
            .ok_or_else(|| match requested_account_id {
                Some(account_id) => GatewayError::account_not_found(account_id),
                None => GatewayError::default_account_not_found(provider),
            })?;
        validate_resolved_account(&account, provider)
            .map_err(|error| error.with_account_id(account.id))?;

        let retry_request = request.clone();
        let (account, transport) = self
            .transport(&account, false)
            .await
            .map_err(|error| error.with_account_id(account.id))?;
        let account_id = account.id;
        let result = tokio::time::timeout(self.request_timeout, transport.complete(request))
            .await
            .map_err(|_| GatewayError::provider_timeout().with_account_id(account.id))?;
        let result = if provider == ProviderKind::Chatgpt
            && result
                .as_ref()
                .is_err_and(|error| error.http_status == Some(401))
        {
            let (refreshed_account, refreshed_transport) = self
                .transport(&account, true)
                .await
                .map_err(|error| error.with_account_id(account.id))?;
            tokio::time::timeout(
                self.request_timeout,
                refreshed_transport.complete(retry_request),
            )
            .await
            .map_err(|_| GatewayError::provider_timeout().with_account_id(account.id))?
            .map_err(|error| GatewayError::provider(error).with_account_id(refreshed_account.id))?
        } else {
            result.map_err(|error| GatewayError::provider(error).with_account_id(account.id))?
        };

        Ok(GatewayCompletion {
            account_id,
            message: result,
        })
    }

    async fn transport(
        &self,
        account: &ResolvedAccount,
        force_chatgpt_refresh: bool,
    ) -> Result<(ResolvedAccount, Arc<dyn LlmTransport>), GatewayError> {
        let (account, credentials) = if account.provider == ProviderKind::Chatgpt.as_str() {
            self.chatgpt_tokens
                .credentials(account, force_chatgpt_refresh)
                .await
                .map_err(GatewayError::chatgpt_refresh)?
        } else {
            (
                account.clone(),
                self.accounts
                    .decrypt(account)
                    .map_err(GatewayError::account_configuration)?,
            )
        };
        let key = TransportCacheKey {
            account_id: account.id,
            credential_version: account.credential_version,
        };
        if let Some(transport) = self.transports.read().await.get(&key).cloned() {
            return Ok((account, transport));
        }

        let built = self
            .factory
            .build(&account.provider, credentials, &account.config)
            .map_err(GatewayError::account_configuration)?;

        let mut transports = self.transports.write().await;
        transports.retain(|existing, _| existing.account_id != account.id || *existing == key);
        let transport = transports.entry(key).or_insert(built).clone();
        Ok((account, transport))
    }
}

fn validate_resolved_account(
    account: &ResolvedAccount,
    requested_provider: ProviderKind,
) -> Result<(), GatewayError> {
    if !account.enabled {
        return Err(GatewayError::account_disabled(account.id));
    }
    if account.provider != requested_provider.as_str() {
        return Err(GatewayError::account_provider_mismatch(
            account.id,
            &account.provider,
            requested_provider.as_str(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct TransportCacheKey {
    account_id: Uuid,
    credential_version: i64,
}

pub struct GatewayCompletion {
    pub account_id: Uuid,
    pub message: AssistantMessage,
}

#[derive(Clone)]
struct ProviderFactory {
    request_timeout: Duration,
}

impl ProviderFactory {
    fn build(
        &self,
        provider: &str,
        credentials: ProviderCredentials,
        config: &serde_json::Value,
    ) -> Result<Arc<dyn LlmTransport>, FactoryError> {
        let provider = provider.parse::<ProviderKind>()?;
        match (provider, &credentials) {
            (ProviderKind::Openai, ProviderCredentials::Openai { api_key }) => {
                let stored: OpenAiStoredConfig = parse_config(config)?;
                let mut config = provider_openai::OpenAiConfig::new(api_key.clone())?;
                if let Some(base_url) = stored.base_url {
                    config = config.with_base_url(base_url)?;
                }
                if let Some(organization) = stored.organization {
                    config = config.with_organization(organization);
                }
                if let Some(project) = stored.project {
                    config = config.with_project(project);
                }
                config = config.with_timeout(self.request_timeout);
                Ok(Arc::new(provider_openai::OpenAiProvider::new(config)?))
            }
            (
                ProviderKind::Chatgpt,
                ProviderCredentials::Chatgpt {
                    access_token,
                    account_id,
                    ..
                },
            ) => {
                let stored: BaseUrlConfig = parse_config(config)?;
                let mut config =
                    provider_chatgpt::ChatGptConfig::new(access_token.clone(), account_id.clone())?;
                if let Some(base_url) = stored.base_url {
                    config = config.with_base_url(base_url)?;
                }
                config = config.with_timeout(self.request_timeout);
                Ok(Arc::new(provider_chatgpt::ChatGptProvider::new(config)?))
            }
            (ProviderKind::Fireworks, ProviderCredentials::Fireworks { api_key }) => {
                let stored: BaseUrlConfig = parse_config(config)?;
                let mut config = provider_fireworks::FireworksConfig::new(api_key.clone())?;
                if let Some(base_url) = stored.base_url {
                    config = config.with_base_url(base_url)?;
                }
                config = config.with_timeout(self.request_timeout);
                Ok(Arc::new(provider_fireworks::FireworksProvider::new(
                    config,
                )?))
            }
            (ProviderKind::Anthropic, ProviderCredentials::Anthropic { api_key }) => {
                let stored: AnthropicStoredConfig = parse_config(config)?;
                let mut config = provider_anthropic::AnthropicConfig::new(api_key.clone())?;
                if let Some(base_url) = stored.base_url {
                    config = config.with_base_url(base_url)?;
                }
                if let Some(api_version) = stored.api_version {
                    config = config.with_api_version(api_version)?;
                }
                if let Some(beta_header) = stored.beta_header {
                    config = config.with_beta_header(beta_header)?;
                }
                config = config.with_timeout(self.request_timeout);
                Ok(Arc::new(provider_anthropic::AnthropicProvider::new(
                    config,
                )?))
            }
            (ProviderKind::Openrouter, ProviderCredentials::Openrouter { api_key }) => {
                let stored: OpenRouterStoredConfig = parse_config(config)?;
                let mut config = provider_openrouter::OpenRouterConfig::new(api_key.clone())?;
                if let Some(base_url) = stored.base_url {
                    config = config.with_base_url(base_url)?;
                }
                if let Some(http_referer) = stored.http_referer {
                    config = config.with_http_referer(http_referer)?;
                }
                if let Some(app_title) = stored.app_title {
                    config = config.with_app_title(app_title)?;
                }
                config = config
                    .with_router_metadata(stored.router_metadata)
                    .with_timeout(self.request_timeout);
                Ok(Arc::new(provider_openrouter::OpenRouterProvider::new(
                    config,
                )?))
            }
            (ProviderKind::Deepseek, ProviderCredentials::Deepseek { api_key }) => {
                let stored: BaseUrlConfig = parse_config(config)?;
                let mut config = provider_deepseek::DeepSeekConfig::new(api_key.clone())?;
                if let Some(base_url) = stored.base_url {
                    config = config.with_base_url(base_url)?;
                }
                config = config.with_timeout(self.request_timeout);
                Ok(Arc::new(provider_deepseek::DeepSeekProvider::new(config)?))
            }
            _ => Err(FactoryError::CredentialProviderMismatch),
        }
    }
}

pub fn validate_provider_config(
    provider: ProviderKind,
    config: &serde_json::Value,
) -> Result<(), String> {
    let credentials = match provider {
        ProviderKind::Openai => ProviderCredentials::Openai {
            api_key: "validation-only".to_owned(),
        },
        ProviderKind::Chatgpt => ProviderCredentials::Chatgpt {
            access_token: "validation-only".to_owned(),
            account_id: "validation-only".to_owned(),
            id_token: String::new(),
            refresh_token: String::new(),
            access_token_expires_at: None,
            refreshed_at: None,
        },
        ProviderKind::Fireworks => ProviderCredentials::Fireworks {
            api_key: "validation-only".to_owned(),
        },
        ProviderKind::Anthropic => ProviderCredentials::Anthropic {
            api_key: "validation-only".to_owned(),
        },
        ProviderKind::Openrouter => ProviderCredentials::Openrouter {
            api_key: "validation-only".to_owned(),
        },
        ProviderKind::Deepseek => ProviderCredentials::Deepseek {
            api_key: "validation-only".to_owned(),
        },
    };
    ProviderFactory {
        request_timeout: Duration::from_secs(1),
    }
    .build(provider.as_str(), credentials, config)
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn parse_config<T>(config: &serde_json::Value) -> Result<T, FactoryError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(config.clone()).map_err(FactoryError::InvalidConfig)
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BaseUrlConfig {
    base_url: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OpenAiStoredConfig {
    base_url: Option<String>,
    organization: Option<String>,
    project: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AnthropicStoredConfig {
    base_url: Option<String>,
    api_version: Option<String>,
    beta_header: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OpenRouterStoredConfig {
    base_url: Option<String>,
    http_referer: Option<String>,
    app_title: Option<String>,
    router_metadata: bool,
}

#[derive(Debug, thiserror::Error)]
enum FactoryError {
    #[error(transparent)]
    UnsupportedProvider(#[from] crate::account::UnsupportedProvider),
    #[error("credential type does not match the account provider")]
    CredentialProviderMismatch,
    #[error("invalid provider account config: {0}")]
    InvalidConfig(serde_json::Error),
    #[error("invalid provider configuration: {0}")]
    Provider(#[from] LlmError),
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayErrorKind {
    Unauthorized,
    InvalidRequest,
    Conflict,
    UnknownModel,
    AccountNotFound,
    AccountDisabled,
    AccountProviderMismatch,
    DefaultAccountNotFound,
    AuthenticationRequired,
    Overloaded,
    Provider,
    ProviderTimeout,
    Internal,
}

#[derive(Debug, Serialize)]
pub struct GatewayError {
    pub kind: GatewayErrorKind,
    pub message: String,
    pub can_retry: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_error: Option<Box<LlmError>>,
    #[serde(skip)]
    pub account_id: Option<Uuid>,
}

impl GatewayError {
    pub(crate) fn bad_json(message: String) -> Self {
        Self::invalid_request(message)
    }

    pub(crate) fn invalid_query(message: String) -> Self {
        Self::invalid_request(message)
    }

    pub(crate) fn internal_error() -> Self {
        Self::internal()
    }

    pub(crate) fn unauthorized() -> Self {
        Self::new(
            GatewayErrorKind::Unauthorized,
            "missing or invalid admin bearer token".to_owned(),
            false,
        )
    }

    pub(crate) fn invalid_request(message: String) -> Self {
        Self::new(GatewayErrorKind::InvalidRequest, message, false)
    }

    pub(crate) fn conflict(message: String) -> Self {
        Self::new(GatewayErrorKind::Conflict, message, false)
    }

    fn unknown_model(message: String) -> Self {
        Self::new(GatewayErrorKind::UnknownModel, message, false)
    }

    pub(crate) fn account_not_found(account_id: Uuid) -> Self {
        Self::new(
            GatewayErrorKind::AccountNotFound,
            format!("provider account {account_id} was not found"),
            false,
        )
    }

    fn default_account_not_found(provider: ProviderKind) -> Self {
        Self::new(
            GatewayErrorKind::DefaultAccountNotFound,
            format!("provider {provider} has no enabled default account"),
            false,
        )
    }

    fn account_disabled(account_id: Uuid) -> Self {
        Self::new(
            GatewayErrorKind::AccountDisabled,
            format!("provider account {account_id} is disabled"),
            false,
        )
    }

    fn account_provider_mismatch(
        account_id: Uuid,
        account_provider: &str,
        request_provider: &str,
    ) -> Self {
        Self::new(
            GatewayErrorKind::AccountProviderMismatch,
            format!(
                "provider account {account_id} belongs to {account_provider}, not {request_provider}"
            ),
            false,
        )
    }

    fn overloaded() -> Self {
        let mut error = Self::new(
            GatewayErrorKind::Overloaded,
            "gateway concurrency capacity is exhausted".to_owned(),
            true,
        );
        error.retry_after_ms = Some(250);
        error
    }

    fn provider(error: LlmError) -> Self {
        Self {
            kind: GatewayErrorKind::Provider,
            message: error.message.clone(),
            can_retry: error.can_retry,
            retry_after_ms: error.retry_after_ms,
            provider_error: Some(Box::new(error)),
            account_id: None,
        }
    }

    fn provider_timeout() -> Self {
        Self::new(
            GatewayErrorKind::ProviderTimeout,
            "provider request exceeded the gateway timeout".to_owned(),
            true,
        )
    }

    fn chatgpt_refresh(error: ChatGptRefreshError) -> Self {
        match error {
            ChatGptRefreshError::ReauthenticationRequired(message) => {
                Self::new(GatewayErrorKind::AuthenticationRequired, message, false)
            }
            error => {
                tracing::error!(%error, "ChatGPT token refresh failed");
                Self::new(
                    GatewayErrorKind::Provider,
                    "ChatGPT authentication could not be refreshed".to_owned(),
                    true,
                )
            }
        }
    }

    fn database(error: sqlx::Error) -> Self {
        tracing::error!(%error, "gateway database operation failed");
        Self::internal()
    }

    fn account_configuration(error: impl std::fmt::Display) -> Self {
        tracing::error!(%error, "provider account configuration failed");
        Self::internal()
    }

    fn internal() -> Self {
        Self::new(
            GatewayErrorKind::Internal,
            "internal gateway error".to_owned(),
            false,
        )
    }

    const fn new(kind: GatewayErrorKind, message: String, can_retry: bool) -> Self {
        Self {
            kind,
            message,
            can_retry,
            retry_after_ms: None,
            provider_error: None,
            account_id: None,
        }
    }

    #[must_use]
    const fn with_account_id(mut self, account_id: Uuid) -> Self {
        self.account_id = Some(account_id);
        self
    }
}

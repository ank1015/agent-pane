use std::{collections::HashMap, sync::Arc, time::Duration};

use llm_contracts::{
    AssistantMessage, LlmError, LlmRequest, LlmTransport, ProviderId, SearchRequest,
    SearchRequestOptions, SearchResponse, SearchTransport, Validate,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::{
    sync::{OwnedSemaphorePermit, RwLock, Semaphore},
    time::timeout,
};
use uuid::Uuid;

use crate::{
    account::{AccountService, AccountStatus, ProviderCredentials, ProviderKind},
    catalog::{self, ProviderCapability},
    chatgpt_auth::{ChatGptRefreshError, ChatGptTokenRefresher},
    db::{Database, LlmOperation, ResolvedAccount},
};

#[derive(Clone)]
struct ConcurrencyGate {
    active: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
    wait_timeout: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct ConcurrencyConfig {
    pub max_active_requests: usize,
    pub max_waiting_requests: usize,
    pub wait_timeout: Duration,
}

impl ConcurrencyGate {
    fn new(config: ConcurrencyConfig) -> Self {
        Self {
            active: Arc::new(Semaphore::new(config.max_active_requests)),
            waiting: Arc::new(Semaphore::new(config.max_waiting_requests)),
            wait_timeout: config.wait_timeout,
        }
    }

    async fn acquire(&self) -> Result<OwnedSemaphorePermit, GatewayError> {
        if let Ok(active_permit) = self.active.clone().try_acquire_owned() {
            return Ok(active_permit);
        }
        let waiting_permit = self
            .waiting
            .clone()
            .try_acquire_owned()
            .map_err(|_| GatewayError::queue_full())?;
        let active_permit = timeout(self.wait_timeout, self.active.clone().acquire_owned())
            .await
            .map_err(|_| GatewayError::queue_timeout())?
            .map_err(|_| GatewayError::internal())?;
        drop(waiting_permit);
        Ok(active_permit)
    }
}

#[derive(Clone)]
pub struct Gateway {
    database: Database,
    accounts: AccountService,
    factory: ProviderFactory,
    transports: Arc<RwLock<HashMap<TransportCacheKey, ProviderTransport>>>,
    concurrency: ConcurrencyGate,
    request_timeout: Duration,
    chatgpt_tokens: ChatGptTokenRefresher,
}

impl Gateway {
    pub fn new(
        database: Database,
        accounts: AccountService,
        concurrency: ConcurrencyConfig,
        request_timeout: Duration,
        chatgpt_oauth_client_id: String,
        chatgpt_oauth_token_url: String,
    ) -> Result<Self, reqwest::Error> {
        let chatgpt_tokens = ChatGptTokenRefresher::new(
            database.clone(),
            accounts.clone(),
            chatgpt_oauth_client_id,
            chatgpt_oauth_token_url,
        )?;
        Ok(Self {
            database,
            accounts,
            factory: ProviderFactory { request_timeout },
            transports: Arc::new(RwLock::new(HashMap::new())),
            concurrency: ConcurrencyGate::new(concurrency),
            request_timeout,
            chatgpt_tokens,
        })
    }

    pub async fn complete(
        &self,
        request_id: Uuid,
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
                "model `{}` is not in the curated {provider} catalog",
                request.model.id
            )));
        }
        let _permit = self.concurrency.acquire().await?;
        let account = self.resolve_account(provider, requested_account_id).await?;
        let account_id = account.id;
        let requested_model = request.model.clone();
        let labels = Value::Object(
            request
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                .collect::<Map<_, _>>(),
        );
        self.database
            .begin_llm_request(
                request_id,
                LlmOperation::Complete,
                account_id,
                provider.as_str(),
                requested_model.id.as_str(),
                &labels,
            )
            .await
            .map_err(|error| GatewayError::database(error).with_account_id(account_id))?;
        let started = std::time::Instant::now();
        let result = self.execute_completion(account, provider, request).await;
        match result {
            Ok(message) => {
                if let Err(error) = self
                    .database
                    .finish_llm_request_success(
                        request_id,
                        Some(message.model.provider.as_str()),
                        Some(message.model.id.as_str()),
                        Some(message.id.as_str()),
                        Some(message.duration_ms),
                        elapsed_milliseconds(started),
                        message.usage.as_ref(),
                    )
                    .await
                {
                    tracing::error!(%error, %request_id, %account_id, "could not finalize successful LLM accounting");
                }
                Ok(GatewayCompletion {
                    account_id,
                    message,
                })
            }
            Err(error) => {
                self.record_failure(request_id, account_id, started, &error)
                    .await;
                Err(error)
            }
        }
    }

    pub async fn search(
        &self,
        request_id: Uuid,
        provider_id: ProviderId,
        request: SearchRequest,
        options: SearchRequestOptions,
        requested_account_id: Option<Uuid>,
    ) -> Result<GatewaySearch, GatewayError> {
        request
            .validate()
            .map_err(|error| GatewayError::invalid_request(error.to_string()))?;
        let provider = provider_id
            .as_str()
            .parse::<ProviderKind>()
            .map_err(|error| GatewayError::invalid_request(error.to_string()))?;
        if !catalog::supports(provider, ProviderCapability::Search) {
            return Err(GatewayError::unsupported_capability(format!(
                "provider `{provider}` does not support search"
            )));
        }
        let model_supported = match provider {
            ProviderKind::Openai => provider_openai::find_model(&request.model).is_some(),
            ProviderKind::Chatgpt => provider_chatgpt::find_model(&request.model).is_some(),
            ProviderKind::Fireworks => false,
        };
        if !model_supported {
            return Err(GatewayError::unknown_model(format!(
                "model `{}` is not in the curated {provider} catalog",
                request.model
            )));
        }
        let _permit = self.concurrency.acquire().await?;
        let account = self.resolve_account(provider, requested_account_id).await?;
        let account_id = account.id;
        let labels = options.originator.as_ref().map_or_else(
            || Value::Object(Map::new()),
            |originator| {
                Value::Object(Map::from_iter([(
                    "originator".to_owned(),
                    Value::String(originator.clone()),
                )]))
            },
        );
        self.database
            .begin_llm_request(
                request_id,
                LlmOperation::Search,
                account_id,
                provider.as_str(),
                &request.model,
                &labels,
            )
            .await
            .map_err(|error| GatewayError::database(error).with_account_id(account_id))?;
        let requested_model = request.model.clone();
        let started = std::time::Instant::now();
        let result = self
            .execute_search(account, provider, request, options)
            .await;
        match result {
            Ok(response) => {
                if let Err(error) = self
                    .database
                    .finish_llm_request_success(
                        request_id,
                        Some(provider.as_str()),
                        Some(&requested_model),
                        None,
                        None,
                        elapsed_milliseconds(started),
                        None,
                    )
                    .await
                {
                    tracing::error!(%error, %request_id, %account_id, "could not finalize successful search accounting");
                }
                Ok(GatewaySearch {
                    account_id,
                    response,
                })
            }
            Err(error) => {
                self.record_failure(request_id, account_id, started, &error)
                    .await;
                Err(error)
            }
        }
    }

    pub async fn invalidate_account(&self, account_id: Uuid) {
        self.transports
            .write()
            .await
            .retain(|key, _| key.account_id != account_id);
    }

    async fn resolve_account(
        &self,
        provider: ProviderKind,
        requested_account_id: Option<Uuid>,
    ) -> Result<ResolvedAccount, GatewayError> {
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
        Ok(account)
    }

    async fn execute_completion(
        &self,
        account: ResolvedAccount,
        provider: ProviderKind,
        request: LlmRequest,
    ) -> Result<AssistantMessage, GatewayError> {
        let retry_request = request.clone();
        let (account, transport) = self
            .transport(&account, false)
            .await
            .map_err(|error| error.with_account_id(account.id))?;
        let result =
            tokio::time::timeout(self.request_timeout, transport.completion.complete(request))
                .await
                .map_err(|_| GatewayError::provider_timeout().with_account_id(account.id))?;
        if provider == ProviderKind::Chatgpt
            && result
                .as_ref()
                .is_err_and(|error| error.http_status == Some(401))
        {
            let (refreshed_account, refreshed_transport) = self
                .transport(&account, true)
                .await
                .map_err(|error| error.with_account_id(account.id))?;
            return tokio::time::timeout(
                self.request_timeout,
                refreshed_transport.completion.complete(retry_request),
            )
            .await
            .map_err(|_| GatewayError::provider_timeout().with_account_id(account.id))?
            .map_err(|error| GatewayError::provider(error).with_account_id(refreshed_account.id));
        }
        result.map_err(|error| GatewayError::provider(error).with_account_id(account.id))
    }

    async fn execute_search(
        &self,
        account: ResolvedAccount,
        provider: ProviderKind,
        request: SearchRequest,
        options: SearchRequestOptions,
    ) -> Result<SearchResponse, GatewayError> {
        let retry_request = request.clone();
        let retry_options = options.clone();
        let (account, transport) = self
            .transport(&account, false)
            .await
            .map_err(|error| error.with_account_id(account.id))?;
        let search = transport.search.ok_or_else(|| {
            GatewayError::unsupported_capability(format!(
                "provider `{provider}` does not support search"
            ))
            .with_account_id(account.id)
        })?;
        let result = tokio::time::timeout(self.request_timeout, search.search(request, options))
            .await
            .map_err(|_| GatewayError::provider_timeout().with_account_id(account.id))?;
        if provider == ProviderKind::Chatgpt
            && result
                .as_ref()
                .is_err_and(|error| error.http_status == Some(401))
        {
            let (refreshed_account, refreshed_transport) = self
                .transport(&account, true)
                .await
                .map_err(|error| error.with_account_id(account.id))?;
            let refreshed_search = refreshed_transport.search.ok_or_else(|| {
                GatewayError::unsupported_capability(format!(
                    "provider `{provider}` does not support search"
                ))
                .with_account_id(account.id)
            })?;
            return tokio::time::timeout(
                self.request_timeout,
                refreshed_search.search(retry_request, retry_options),
            )
            .await
            .map_err(|_| GatewayError::provider_timeout().with_account_id(account.id))?
            .map_err(|error| GatewayError::provider(error).with_account_id(refreshed_account.id));
        }
        result.map_err(|error| GatewayError::provider(error).with_account_id(account.id))
    }

    async fn transport(
        &self,
        account: &ResolvedAccount,
        force_chatgpt_refresh: bool,
    ) -> Result<(ResolvedAccount, ProviderTransport), GatewayError> {
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
            runtime_revision: account.runtime_revision,
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

    async fn record_failure(
        &self,
        request_id: Uuid,
        account_id: Uuid,
        started: std::time::Instant,
        error: &GatewayError,
    ) {
        if let Err(database_error) = self
            .database
            .finish_llm_request_failure(
                request_id,
                error.accounting_status(),
                error.kind.as_str(),
                error
                    .provider_error
                    .as_deref()
                    .and_then(|provider| provider.provider_code.as_deref()),
                error.can_retry,
                elapsed_milliseconds(started),
            )
            .await
        {
            tracing::error!(%database_error, %request_id, %account_id, "could not finalize failed LLM accounting");
        }
    }
}

fn elapsed_milliseconds(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn validate_resolved_account(
    account: &ResolvedAccount,
    requested_provider: ProviderKind,
) -> Result<(), GatewayError> {
    if account.provider != requested_provider.as_str() {
        return Err(GatewayError::account_provider_mismatch(
            account.id,
            &account.provider,
            requested_provider.as_str(),
        ));
    }
    match account.status.parse::<AccountStatus>() {
        Ok(AccountStatus::Active) => Ok(()),
        Ok(AccountStatus::Disabled) => Err(GatewayError::account_disabled(account.id)),
        Ok(AccountStatus::ReauthRequired) => Err(GatewayError::authentication_required(
            "provider account requires authentication".to_owned(),
        )),
        Err(error) => {
            tracing::error!(%error, account_id = %account.id, "stored account status is invalid");
            Err(GatewayError::internal())
        }
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct TransportCacheKey {
    account_id: Uuid,
    runtime_revision: i64,
    credential_version: i64,
}

#[derive(Clone)]
struct ProviderTransport {
    completion: Arc<dyn LlmTransport>,
    search: Option<Arc<dyn SearchTransport>>,
}

pub struct GatewayCompletion {
    pub account_id: Uuid,
    pub message: AssistantMessage,
}

pub struct GatewaySearch {
    pub account_id: Uuid,
    pub response: SearchResponse,
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
        config: &Value,
    ) -> Result<ProviderTransport, FactoryError> {
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
                let provider = Arc::new(provider_openai::OpenAiProvider::new(config)?);
                Ok(ProviderTransport {
                    completion: provider.clone(),
                    search: Some(provider),
                })
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
                let provider = Arc::new(provider_chatgpt::ChatGptProvider::new(config)?);
                Ok(ProviderTransport {
                    completion: provider.clone(),
                    search: Some(provider),
                })
            }
            (ProviderKind::Fireworks, ProviderCredentials::Fireworks { api_key }) => {
                let stored: BaseUrlConfig = parse_config(config)?;
                let mut config = provider_fireworks::FireworksConfig::new(api_key.clone())?;
                if let Some(base_url) = stored.base_url {
                    config = config.with_base_url(base_url)?;
                }
                config = config.with_timeout(self.request_timeout);
                Ok(ProviderTransport {
                    completion: Arc::new(provider_fireworks::FireworksProvider::new(config)?),
                    search: None,
                })
            }
            _ => Err(FactoryError::CredentialProviderMismatch),
        }
    }
}

pub fn validate_provider_config(provider: ProviderKind, config: &Value) -> Result<(), String> {
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
    };
    ProviderFactory {
        request_timeout: Duration::from_secs(1),
    }
    .build(provider.as_str(), credentials, config)
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn parse_config<T>(config: &Value) -> Result<T, FactoryError>
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
    AuthenticationRateLimited,
    InvalidRequest,
    Conflict,
    UnsupportedCapability,
    UnknownModel,
    AccountNotFound,
    RequestNotFound,
    AccountDisabled,
    AccountProviderMismatch,
    DefaultAccountNotFound,
    AuthenticationRequired,
    Overloaded,
    Provider,
    ProviderTimeout,
    Internal,
}

impl GatewayErrorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::AuthenticationRateLimited => "authentication_rate_limited",
            Self::InvalidRequest => "invalid_request",
            Self::Conflict => "conflict",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::UnknownModel => "unknown_model",
            Self::AccountNotFound => "account_not_found",
            Self::RequestNotFound => "request_not_found",
            Self::AccountDisabled => "account_disabled",
            Self::AccountProviderMismatch => "account_provider_mismatch",
            Self::DefaultAccountNotFound => "default_account_not_found",
            Self::AuthenticationRequired => "authentication_required",
            Self::Overloaded => "overloaded",
            Self::Provider => "provider",
            Self::ProviderTimeout => "provider_timeout",
            Self::Internal => "internal",
        }
    }
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
            "missing or invalid bearer token".to_owned(),
            false,
        )
    }

    pub(crate) fn authentication_rate_limited() -> Self {
        let mut error = Self::new(
            GatewayErrorKind::AuthenticationRateLimited,
            "too many failed authentication attempts".to_owned(),
            true,
        );
        error.retry_after_ms = Some(60_000);
        error
    }

    pub(crate) fn invalid_request(message: String) -> Self {
        Self::new(GatewayErrorKind::InvalidRequest, message, false)
    }

    pub(crate) fn conflict(message: String) -> Self {
        Self::new(GatewayErrorKind::Conflict, message, false)
    }

    fn unsupported_capability(message: String) -> Self {
        Self::new(GatewayErrorKind::UnsupportedCapability, message, false)
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

    pub(crate) fn request_not_found(request_id: Uuid) -> Self {
        Self::new(
            GatewayErrorKind::RequestNotFound,
            format!("LLM request {request_id} was not found"),
            false,
        )
    }

    fn default_account_not_found(provider: ProviderKind) -> Self {
        Self::new(
            GatewayErrorKind::DefaultAccountNotFound,
            format!("provider {provider} has no default account"),
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

    fn authentication_required(message: String) -> Self {
        Self::new(GatewayErrorKind::AuthenticationRequired, message, false)
    }

    fn queue_full() -> Self {
        let mut error = Self::new(
            GatewayErrorKind::Overloaded,
            "gateway request queue is full".to_owned(),
            true,
        );
        error.retry_after_ms = Some(1_000);
        error
    }

    fn queue_timeout() -> Self {
        let mut error = Self::new(
            GatewayErrorKind::Overloaded,
            "gateway request exceeded the maximum queue wait".to_owned(),
            true,
        );
        error.retry_after_ms = Some(1_000);
        error
    }

    fn provider(mut error: LlmError) -> Self {
        // The provider packages retain raw error bodies for direct callers and
        // diagnostics. The network gateway deliberately does not echo them.
        error.native_error = None;
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
                Self::authentication_required(message)
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

    fn accounting_status(&self) -> &'static str {
        match self.kind {
            GatewayErrorKind::Provider | GatewayErrorKind::AuthenticationRequired => {
                "provider_error"
            }
            GatewayErrorKind::ProviderTimeout => "timeout",
            _ => "internal_error",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use llm_contracts::LlmError;
    use serde_json::json;

    use super::{
        ConcurrencyConfig, ConcurrencyGate, GatewayError, GatewayErrorKind,
        validate_provider_config,
    };
    use crate::account::ProviderKind;

    #[test]
    fn provider_configs_reject_unknown_keys() {
        assert!(validate_provider_config(ProviderKind::Openai, &json!({})).is_ok());
        assert!(validate_provider_config(ProviderKind::Openai, &json!({"unknown": true})).is_err());
        assert!(validate_provider_config(ProviderKind::Fireworks, &json!({})).is_ok());
        assert!(
            validate_provider_config(
                ProviderKind::Openai,
                &json!({"base_url": "http://api.example.com/v1"}),
            )
            .is_err()
        );
        assert!(
            validate_provider_config(
                ProviderKind::Fireworks,
                &json!({"base_url": "http://127.0.0.1:8080/v1"}),
            )
            .is_ok()
        );
    }

    #[test]
    fn gateway_errors_do_not_expose_raw_provider_bodies() {
        let error = GatewayError::provider(LlmError {
            message: "provider rejected the request".to_owned(),
            provider_code: Some("invalid".to_owned()),
            provider_type: Some("request_error".to_owned()),
            http_status: Some(400),
            can_retry: false,
            retry_after_ms: None,
            native_error: Some(Box::new(json!({"secret": "must-not-cross-gateway"}))),
        });
        assert!(
            error
                .provider_error
                .expect("provider metadata remains available")
                .native_error
                .is_none()
        );
    }

    #[tokio::test]
    async fn queued_request_continues_when_capacity_is_released() {
        let gate = test_gate(Duration::from_secs(1));
        let active = gate.acquire().await.expect("first request is admitted");
        let waiting_gate = gate.clone();
        let waiting = tokio::spawn(async move { waiting_gate.acquire().await });

        wait_until_queue_is_full(&gate).await;
        assert!(!waiting.is_finished());
        drop(active);

        let admitted = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("queued request is woken")
            .expect("queued task does not panic")
            .expect("queued request is admitted");
        drop(admitted);
    }

    #[tokio::test]
    async fn request_is_rejected_when_waiting_queue_is_full() {
        let gate = test_gate(Duration::from_secs(1));
        let active = gate.acquire().await.expect("first request is admitted");
        let waiting_gate = gate.clone();
        let waiting = tokio::spawn(async move { waiting_gate.acquire().await });

        wait_until_queue_is_full(&gate).await;
        let error = gate.acquire().await.expect_err("queue is bounded");
        assert!(matches!(error.kind, GatewayErrorKind::Overloaded));
        assert_eq!(error.message, "gateway request queue is full");

        drop(active);
        let _admitted = waiting
            .await
            .expect("queued task does not panic")
            .expect("queued request is admitted after release");
    }

    #[tokio::test]
    async fn queued_request_is_rejected_after_wait_timeout() {
        let gate = test_gate(Duration::from_millis(25));
        let _active = gate.acquire().await.expect("first request is admitted");

        let error = gate
            .acquire()
            .await
            .expect_err("queue wait should time out");
        assert!(matches!(error.kind, GatewayErrorKind::Overloaded));
        assert_eq!(
            error.message,
            "gateway request exceeded the maximum queue wait"
        );
    }

    async fn wait_until_queue_is_full(gate: &ConcurrencyGate) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while gate.waiting.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("request enters the waiting queue");
    }

    fn test_gate(wait_timeout: Duration) -> ConcurrencyGate {
        ConcurrencyGate::new(ConcurrencyConfig {
            max_active_requests: 1,
            max_waiting_requests: 1,
            wait_timeout,
        })
    }
}

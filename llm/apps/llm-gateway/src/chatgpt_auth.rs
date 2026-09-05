use std::{collections::HashMap, sync::Arc};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    account::{AccountService, AccountServiceError, ProviderCredentials, ProviderKind},
    db::{AccountStoreError, Database, ResolvedAccount},
};

const REFRESH_WINDOW: Duration = Duration::minutes(5);
const FALLBACK_REFRESH_INTERVAL: Duration = Duration::days(8);

#[derive(Clone)]
pub(crate) struct ChatGptTokenRefresher {
    database: Database,
    accounts: AccountService,
    client: reqwest::Client,
    client_id: String,
    token_url: String,
    locks: Arc<Mutex<HashMap<Uuid, Arc<Mutex<()>>>>>,
}

impl ChatGptTokenRefresher {
    pub(crate) fn new(
        database: Database,
        accounts: AccountService,
        client_id: String,
        token_url: String,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            database,
            accounts,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            client_id,
            token_url,
            locks: Arc::default(),
        })
    }

    pub(crate) async fn credentials(
        &self,
        account: &ResolvedAccount,
        force_refresh: bool,
    ) -> Result<(ResolvedAccount, ProviderCredentials), ChatGptRefreshError> {
        let credentials = self.accounts.decrypt(account)?;
        if !force_refresh && !needs_refresh(&credentials, Utc::now())? {
            return Ok((account.clone(), credentials));
        }

        let account_lock = self.account_lock(account.id).await;
        let _refresh_guard = account_lock.lock().await;
        let mut distributed_lock = self.database.pool().begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("llm-gateway:chatgpt-refresh:{}", account.id))
            .execute(&mut *distributed_lock)
            .await?;
        let current = self
            .database
            .resolve_account(ProviderKind::Chatgpt.as_str(), Some(account.id))
            .await?
            .ok_or(AccountStoreError::NotFound(account.id))?;
        let current_credentials = self.accounts.decrypt(&current)?;
        if force_refresh && current.credential_version != account.credential_version {
            return Ok((current, current_credentials));
        }
        if !force_refresh && !needs_refresh(&current_credentials, Utc::now())? {
            return Ok((current, current_credentials));
        }

        let refreshed = match self.refresh(current_credentials).await {
            Ok(refreshed) => refreshed,
            Err(error @ ChatGptRefreshError::ReauthenticationRequired(_)) => {
                if let Err(database_error) = self
                    .database
                    .mark_reauthentication_required(account.id)
                    .await
                {
                    tracing::error!(
                        %database_error,
                        account_id = %account.id,
                        "could not persist ChatGPT reauthentication state"
                    );
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let account_record = self
            .database
            .find_account(account.id)
            .await?
            .ok_or(AccountStoreError::NotFound(account.id))?;
        self.accounts
            .rotate_credentials(&account_record, &refreshed)
            .await?;
        let updated = self
            .database
            .resolve_account(ProviderKind::Chatgpt.as_str(), Some(account.id))
            .await?
            .ok_or(AccountStoreError::NotFound(account.id))?;
        Ok((updated, refreshed))
    }

    async fn account_lock(&self, account_id: Uuid) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks
            .entry(account_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn refresh(
        &self,
        credentials: ProviderCredentials,
    ) -> Result<ProviderCredentials, ChatGptRefreshError> {
        let ProviderCredentials::Chatgpt {
            access_token,
            account_id,
            id_token,
            refresh_token,
            access_token_expires_at,
            ..
        } = &credentials
        else {
            return Err(ChatGptRefreshError::CredentialProviderMismatch);
        };
        let mut access_token = access_token.clone();
        let account_id = account_id.clone();
        let mut id_token = id_token.clone();
        let mut refresh_token = refresh_token.clone();
        let mut access_token_expires_at = access_token_expires_at.to_owned();
        if refresh_token.trim().is_empty() {
            return Err(ChatGptRefreshError::ReauthenticationRequired(
                "ChatGPT credentials cannot be refreshed; sign in again".to_owned(),
            ));
        }

        let response = self
            .client
            .post(&self.token_url)
            .header("originator", "codex_cli_rs")
            .json(&RefreshRequest {
                client_id: &self.client_id,
                grant_type: "refresh_token",
                refresh_token: &refresh_token,
            })
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            if status == StatusCode::UNAUTHORIZED
                || (status == StatusCode::BAD_REQUEST && is_permanent_refresh_error(&body))
            {
                return Err(ChatGptRefreshError::ReauthenticationRequired(
                    "ChatGPT authentication has expired; sign in again".to_owned(),
                ));
            }
            return Err(ChatGptRefreshError::Rejected { status, body });
        }

        let refreshed: RefreshResponse = response.json().await?;
        if let Some(token) = refreshed.id_token {
            id_token = token;
        }
        if let Some(token) = refreshed.access_token {
            access_token_expires_at = Some(
                try_parse_jwt_expiration(&token).ok_or(ChatGptRefreshError::InvalidAccessToken)?,
            );
            access_token = token;
        }
        if let Some(token) = refreshed.refresh_token {
            refresh_token = token;
        }

        Ok(ProviderCredentials::Chatgpt {
            access_token,
            account_id,
            id_token,
            refresh_token,
            access_token_expires_at,
            refreshed_at: Some(Utc::now()),
        })
    }
}

fn needs_refresh(
    credentials: &ProviderCredentials,
    now: DateTime<Utc>,
) -> Result<bool, ChatGptRefreshError> {
    let ProviderCredentials::Chatgpt {
        access_token,
        access_token_expires_at,
        refreshed_at,
        ..
    } = credentials
    else {
        return Err(ChatGptRefreshError::CredentialProviderMismatch);
    };
    let expires_at = access_token_expires_at
        .to_owned()
        .or_else(|| try_parse_jwt_expiration(access_token));
    Ok(expires_at.map_or_else(
        || refreshed_at.is_some_and(|refreshed| refreshed <= now - FALLBACK_REFRESH_INTERVAL),
        |expires| expires <= now + REFRESH_WINDOW,
    ))
}

fn try_parse_jwt_expiration(token: &str) -> Option<DateTime<Utc>> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: JwtClaims = serde_json::from_slice(&decoded).ok()?;
    claims
        .exp
        .and_then(|expiration| DateTime::from_timestamp(expiration, 0))
}

fn is_permanent_refresh_error(body: &str) -> bool {
    let normalized = body.to_ascii_lowercase();
    normalized.contains("invalid_grant")
        || normalized.contains("refresh_token_expired")
        || normalized.contains("refresh_token_reused")
        || normalized.contains("refresh_token_invalidated")
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'a str,
    grant_type: &'static str,
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct JwtClaims {
    exp: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ChatGptRefreshError {
    #[error("credential type does not match ChatGPT")]
    CredentialProviderMismatch,
    #[error("ChatGPT access token is not a valid JWT")]
    InvalidAccessToken,
    #[error("{0}")]
    ReauthenticationRequired(String),
    #[error("ChatGPT token endpoint rejected refresh with status {status}")]
    Rejected { status: StatusCode, body: String },
    #[error(transparent)]
    Request(#[from] reqwest::Error),
    #[error(transparent)]
    Account(#[from] AccountServiceError),
    #[error(transparent)]
    Store(#[from] AccountStoreError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};

    use super::{
        FALLBACK_REFRESH_INTERVAL, REFRESH_WINDOW, is_permanent_refresh_error, needs_refresh,
    };
    use crate::account::ProviderCredentials;

    #[test]
    fn refreshes_inside_the_expiration_window() {
        let now = Utc.with_ymd_and_hms(2026, 8, 23, 0, 0, 0).unwrap();
        let credentials = credentials(Some(now + REFRESH_WINDOW), Some(now - Duration::minutes(1)));
        assert!(needs_refresh(&credentials, now).unwrap());
    }

    #[test]
    fn leaves_fresh_access_tokens_unchanged() {
        let now = Utc.with_ymd_and_hms(2026, 8, 23, 0, 0, 0).unwrap();
        let credentials = credentials(Some(now + REFRESH_WINDOW + Duration::seconds(1)), Some(now));
        assert!(!needs_refresh(&credentials, now).unwrap());
    }

    #[test]
    fn legacy_tokens_without_expiry_remain_usable_until_the_provider_rejects_them() {
        let now = Utc.with_ymd_and_hms(2026, 8, 23, 0, 0, 0).unwrap();
        assert!(!needs_refresh(&credentials(None, None), now).unwrap());
    }

    #[test]
    fn fallback_refreshes_old_tokens_without_an_expiry_claim() {
        let now = Utc.with_ymd_and_hms(2026, 8, 23, 0, 0, 0).unwrap();
        let credentials = credentials(None, Some(now - FALLBACK_REFRESH_INTERVAL));
        assert!(needs_refresh(&credentials, now).unwrap());
    }

    #[test]
    fn recognizes_rotated_or_revoked_refresh_tokens_as_permanent_failures() {
        assert!(is_permanent_refresh_error(
            r#"{"error":"refresh_token_reused"}"#
        ));
        assert!(is_permanent_refresh_error(r#"{"error":"invalid_grant"}"#));
        assert!(!is_permanent_refresh_error(
            r#"{"error":"temporarily_unavailable"}"#
        ));
    }

    fn credentials(
        access_token_expires_at: Option<chrono::DateTime<Utc>>,
        refreshed_at: Option<chrono::DateTime<Utc>>,
    ) -> ProviderCredentials {
        ProviderCredentials::Chatgpt {
            access_token: "legacy-token".to_owned(),
            account_id: "account-id".to_owned(),
            id_token: String::new(),
            refresh_token: "refresh-token".to_owned(),
            access_token_expires_at,
            refreshed_at,
        }
    }
}

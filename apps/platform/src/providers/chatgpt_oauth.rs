use std::{collections::HashMap, sync::Arc};

use axum::{
    Router,
    extract::{Query, State},
    response::Html,
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

use crate::upstream::llm_gateway::{ChatGptGatewayCredentials, LlmGatewayClient, LlmGatewayError};

const LOGIN_TTL: Duration = Duration::minutes(15);
const CHATGPT_SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
const OAUTH_ORIGINATOR: &str = "codex_cli_rs";

#[derive(Clone)]
pub struct ChatGptLoginService {
    gateway: LlmGatewayClient,
    http: reqwest::Client,
    client_id: String,
    issuer: Url,
    redirect_uri: Url,
    dashboard_origin: Url,
    attempts: Arc<Mutex<HashMap<Uuid, LoginAttempt>>>,
}

impl ChatGptLoginService {
    pub fn new(
        gateway: LlmGatewayClient,
        client_id: String,
        issuer: Url,
        redirect_uri: Url,
        dashboard_origin: Url,
    ) -> Self {
        Self {
            gateway,
            http: reqwest::Client::new(),
            client_id,
            issuer,
            redirect_uri,
            dashboard_origin,
            attempts: Arc::default(),
        }
    }

    pub async fn start(&self, name: String) -> Result<LoginStart, LoginError> {
        if name.trim().is_empty() || name != name.trim() {
            return Err(LoginError::InvalidName);
        }

        let login_id = Uuid::now_v7();
        let state = random_url_safe(32);
        let verifier = random_url_safe(64);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let authorization_url = self.authorization_url(&state, &challenge)?;
        let now = Utc::now();
        let mut attempts = self.attempts.lock().await;
        attempts.retain(|_, attempt| attempt.created_at > now - LOGIN_TTL);
        attempts.insert(
            login_id,
            LoginAttempt {
                name,
                state,
                verifier,
                created_at: now,
                status: AttemptStatus::Pending,
            },
        );

        Ok(LoginStart {
            login_id,
            authorization_url: authorization_url.into(),
        })
    }

    pub async fn status(&self, login_id: Uuid) -> Result<LoginStatus, LoginError> {
        let attempts = self.attempts.lock().await;
        let attempt = attempts.get(&login_id).ok_or(LoginError::NotFound)?;
        if attempt.created_at <= Utc::now() - LOGIN_TTL {
            return Err(LoginError::Expired);
        }
        Ok(attempt.status.as_response())
    }

    pub async fn cancel(&self, login_id: Uuid) -> Result<(), LoginError> {
        self.attempts
            .lock()
            .await
            .remove(&login_id)
            .map(|_| ())
            .ok_or(LoginError::NotFound)
    }

    async fn complete(&self, query: CallbackQuery) -> Result<(), LoginError> {
        if let Some(error) = query.error {
            let message = query.error_description.unwrap_or(error);
            self.fail_by_state(&query.state, message.clone()).await;
            return Err(LoginError::AuthorizationRejected(message));
        }
        let code = query.code.ok_or(LoginError::MissingAuthorizationCode)?;

        let (login_id, name, verifier) = {
            let mut attempts = self.attempts.lock().await;
            let (login_id, attempt) = attempts
                .iter_mut()
                .find(|(_, attempt)| attempt.state == query.state)
                .ok_or(LoginError::InvalidState)?;
            if !matches!(attempt.status, AttemptStatus::Pending) {
                return Err(LoginError::AlreadyCompleted);
            }
            attempt.status = AttemptStatus::Exchanging;
            (*login_id, attempt.name.clone(), attempt.verifier.clone())
        };

        let result = self.exchange_and_create(&name, &code, &verifier).await;
        let mut attempts = self.attempts.lock().await;
        let Some(attempt) = attempts.get_mut(&login_id) else {
            return Err(LoginError::NotFound);
        };
        match result {
            Ok(provider_id) => {
                attempt.status = AttemptStatus::Succeeded { provider_id };
                Ok(())
            }
            Err(error) => {
                attempt.status = AttemptStatus::Failed {
                    message: error.user_message(),
                };
                Err(error)
            }
        }
    }

    async fn exchange_and_create(
        &self,
        name: &str,
        code: &str,
        verifier: &str,
    ) -> Result<Uuid, LoginError> {
        let token_url = self
            .issuer
            .join("oauth/token")
            .map_err(LoginError::InvalidIssuer)?;
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "authorization_code")
            .append_pair("code", code)
            .append_pair("redirect_uri", self.redirect_uri.as_str())
            .append_pair("client_id", &self.client_id)
            .append_pair("code_verifier", verifier)
            .finish();
        let response = self
            .http
            .post(token_url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header("originator", OAUTH_ORIGINATOR)
            .body(body)
            .send()
            .await
            .map_err(LoginError::TokenRequest)?;
        if !response.status().is_success() {
            return Err(LoginError::TokenRejected(response.status()));
        }
        let tokens: TokenResponse = response.json().await.map_err(LoginError::TokenRequest)?;
        let id_claims: IdTokenClaims = decode_jwt(&tokens.id_token)?;
        let access_claims: AccessTokenClaims = decode_jwt(&tokens.access_token)?;
        let account_id = id_claims
            .auth
            .and_then(|auth| auth.chatgpt_account_id)
            .filter(|value| !value.trim().is_empty())
            .ok_or(LoginError::MissingAccountId)?;
        let access_token_expires_at = access_claims
            .exp
            .and_then(|expiration| DateTime::from_timestamp(expiration, 0))
            .ok_or(LoginError::MissingExpiration)?;
        let response = self
            .gateway
            .create_chatgpt_provider(
                name,
                &ChatGptGatewayCredentials {
                    id_token: tokens.id_token,
                    access_token: tokens.access_token,
                    refresh_token: tokens.refresh_token,
                    account_id,
                    access_token_expires_at,
                },
            )
            .await
            .map_err(LoginError::Gateway)?;
        Ok(response.account.id)
    }

    async fn fail_by_state(&self, state: &str, message: String) {
        if let Some((_, attempt)) = self
            .attempts
            .lock()
            .await
            .iter_mut()
            .find(|(_, attempt)| attempt.state == state)
        {
            attempt.status = AttemptStatus::Failed { message };
        }
    }

    fn authorization_url(&self, state: &str, challenge: &str) -> Result<Url, LoginError> {
        let mut url = self
            .issuer
            .join("oauth/authorize")
            .map_err(LoginError::InvalidIssuer)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", self.redirect_uri.as_str())
            .append_pair("scope", CHATGPT_SCOPES)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("state", state)
            .append_pair("originator", OAUTH_ORIGINATOR);
        Ok(url)
    }
}

pub fn callback_router(service: ChatGptLoginService) -> Router {
    Router::new()
        .route("/auth/callback", get(callback))
        .with_state(service)
}

async fn callback(
    State(service): State<ChatGptLoginService>,
    Query(query): Query<CallbackQuery>,
) -> Html<String> {
    let result = service.complete(query).await;
    let (kind, title, message) = match result {
        Ok(()) => (
            "success",
            "ChatGPT connected",
            "Your provider was added. You can close this window.",
        ),
        Err(error) => {
            tracing::warn!(%error, "ChatGPT OAuth callback failed");
            (
                "error",
                "Could not connect ChatGPT",
                "Return to Agent Pane and try signing in again.",
            )
        }
    };
    Html(callback_html(
        service.dashboard_origin.as_str().trim_end_matches('/'),
        kind,
        title,
        message,
    ))
}

fn callback_html(origin: &str, kind: &str, title: &str, message: &str) -> String {
    let origin = serde_json::to_string(origin).expect("origin serializes");
    let kind_json = serde_json::to_string(kind).expect("kind serializes");
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>{title}</title><style>html{{color-scheme:dark}}body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#111;color:#ededed;font:15px system-ui}}main{{max-width:420px;padding:32px;text-align:center}}p{{color:#999;line-height:1.5}}</style></head><body><main><h1>{title}</h1><p>{message}</p></main><script>if(window.opener){{window.opener.postMessage({{type:'agent-pane:chatgpt-login',status:{kind_json}}},{origin});setTimeout(()=>window.close(),350)}}</script></body></html>"#
    )
}

fn random_url_safe(bytes: usize) -> String {
    let mut random = vec![0_u8; bytes];
    rand::thread_rng().fill_bytes(&mut random);
    URL_SAFE_NO_PAD.encode(random)
}

fn decode_jwt<T: DeserializeOwned>(token: &str) -> Result<T, LoginError> {
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(LoginError::InvalidToken);
    };
    if header.is_empty() || payload.is_empty() || signature.is_empty() {
        return Err(LoginError::InvalidToken);
    }
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| LoginError::InvalidToken)?;
    serde_json::from_slice(&payload).map_err(|_| LoginError::InvalidToken)
}

struct LoginAttempt {
    name: String,
    state: String,
    verifier: String,
    created_at: DateTime<Utc>,
    status: AttemptStatus,
}

enum AttemptStatus {
    Pending,
    Exchanging,
    Succeeded { provider_id: Uuid },
    Failed { message: String },
}

impl AttemptStatus {
    fn as_response(&self) -> LoginStatus {
        match self {
            Self::Pending => LoginStatus::Pending,
            Self::Exchanging => LoginStatus::Exchanging,
            Self::Succeeded { provider_id } => LoginStatus::Succeeded {
                provider_id: *provider_id,
            },
            Self::Failed { message } => LoginStatus::Failed {
                error: message.clone(),
            },
        }
    }
}

#[derive(Serialize)]
pub struct LoginStart {
    pub login_id: Uuid,
    pub authorization_url: String,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoginStatus {
    Pending,
    Exchanging,
    Succeeded { provider_id: Uuid },
    Failed { error: String },
}

#[derive(Deserialize)]
pub struct StartLoginRequest {
    pub name: String,
}

#[derive(Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    state: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

#[derive(Deserialize)]
struct IdTokenClaims {
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct AuthClaims {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
}

#[derive(Deserialize)]
struct AccessTokenClaims {
    exp: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("account name must not be blank or contain surrounding whitespace")]
    InvalidName,
    #[error("login attempt was not found")]
    NotFound,
    #[error("login attempt expired")]
    Expired,
    #[error("OAuth callback state was invalid")]
    InvalidState,
    #[error("OAuth callback did not contain an authorization code")]
    MissingAuthorizationCode,
    #[error("login attempt has already completed")]
    AlreadyCompleted,
    #[error("ChatGPT authorization was rejected: {0}")]
    AuthorizationRejected(String),
    #[error("could not construct the ChatGPT OAuth URL")]
    InvalidIssuer(#[source] url::ParseError),
    #[error("ChatGPT token request failed")]
    TokenRequest(#[source] reqwest::Error),
    #[error("ChatGPT token endpoint rejected the request with status {0}")]
    TokenRejected(reqwest::StatusCode),
    #[error("ChatGPT returned an invalid token")]
    InvalidToken,
    #[error("ChatGPT token did not contain an account ID")]
    MissingAccountId,
    #[error("ChatGPT access token did not contain an expiry")]
    MissingExpiration,
    #[error("could not save the ChatGPT provider")]
    Gateway(#[source] LlmGatewayError),
}

impl LoginError {
    pub fn user_message(&self) -> String {
        match self {
            Self::InvalidName => self.to_string(),
            Self::AuthorizationRejected(message) => message.clone(),
            Self::Gateway(_) => "Could not save the ChatGPT provider.".to_owned(),
            _ => "Could not complete ChatGPT sign in. Please try again.".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use super::{AccessTokenClaims, CHATGPT_SCOPES, ChatGptLoginService, decode_jwt};
    use crate::upstream::llm_gateway::LlmGatewayClient;

    #[test]
    fn decodes_jwt_expiration() {
        let claims: AccessTokenClaims =
            decode_jwt("eyJhbGciOiJub25lIn0.eyJleHAiOjE3ODc0Mjg4MDB9.signature").unwrap();
        assert_eq!(claims.exp, Some(1_787_428_800));
    }

    #[test]
    fn rejects_tokens_without_three_non_empty_segments() {
        assert!(decode_jwt::<AccessTokenClaims>("not-a-jwt").is_err());
    }

    #[test]
    fn authorization_url_matches_the_codex_pkce_contract() {
        let gateway = LlmGatewayClient::new(
            "http://127.0.0.1:3000".parse().unwrap(),
            "admin-token",
            StdDuration::from_secs(1),
        )
        .unwrap();
        let service = ChatGptLoginService::new(
            gateway,
            "client-id".to_owned(),
            "https://auth.openai.com".parse().unwrap(),
            "http://localhost:1455/auth/callback".parse().unwrap(),
            "http://localhost:5173".parse().unwrap(),
        );
        let url = service
            .authorization_url("csrf-state", "pkce-challenge")
            .unwrap();
        let parameters: std::collections::HashMap<_, _> = url.query_pairs().collect();

        assert_eq!(url.path(), "/oauth/authorize");
        assert_eq!(parameters.get("response_type").unwrap(), "code");
        assert_eq!(parameters.get("scope").unwrap(), CHATGPT_SCOPES);
        assert_eq!(parameters.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(parameters.get("code_challenge").unwrap(), "pkce-challenge");
        assert_eq!(parameters.get("state").unwrap(), "csrf-state");
        assert_eq!(parameters.get("originator").unwrap(), "codex_cli_rs");
    }
}

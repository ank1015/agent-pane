//! Local ChatGPT browser sign-in, adapted from the previous platform's PKCE flow.
//! Only the gateway receives credentials; the dashboard polls a non-secret status.
use std::{collections::HashMap, sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::{
    LlmGatewayClient, ProviderKind,
    error::ProviderError,
    model::{ChatGptCredentials, GatewayCreateAccount, validate_name},
};

pub const CALLBACK_ADDRESS: &str = "127.0.0.1:1455";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const MAX_ATTEMPTS: usize = 64;
const TTL: chrono::Duration = chrono::Duration::minutes(15);

#[derive(Clone)]
pub struct ChatGptLoginService {
    gateway: LlmGatewayClient,
    http: reqwest::Client,
    issuer: Url,
    attempts: Arc<Mutex<HashMap<Uuid, Attempt>>>,
}

struct Attempt {
    name: String,
    state: Zeroizing<String>,
    verifier: Zeroizing<String>,
    expires_at: DateTime<Utc>,
    status: LoginStatus,
}

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum LoginStatus {
    Pending,
    Exchanging,
    Succeeded { provider_id: Uuid },
    Failed { error: &'static str },
}

#[derive(Serialize)]
pub(super) struct LoginStart {
    pub login_id: Uuid,
    pub authorization_url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StartLoginInput {
    pub name: String,
}

// Do not log callback queries, token responses, or OAuth request errors.
#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
pub(super) struct CallbackQuery {
    pub state: String,
    pub code: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct TokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

impl ChatGptLoginService {
    pub fn new(gateway: LlmGatewayClient) -> Result<Self, reqwest::Error> {
        Ok(Self {
            gateway,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()?,
            issuer: Url::parse("https://auth.openai.com/").expect("static issuer URL"),
            attempts: Arc::default(),
        })
    }

    pub(super) async fn start(&self, name: String) -> Result<LoginStart, ProviderError> {
        validate_name(&name)?;
        let mut attempts = self.attempts.lock().await;
        prune(&mut attempts);
        if attempts.len() >= MAX_ATTEMPTS {
            return Err(ProviderError::LoginLimit);
        }
        let login_id = Uuid::new_v4();
        let state = random_secret::<32>();
        let verifier = random_secret::<64>();
        let mut authorization_url = self.issuer.join("oauth/authorize").expect("static path");
        authorization_url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            (
                "scope",
                "openid profile email offline_access api.connectors.read api.connectors.invoke",
            ),
            (
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            ),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "codex_cli_rs"),
        ]);
        attempts.insert(
            login_id,
            Attempt {
                name,
                state,
                verifier,
                expires_at: Utc::now() + TTL,
                status: LoginStatus::Pending,
            },
        );
        Ok(LoginStart {
            login_id,
            authorization_url: authorization_url.into(),
        })
    }

    pub(super) async fn status(&self, id: Uuid) -> Result<LoginStatus, ProviderError> {
        let mut attempts = self.attempts.lock().await;
        prune(&mut attempts);
        attempts
            .get(&id)
            .map(|attempt| attempt.status.clone())
            .ok_or(ProviderError::LoginNotFound)
    }

    pub(super) async fn cancel(&self, id: Uuid) -> Result<(), ProviderError> {
        let mut attempts = self.attempts.lock().await;
        if attempts
            .get(&id)
            .is_some_and(|attempt| matches!(attempt.status, LoginStatus::Exchanging))
        {
            // Once credential exchange starts, an account may be committed upstream.
            // Never claim it was canceled while that write is in flight.
            return Err(ProviderError::LoginExchanging);
        }
        attempts.remove(&id);
        Ok(())
    }

    pub(super) async fn complete(&self, query: CallbackQuery) -> Result<(), ProviderError> {
        let (id, name, verifier) = {
            let mut attempts = self.attempts.lock().await;
            prune(&mut attempts);
            let (id, attempt) = attempts
                .iter_mut()
                .find(|(_, attempt)| {
                    attempt.state.as_str() == query.state
                        && matches!(attempt.status, LoginStatus::Pending)
                })
                .ok_or(ProviderError::LoginNotFound)?;
            if query.error.is_some()
                || query
                    .code
                    .as_ref()
                    .is_none_or(|code| code.is_empty() || code.len() > 8192)
            {
                attempt.status = LoginStatus::Failed {
                    error: "ChatGPT sign-in was not completed. Try again.",
                };
                attempt.verifier.zeroize();
                return Err(ProviderError::InvalidRequest(
                    "ChatGPT sign-in was not completed.",
                ));
            }
            attempt.status = LoginStatus::Exchanging;
            (
                *id,
                attempt.name.clone(),
                Zeroizing::new(std::mem::take(&mut *attempt.verifier)),
            )
        };
        // Run independently of the popup connection. Closing that connection must
        // not strand an attempt in Exchanging after a potentially successful write.
        let service = self.clone();
        let task = tokio::spawn(async move {
            let result = service
                .exchange(
                    &name,
                    query.code.as_deref().expect("checked code"),
                    &verifier,
                )
                .await;
            let mut attempts = service.attempts.lock().await;
            if let Some(attempt) = attempts.get_mut(&id) {
                attempt.status = match &result {
                    Ok(provider_id) => LoginStatus::Succeeded {
                        provider_id: *provider_id,
                    },
                    Err(_) => LoginStatus::Failed {
                        error: "ChatGPT sign-in could not be completed. Refresh providers before starting again.",
                    },
                };
                attempt.expires_at = Utc::now() + TTL;
            }
            result
        });
        task.await.map_err(|_| ProviderError::LoginUnavailable)??;
        Ok(())
    }

    async fn exchange(
        &self,
        name: &str,
        code: &str,
        verifier: &str,
    ) -> Result<Uuid, ProviderError> {
        let mut response = self
            .http
            .post(self.issuer.join("oauth/token").expect("static path"))
            .header("originator", "codex_cli_rs")
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("redirect_uri", REDIRECT_URI),
                ("code", code),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(ProviderError::Request)?;
        if !response.status().is_success() {
            return Err(ProviderError::GatewayStatus(response.status()));
        }
        let mut body = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(ProviderError::Request)? {
            if body.len() + chunk.len() > 128 * 1024 {
                return Err(ProviderError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        let mut token: TokenResponse =
            serde_json::from_slice(&body).map_err(|_| ProviderError::InvalidResponse)?;
        if token.refresh_token.trim().is_empty() {
            return Err(ProviderError::InvalidResponse);
        }
        // These tokens came directly from the trusted TLS token endpoint, not a
        // caller-supplied JWT. Decoding here only extracts gateway account metadata.
        let id_claims = jwt_payload(&token.id_token)?;
        let access_claims = jwt_payload(&token.access_token)?;
        let account_id = id_claims["https://api.openai.com/auth"]["chatgpt_account_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or(ProviderError::InvalidResponse)?
            .to_owned();
        let expires_at = access_claims["exp"]
            .as_i64()
            .and_then(|exp| DateTime::from_timestamp(exp, 0))
            .filter(|expires| *expires > Utc::now())
            .ok_or(ProviderError::InvalidResponse)?;
        let credentials = ChatGptCredentials {
            id_token: std::mem::take(&mut token.id_token),
            access_token: std::mem::take(&mut token.access_token),
            refresh_token: std::mem::take(&mut token.refresh_token),
            account_id,
            access_token_expires_at: expires_at,
        };
        let input = GatewayCreateAccount {
            provider: ProviderKind::Chatgpt,
            name,
            credentials: &credentials,
        };
        Ok(self.gateway.create_account(&input).await?.account.id)
    }
}

fn prune(attempts: &mut HashMap<Uuid, Attempt>) {
    attempts.retain(|_, attempt| {
        attempt.expires_at > Utc::now() || matches!(attempt.status, LoginStatus::Exchanging)
    });
}

fn random_secret<const N: usize>() -> Zeroizing<String> {
    let mut bytes = Zeroizing::new([0u8; N]);
    OsRng.fill_bytes(bytes.as_mut());
    Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes.as_ref()))
}

fn jwt_payload(token: &str) -> Result<serde_json::Value, ProviderError> {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(ProviderError::InvalidResponse);
    }
    let bytes = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| ProviderError::InvalidResponse)?,
    );
    serde_json::from_slice(&bytes).map_err(|_| ProviderError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> LlmGatewayClient {
        LlmGatewayClient::new(
            Url::parse("http://127.0.0.1:1").unwrap(),
            "test-token",
            Duration::from_secs(1),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn starts_single_use_pkce_login_and_can_cancel_it() {
        let service = ChatGptLoginService::new(client()).unwrap();
        let first = service.start("Personal".to_owned()).await.unwrap();
        let second = service.start("Work".to_owned()).await.unwrap();
        assert_ne!(first.login_id, second.login_id);
        let url = Url::parse(&first.authorization_url).unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["redirect_uri"], REDIRECT_URI);
        assert_eq!(query["code_challenge_method"], "S256");
        assert_ne!(
            query["state"],
            Url::parse(&second.authorization_url)
                .unwrap()
                .query_pairs()
                .find(|(key, _)| key == "state")
                .unwrap()
                .1
        );
        assert!(matches!(
            service.status(first.login_id).await.unwrap(),
            LoginStatus::Pending
        ));
        service.cancel(first.login_id).await.unwrap();
        assert!(matches!(
            service.status(first.login_id).await,
            Err(ProviderError::LoginNotFound)
        ));
    }

    #[test]
    fn jwt_payload_rejects_malformed_tokens() {
        for token in ["", "a", "a.b", "a..c", "a.b.c.d", "a.***.c"] {
            assert!(jwt_payload(token).is_err());
        }
    }
}

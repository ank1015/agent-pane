use super::{Result, invalid, upstream};
use reqwest::{Client, Method};
use serde_json::Value;
use std::time::Duration;
use url::Url;

#[derive(Clone)]
pub struct SitesClient {
    client: Client,
    base: Url,
    token: std::sync::Arc<zeroize::Zeroizing<String>>,
}
impl SitesClient {
    pub fn new(base: Url, token: &str) -> Result<Self> {
        super::validate_token(token)?;
        let loopback = base
            .host_str()
            .is_some_and(|h| matches!(h, "localhost" | "127.0.0.1" | "[::1]"));
        if !(base.scheme() == "https" || base.scheme() == "http" && loopback)
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err(invalid(
                "Sites URL must be an HTTPS origin (HTTP allowed on loopback).",
            ));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(40))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| upstream())?;
        Ok(Self {
            client,
            base,
            token: std::sync::Arc::new(zeroize::Zeroizing::new(token.into())),
        })
    }
    pub async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = self.base.join(path).map_err(|_| upstream())?;
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(self.token.as_str());
        if let Some(body) = body {
            request = request.json(body);
        }
        let mut response = request.send().await.map_err(|_| upstream())?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| upstream())? {
            if bytes.len() + chunk.len() > 512 * 1024 {
                return Err(upstream());
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            if let Some(error) = public_rejection(status, &bytes) {
                return Err(error);
            }
            return Err(super::Error::Service(status.as_u16()));
        }
        serde_json::from_slice(&bytes).map_err(|_| upstream())
    }
}

fn public_rejection(status: reqwest::StatusCode, bytes: &[u8]) -> Option<super::Error> {
    if !status.is_client_error() {
        return None;
    }
    let envelope =
        serde_json::from_slice::<platform_runtime_contracts::ErrorEnvelope>(bytes).ok()?;
    let error = envelope.error;
    if error.code.is_empty()
        || error.code.len() > 128
        || !error
            .code
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        || error.message.is_empty()
        || error.message.len() > 4096
    {
        return None;
    }
    Some(super::Error::Rejected {
        status: status.as_u16(),
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_bounded_public_rejections_but_not_server_failure_bodies() {
        let body = br#"{"error":{"code":"INVALID_REQUEST","message":"Only index.html and backend.js may be updated."}}"#;
        let error = public_rejection(reqwest::StatusCode::BAD_REQUEST, body).unwrap();
        assert!(
            matches!(error, super::super::Error::Rejected { status: 400, error } if error.message.contains("index.html"))
        );
        assert!(public_rejection(reqwest::StatusCode::INTERNAL_SERVER_ERROR, body).is_none());
        assert!(
            public_rejection(
                reqwest::StatusCode::BAD_REQUEST,
                b"<html>upstream error</html>"
            )
            .is_none()
        );
        let oversized = serde_json::to_vec(
            &serde_json::json!({"error":{"code":"INVALID_REQUEST","message":"x".repeat(4097)}}),
        )
        .unwrap();
        assert!(public_rejection(reqwest::StatusCode::BAD_REQUEST, &oversized).is_none());
    }
}

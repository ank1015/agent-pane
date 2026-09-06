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
            return Err(super::Error::Service(status.as_u16()));
        }
        serde_json::from_slice(&bytes).map_err(|_| upstream())
    }
}

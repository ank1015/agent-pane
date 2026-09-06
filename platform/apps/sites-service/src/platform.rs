//! Parent-only Platform transport. Guest code cannot select URLs, scope or tokens.
use crate::{Error, Result};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Clone)]
pub struct PlatformClient {
    client: reqwest::Client,
    url: url::Url,
    token: Arc<zeroize::Zeroizing<String>>,
}
#[derive(Clone)]
pub struct Scope {
    pub site: Uuid,
    pub project: Uuid,
    pub invocation: Uuid,
    pub release: Uuid,
}
impl PlatformClient {
    pub fn new(url: &str, token: &str) -> Result<Self> {
        crate::config::validate_token(token)?;
        let mut url =
            url::Url::parse(url).map_err(|_| Error::Config("Invalid SITES_PLATFORM_URL"))?;
        let local = url
            .host_str()
            .is_some_and(|h| matches!(h, "localhost" | "127.0.0.1" | "[::1]"));
        if !(url.scheme() == "https" || url.scheme() == "http" && local)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Config(
                "SITES_PLATFORM_URL must be an HTTPS origin (HTTP allowed on loopback)",
            ));
        }
        url.set_path("/internal/site-capabilities");
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(8))
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| Error::Config("Cannot create Platform client"))?;
        Ok(Self {
            client,
            url,
            token: Arc::new(zeroize::Zeroizing::new(token.into())),
        })
    }
    pub fn from_env() -> Result<Option<Self>> {
        let url = std::env::var("SITES_PLATFORM_URL").unwrap_or_default();
        let token = zeroize::Zeroizing::new(
            std::env::var("SITES_PLATFORM_CAPABILITY_TOKEN").unwrap_or_default(),
        );
        if url.is_empty() && token.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self::new(&url, &token)?))
    }
    pub async fn call(&self, scope: &Scope, method: &str, args: Value) -> Value {
        let result=async {
            let mut response=self.client.post(self.url.clone()).bearer_auth(self.token.as_str()).json(&json!({"site_id":scope.site,"project_id":scope.project,"invocation_id":scope.invocation,"release_id":scope.release,"method":method,"args":args})).send().await.map_err(|_|())?;
            let success=response.status().is_success();let mut bytes=vec![];
            while let Some(chunk)=response.chunk().await.map_err(|_|())?{
                if bytes.len()+chunk.len()>256*1024{return Err(());}
                bytes.extend_from_slice(&chunk);
            }
            let value:Value=serde_json::from_slice(&bytes).map_err(|_|())?;
            if success {Ok(json!({"value":value}))}
            else if value["error"]["code"].is_string()&&value["error"]["message"].is_string() {Ok(json!({"error":value["error"]}))}
            else {Err(())}
        }.await;
        result.unwrap_or_else(|_|json!({"error":{"code":"PLATFORM_UNAVAILABLE","message":"Platform request outcome is uncertain. Retry mutations with the same idempotency key."}}))
    }
}
pub fn allowed(method: &str) -> bool {
    matches!(
        method,
        "callbacks.list"
            | "callbacks.get"
            | "environments.list"
            | "environments.get"
            | "harnesses.list"
            | "harnesses.get"
            | "sessions.startOptions"
            | "sessions.start"
            | "sessions.list"
            | "sessions.get"
            | "sessions.messages"
            | "sessions.metrics"
            | "sessions.send"
            | "sessions.stop"
    )
}

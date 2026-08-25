use serde::de::DeserializeOwned;

use execution_contracts::{EnvironmentId, ExecutionError, MachineId};
use execution_protocol::{
    CreateOperationRequest, Environment, MachineSummary, Operation, OperationEvent, OperationRecord,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use url::Url;

use crate::{
    ExecutionGatewayClientError, ExecutionGatewayConfig, GatewayEnvironment, GatewayMachineRuntime,
};

#[derive(Clone)]
pub struct ExecutionGatewayClient {
    http: reqwest::Client,
    base_url: Url,
    pub(crate) poll_interval: std::time::Duration,
    pub(crate) event_page_size: u32,
}

impl ExecutionGatewayClient {
    pub fn new(config: ExecutionGatewayConfig) -> Result<Self, ExecutionGatewayClientError> {
        validate_config(&config)?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", config.api_token))
            .map_err(ExecutionGatewayClientError::InvalidApiToken)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(config.request_timeout)
            .build()
            .map_err(ExecutionGatewayClientError::BuildClient)?;

        Ok(Self {
            http,
            base_url: config.base_url,
            poll_interval: config.poll_interval,
            event_page_size: config.event_page_size,
        })
    }

    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub async fn list_machines(&self) -> Result<Vec<MachineSummary>, ExecutionGatewayClientError> {
        self.send_json(self.http.get(self.url(&["v1", "machines"])?))
            .await
    }

    pub async fn machine(
        &self,
        machine_id: &MachineId,
    ) -> Result<MachineSummary, ExecutionGatewayClientError> {
        self.send_json(
            self.http
                .get(self.url(&["v1", "machines", machine_id.as_str()])?),
        )
        .await
    }

    pub async fn create_operation(
        &self,
        machine_id: &MachineId,
        operation: Operation,
    ) -> Result<OperationRecord, ExecutionGatewayClientError> {
        self.send_json(
            self.http
                .post(self.url(&["v1", "machines", machine_id.as_str(), "operations"])?)
                .json(&CreateOperationRequest {
                    operation: Box::new(operation),
                }),
        )
        .await
    }

    pub async fn environment_record(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<Environment, ExecutionGatewayClientError> {
        self.send_json(
            self.http
                .get(self.url(&["v1", "environments", environment_id.as_str()])?),
        )
        .await
    }

    pub async fn create_environment_operation(
        &self,
        environment_id: &EnvironmentId,
        operation: Operation,
    ) -> Result<OperationRecord, ExecutionGatewayClientError> {
        self.send_json(
            self.http
                .post(self.url(&["v1", "environments", environment_id.as_str(), "operations"])?)
                .json(&CreateOperationRequest {
                    operation: Box::new(operation),
                }),
        )
        .await
    }

    pub async fn operation(
        &self,
        operation_id: &str,
    ) -> Result<OperationRecord, ExecutionGatewayClientError> {
        self.send_json(
            self.http
                .get(self.url(&["v1", "operations", operation_id])?),
        )
        .await
    }

    pub async fn operation_events(
        &self,
        operation_id: &str,
        after: u64,
        limit: u32,
    ) -> Result<Vec<OperationEvent>, ExecutionGatewayClientError> {
        let mut url = self.url(&["v1", "operations", operation_id, "events"])?;
        url.query_pairs_mut()
            .append_pair("after", &after.to_string())
            .append_pair("limit", &limit.clamp(1, 1_000).to_string());
        self.send_json(self.http.get(url)).await
    }

    pub async fn cancel_operation(
        &self,
        operation_id: &str,
    ) -> Result<OperationRecord, ExecutionGatewayClientError> {
        self.send_json(
            self.http
                .post(self.url(&["v1", "operations", operation_id, "cancel"])?),
        )
        .await
    }

    pub async fn machine_runtime(
        &self,
        machine_id: &MachineId,
    ) -> Result<GatewayMachineRuntime, ExecutionGatewayClientError> {
        GatewayMachineRuntime::connect(self.clone(), machine_id).await
    }

    pub async fn environment(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<GatewayEnvironment, ExecutionGatewayClientError> {
        GatewayEnvironment::connect(self.clone(), environment_id).await
    }

    fn url(&self, segments: &[&str]) -> Result<Url, ExecutionGatewayClientError> {
        let mut url = self.base_url.clone();
        let mut path = url
            .path_segments_mut()
            .map_err(|()| ExecutionGatewayClientError::InvalidUrl)?;
        path.pop_if_empty();
        path.extend(segments.iter().copied());
        drop(path);
        Ok(url)
    }

    async fn send_json<T>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ExecutionGatewayClientError>
    where
        T: DeserializeOwned,
    {
        let response = request
            .send()
            .await
            .map_err(ExecutionGatewayClientError::Request)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(ExecutionGatewayClientError::Request)?;
        if !status.is_success() {
            return Err(ExecutionGatewayClientError::Rejected {
                status,
                error: serde_json::from_slice::<ExecutionError>(&body).ok(),
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        serde_json::from_slice(&body).map_err(ExecutionGatewayClientError::InvalidResponse)
    }
}

fn validate_config(config: &ExecutionGatewayConfig) -> Result<(), ExecutionGatewayClientError> {
    if config.api_token.is_empty() {
        return Err(ExecutionGatewayClientError::InvalidConfiguration(
            "API token must not be empty".to_owned(),
        ));
    }
    if config.request_timeout.is_zero() {
        return Err(ExecutionGatewayClientError::InvalidConfiguration(
            "request timeout must be greater than zero".to_owned(),
        ));
    }
    if config.poll_interval.is_zero() {
        return Err(ExecutionGatewayClientError::InvalidConfiguration(
            "poll interval must be greater than zero".to_owned(),
        ));
    }
    if !(1..=1_000).contains(&config.event_page_size) {
        return Err(ExecutionGatewayClientError::InvalidConfiguration(
            "event page size must be between 1 and 1000".to_owned(),
        ));
    }
    if config.base_url.cannot_be_a_base() {
        return Err(ExecutionGatewayClientError::InvalidConfiguration(
            "base URL cannot be used as a hierarchical base".to_owned(),
        ));
    }
    Ok(())
}

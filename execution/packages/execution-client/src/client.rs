use std::{future::Future, sync::Arc};

use execution_core::{
    ExecutionError, ExecutionErrorCode, ExecutionHostId, ExecutionResult, OperationContext,
    Validate,
};
use execution_wire::{
    Operation, OperationResult, PROTOCOL_VERSION, RequestEnvelope, RequestId, ResponseEnvelope,
};
use reqwest::{
    Client,
    header::{ACCEPT, AUTHORIZATION, HeaderValue},
};
use url::Url;
use uuid::Uuid;

use crate::{
    ConfigError, ExecutionClientConfig, GatewayHostRuntime,
    error::{gateway_error, protocol_error, transport_error},
};

/// Cheaply cloneable, authenticated connection to one gateway deployment.
#[derive(Clone, Debug)]
pub struct ExecutionClient {
    inner: Arc<ClientInner>,
}

#[derive(Debug)]
struct ClientInner {
    http: Client,
    base_url: Url,
    authorization: HeaderValue,
    max_response_bytes: usize,
}

impl ExecutionClient {
    pub fn new(config: ExecutionClientConfig) -> Result<Self, ConfigError> {
        let authorization = config.authorization()?;
        let http = Client::builder()
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(ConfigError::BuildClient)?;
        Ok(Self {
            inner: Arc::new(ClientInner {
                http,
                base_url: config.base_url,
                authorization,
                max_response_bytes: config.max_response_bytes,
            }),
        })
    }

    /// Describes a ready host through the gateway and validates its identity.
    ///
    /// Hosted IDs are UUIDs. No host is provisioned or resumed by this method.
    /// The runtime's descriptor remains a snapshot; connect again to refresh it.
    pub async fn connect_host(
        &self,
        context: &OperationContext,
        host_id: ExecutionHostId,
    ) -> ExecutionResult<GatewayHostRuntime> {
        context.checkpoint()?;
        let id = Uuid::parse_str(host_id.as_str()).map_err(|_| {
            ExecutionError::new(
                ExecutionErrorCode::InvalidRequest,
                "gateway host ID must be a UUID",
            )
        })?;
        let host_id = ExecutionHostId::new(id.to_string())?;
        let mut url = self.inner.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| protocol_error("gateway URL cannot contain path segments"))?
            .pop_if_empty()
            .extend(["v1", "hosts", host_id.as_str(), "operations"]);
        let result = self.call(context, &url, Operation::Describe).await?;
        let OperationResult::HostDescriptor(descriptor) = result else {
            return Err(protocol_error(
                "gateway describe returned an unexpected result",
            ));
        };
        descriptor
            .validate()
            .map_err(|_| protocol_error("gateway returned an invalid host descriptor"))?;
        if descriptor.host_id != host_id {
            return Err(protocol_error(
                "gateway descriptor does not match the requested host",
            ));
        }
        Ok(GatewayHostRuntime {
            client: self.clone(),
            url,
            descriptor,
        })
    }

    pub(crate) async fn call(
        &self,
        context: &OperationContext,
        url: &Url,
        operation: Operation,
    ) -> ExecutionResult<OperationResult> {
        context.checkpoint()?;
        operation.validate()?;
        let request = RequestEnvelope::new(RequestId::generate(), operation);
        within_context(context, self.send(url, request)).await
    }

    async fn send(&self, url: &Url, request: RequestEnvelope) -> ExecutionResult<OperationResult> {
        let mut response = self
            .inner
            .http
            .post(url.clone())
            .header(AUTHORIZATION, self.inner.authorization.clone())
            .header(ACCEPT, "application/json")
            .json(&request)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        let gateway_request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let limit = self.inner.max_response_bytes;
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(response_too_large());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(response_too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(gateway_error(status, &bytes, gateway_request_id.as_deref()));
        }
        let response: ResponseEnvelope = serde_json::from_slice(&bytes)
            .map_err(|_| protocol_error("gateway returned invalid execution JSON"))?;
        if response.version() != PROTOCOL_VERSION || response.request_id() != &request.request_id {
            return Err(protocol_error(
                "gateway returned an uncorrelated execution response",
            ));
        }
        match response {
            ResponseEnvelope::Success { result, .. } => Ok(result),
            ResponseEnvelope::Error { error, .. } => Err(error),
        }
    }
}

fn response_too_large() -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCode::ResourceExhausted,
        "gateway response exceeded the configured byte limit",
    )
    .with_detail("source", "client")
}

async fn within_context<T>(
    context: &OperationContext,
    operation: impl Future<Output = ExecutionResult<T>>,
) -> ExecutionResult<T> {
    let deadline = async {
        match context.remaining() {
            Some(remaining) => tokio::time::sleep(remaining).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        biased;
        _ = context.cancelled() => Err(ExecutionError::cancelled()),
        _ = deadline => Err(ExecutionError::deadline_exceeded()),
        result = operation => result,
    }
}

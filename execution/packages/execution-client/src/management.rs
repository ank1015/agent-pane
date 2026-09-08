//! Typed gateway resources. Mutations return asynchronous resource state, not a
//! readiness guarantee. Persist a stable key before creation and reuse it on retry.
use crate::{
    ExecutionClient,
    client::{response_too_large, within_context},
    error::{gateway_error, protocol_error, transport_error},
};
use execution_api::*;
use execution_core::{ExecutionError, ExecutionErrorCode, ExecutionResult, OperationContext};
use reqwest::{
    Method,
    header::{ACCEPT, AUTHORIZATION},
};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

#[derive(Clone, Debug, Default, Serialize)]
pub struct HostFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ExecutionHostKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<ExecutionHostState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub e2b_account_id: Option<Uuid>,
    pub include_deleted: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SnapshotFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<SnapshotState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub e2b_account_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_host_id: Option<Uuid>,
    pub include_deleted: bool,
}

impl ExecutionClient {
    /// Request deletion of an existing host; the returned state may still be deleting.
    pub async fn delete_host(
        &self,
        ctx: &OperationContext,
        id: Uuid,
    ) -> ExecutionResult<ExecutionHost> {
        self.management(ctx, Method::DELETE, &format!("hosts/{id}"), None, None, &())
            .await
    }
    pub async fn list_accounts(&self, ctx: &OperationContext) -> ExecutionResult<Vec<E2bAccount>> {
        self.management(ctx, Method::GET, "e2b-accounts", None, None, &())
            .await
    }
    pub async fn list_hosts(
        &self,
        ctx: &OperationContext,
        filter: &HostFilter,
    ) -> ExecutionResult<Vec<ExecutionHost>> {
        self.management(ctx, Method::GET, "hosts", None, None, filter)
            .await
    }
    pub async fn get_host(
        &self,
        ctx: &OperationContext,
        id: Uuid,
    ) -> ExecutionResult<ExecutionHost> {
        self.management(ctx, Method::GET, &format!("hosts/{id}"), None, None, &())
            .await
    }
    pub async fn create_host(
        &self,
        ctx: &OperationContext,
        key: &str,
        request: &CreateExecutionHostRequest,
    ) -> ExecutionResult<ExecutionHost> {
        self.management(
            ctx,
            Method::POST,
            "hosts",
            Some(key),
            Some(
                serde_json::to_value(request)
                    .map_err(|_| protocol_error("invalid host request"))?,
            ),
            &(),
        )
        .await
    }
    pub async fn resume_host(
        &self,
        ctx: &OperationContext,
        id: Uuid,
    ) -> ExecutionResult<ExecutionHost> {
        self.management(
            ctx,
            Method::POST,
            &format!("hosts/{id}/resume"),
            None,
            None,
            &(),
        )
        .await
    }
    pub async fn list_snapshots(
        &self,
        ctx: &OperationContext,
        filter: &SnapshotFilter,
    ) -> ExecutionResult<Vec<E2bSnapshot>> {
        self.management(ctx, Method::GET, "snapshots", None, None, filter)
            .await
    }
    pub async fn get_snapshot(
        &self,
        ctx: &OperationContext,
        id: Uuid,
    ) -> ExecutionResult<E2bSnapshot> {
        self.management(
            ctx,
            Method::GET,
            &format!("snapshots/{id}"),
            None,
            None,
            &(),
        )
        .await
    }
    pub async fn create_snapshot(
        &self,
        ctx: &OperationContext,
        host: Uuid,
        key: &str,
        request: &CreateSnapshotRequest,
    ) -> ExecutionResult<E2bSnapshot> {
        self.management(
            ctx,
            Method::POST,
            &format!("hosts/{host}/snapshots"),
            Some(key),
            Some(
                serde_json::to_value(request)
                    .map_err(|_| protocol_error("invalid snapshot request"))?,
            ),
            &(),
        )
        .await
    }
    async fn management<T: DeserializeOwned, Q: Serialize>(
        &self,
        ctx: &OperationContext,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<serde_json::Value>,
        query: &Q,
    ) -> ExecutionResult<T> {
        ctx.checkpoint()?;
        let mut url = self.inner.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| protocol_error("invalid gateway URL"))?
            .pop_if_empty()
            .push("v1")
            .extend(path.split('/'));
        let mut request = self
            .inner
            .http
            .request(method, url)
            .header(AUTHORIZATION, self.inner.authorization.clone())
            .header(ACCEPT, "application/json")
            .query(query);
        if let Some(key) = key {
            if key.is_empty() || key.len() > 255 || !key.bytes().all(|b| (33..=126).contains(&b)) {
                return Err(ExecutionError::new(
                    ExecutionErrorCode::InvalidRequest,
                    "idempotency key must contain 1–255 printable ASCII characters without spaces",
                ));
            }
            request = request.header("idempotency-key", key);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        within_context(ctx, async {
            let mut response = request.send().await.map_err(transport_error)?;
            let status = response.status();
            let request_id = response
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let limit = self.inner.max_response_bytes;
            if response.content_length().is_some_and(|n| n > limit as u64) {
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
                return Err(gateway_error(status, &bytes, request_id.as_deref()));
            }
            serde_json::from_slice(&bytes)
                .map_err(|_| protocol_error("gateway returned invalid resource JSON"))
        })
        .await
    }
}

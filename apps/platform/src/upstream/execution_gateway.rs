use std::time::Duration;

use axum::http::StatusCode;
use reqwest::header;
use serde::de::DeserializeOwned;
use url::Url;
use uuid::Uuid;

use crate::{
    error::execution_gateway_rejected_body,
    machines::model::{
        CreateMachineEnvironmentRequest, CreateSandboxAccountRequest,
        CreateSandboxEnvironmentTemplateRequest, CreateSandboxRequest,
        CreateSandboxSnapshotRequest, CreateSnapshotRequest, MachineInventory,
        RotateSandboxCredentialsRequest, SandboxAccount, SandboxCreated,
        SandboxEnvironmentInstance, SandboxEnvironmentTemplate, SandboxMachine, Snapshot,
        SnapshotQuery, UpdateNameRequest, UpdateSandboxEnvironmentTemplateRequest,
    },
};
use execution_protocol::{Environment, MachineSummary, ProjectEnvironment};

#[derive(Clone)]
pub struct ExecutionGatewayClient {
    http: reqwest::Client,
    base_url: Url,
    materialization_timeout: Duration,
}

impl ExecutionGatewayClient {
    pub fn new(
        base_url: Url,
        control_token: &str,
        timeout: Duration,
    ) -> Result<Self, ExecutionGatewayClientError> {
        Self::new_with_materialization_timeout(base_url, control_token, timeout, timeout)
    }

    pub fn new_with_materialization_timeout(
        base_url: Url,
        control_token: &str,
        timeout: Duration,
        materialization_timeout: Duration,
    ) -> Result<Self, ExecutionGatewayClientError> {
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {control_token}"))
                .map_err(ExecutionGatewayClientError::InvalidControlToken)?;
        authorization.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()
            .map_err(ExecutionGatewayClientError::Build)?;

        Ok(Self {
            http,
            base_url: normalized_base_url(base_url),
            materialization_timeout,
        })
    }

    pub(crate) async fn ready(&self) -> Result<(), ExecutionGatewayError> {
        self.send_empty(self.http.get(self.url("health")?)).await
    }

    pub(crate) async fn list_sandbox_accounts(
        &self,
    ) -> Result<Vec<SandboxAccount>, ExecutionGatewayError> {
        self.send_json(self.http.get(self.url("v1/control/sandbox-accounts")?))
            .await
    }

    pub(crate) async fn machine_inventory(
        &self,
    ) -> Result<MachineInventory, ExecutionGatewayError> {
        self.send_json(self.http.get(self.url("v1/control/machines")?))
            .await
    }

    pub(crate) async fn list_environments(
        &self,
        machine_id: &str,
    ) -> Result<Vec<Environment>, ExecutionGatewayError> {
        self.send_json(
            self.http
                .get(self.url("v1/control/environments")?)
                .query(&[("machine_id", machine_id)]),
        )
        .await
    }

    pub(crate) async fn create_environment(
        &self,
        machine_id: &str,
        request: &CreateMachineEnvironmentRequest,
    ) -> Result<Environment, ExecutionGatewayError> {
        self.send_json(self.http.post(self.url("v1/control/environments")?).json(
            &serde_json::json!({
                "project_id": request.project_id,
                "machine_id": machine_id,
                "name": request.name.as_str(),
                "workspace_root_id": request.workspace_root_id.as_str(),
                "path": request.path.as_str(),
            }),
        ))
        .await
    }

    pub(crate) async fn list_project_environments(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<ProjectEnvironment>, ExecutionGatewayError> {
        self.send_json(
            self.http
                .get(self.url(&format!("v1/control/projects/{project_id}/environments"))?),
        )
        .await
    }

    pub(crate) async fn get_sandbox_account(
        &self,
        account_id: Uuid,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.send_json(
            self.http
                .get(self.url(&format!("v1/control/sandbox-accounts/{account_id}"))?),
        )
        .await
    }

    pub(crate) async fn create_sandbox_account(
        &self,
        request: &CreateSandboxAccountRequest,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.send_json(
            self.http
                .post(self.url("v1/control/sandbox-accounts")?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn rotate_sandbox_credentials(
        &self,
        account_id: Uuid,
        request: &RotateSandboxCredentialsRequest,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.send_json(
            self.http
                .put(self.url(&format!(
                    "v1/control/sandbox-accounts/{account_id}/credentials"
                ))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn update_sandbox_account_name(
        &self,
        account_id: Uuid,
        request: &UpdateNameRequest,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.send_json(
            self.http
                .patch(self.url(&format!("v1/control/sandbox-accounts/{account_id}"))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn delete_sandbox_account(
        &self,
        account_id: Uuid,
    ) -> Result<(), ExecutionGatewayError> {
        self.send_empty(
            self.http
                .delete(self.url(&format!("v1/control/sandbox-accounts/{account_id}"))?),
        )
        .await
    }

    pub(crate) async fn list_sandbox_machines(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<SandboxMachine>, ExecutionGatewayError> {
        self.send_json(self.http.get(self.url(&format!(
            "v1/control/sandbox-accounts/{account_id}/sandboxes"
        ))?))
        .await
    }

    pub(crate) async fn create_sandbox(
        &self,
        account_id: Uuid,
        request: &CreateSandboxRequest,
    ) -> Result<SandboxCreated, ExecutionGatewayError> {
        self.send_json(
            self.http
                .post(self.url(&format!(
                    "v1/control/sandbox-accounts/{account_id}/sandboxes"
                ))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn create_sandbox_snapshot(
        &self,
        account_id: Uuid,
        sandbox_id: &str,
        request: &CreateSandboxSnapshotRequest,
    ) -> Result<Snapshot, ExecutionGatewayError> {
        self.send_json(
            self.http
                .post(self.url(&format!(
                    "v1/control/sandbox-accounts/{account_id}/sandboxes/{sandbox_id}/snapshots"
                ))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn list_snapshots(
        &self,
        query: &SnapshotQuery,
    ) -> Result<Vec<Snapshot>, ExecutionGatewayError> {
        self.send_json(
            self.http
                .get(self.url("v1/control/snapshots")?)
                .query(query),
        )
        .await
    }

    pub(crate) async fn get_snapshot(
        &self,
        snapshot_id: Uuid,
    ) -> Result<Snapshot, ExecutionGatewayError> {
        self.send_json(
            self.http
                .get(self.url(&format!("v1/control/snapshots/{snapshot_id}"))?),
        )
        .await
    }

    pub(crate) async fn update_snapshot_name(
        &self,
        snapshot_id: Uuid,
        request: &UpdateNameRequest,
    ) -> Result<Snapshot, ExecutionGatewayError> {
        self.send_json(
            self.http
                .patch(self.url(&format!("v1/control/snapshots/{snapshot_id}"))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn create_snapshot(
        &self,
        request: &CreateSnapshotRequest,
    ) -> Result<Snapshot, ExecutionGatewayError> {
        self.send_json(
            self.http
                .post(self.url("v1/control/snapshots")?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn delete_snapshot(
        &self,
        snapshot_id: Uuid,
    ) -> Result<(), ExecutionGatewayError> {
        self.send_empty(
            self.http
                .delete(self.url(&format!("v1/control/snapshots/{snapshot_id}"))?),
        )
        .await
    }

    pub(crate) async fn list_sandbox_environment_templates(
        &self,
    ) -> Result<Vec<SandboxEnvironmentTemplate>, ExecutionGatewayError> {
        self.send_json(
            self.http
                .get(self.url("v1/control/sandbox-environment-templates")?),
        )
        .await
    }

    pub(crate) async fn get_sandbox_environment_template(
        &self,
        template_id: Uuid,
    ) -> Result<SandboxEnvironmentTemplate, ExecutionGatewayError> {
        self.send_json(self.http.get(self.url(&format!(
            "v1/control/sandbox-environment-templates/{template_id}"
        ))?))
        .await
    }

    pub(crate) async fn create_sandbox_environment_template(
        &self,
        request: &CreateSandboxEnvironmentTemplateRequest,
    ) -> Result<SandboxEnvironmentTemplate, ExecutionGatewayError> {
        self.send_json(
            self.http
                .post(self.url("v1/control/sandbox-environment-templates")?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn update_sandbox_environment_template(
        &self,
        template_id: Uuid,
        request: &UpdateSandboxEnvironmentTemplateRequest,
    ) -> Result<SandboxEnvironmentTemplate, ExecutionGatewayError> {
        self.send_json(
            self.http
                .patch(self.url(&format!(
                    "v1/control/sandbox-environment-templates/{template_id}"
                ))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn delete_sandbox_environment_template(
        &self,
        template_id: Uuid,
    ) -> Result<(), ExecutionGatewayError> {
        self.send_empty(self.http.delete(self.url(&format!(
            "v1/control/sandbox-environment-templates/{template_id}"
        ))?))
        .await
    }

    pub(crate) async fn materialize_sandbox_environment_template(
        &self,
        template_id: Uuid,
    ) -> Result<SandboxEnvironmentInstance, ExecutionGatewayError> {
        self.send_json(
            self.http
                .post(self.url(&format!(
                    "v1/control/sandbox-environment-templates/{template_id}/environments"
                ))?)
                .timeout(self.materialization_timeout),
        )
        .await
    }

    pub(crate) async fn list_sandbox_environment_instances(
        &self,
        template_id: Uuid,
    ) -> Result<Vec<SandboxEnvironmentInstance>, ExecutionGatewayError> {
        self.send_json(self.http.get(self.url(&format!(
            "v1/control/sandbox-environment-templates/{template_id}/environments"
        ))?))
        .await
    }

    pub(crate) async fn delete_environment(
        &self,
        environment_id: &str,
    ) -> Result<(), ExecutionGatewayError> {
        self.send_empty(
            self.http
                .delete(self.url(&format!("v1/control/environments/{environment_id}"))?),
        )
        .await
    }

    pub(crate) async fn update_environment_name(
        &self,
        environment_id: &str,
        request: &UpdateNameRequest,
    ) -> Result<Environment, ExecutionGatewayError> {
        self.send_json(
            self.http
                .patch(self.url(&format!("v1/control/environments/{environment_id}"))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn update_machine_name(
        &self,
        machine_id: &str,
        request: &UpdateNameRequest,
    ) -> Result<MachineSummary, ExecutionGatewayError> {
        self.send_json(
            self.http
                .patch(self.url(&format!("v1/control/machines/{machine_id}"))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn delete_machine(
        &self,
        machine_id: &str,
    ) -> Result<(), ExecutionGatewayError> {
        self.send_empty(
            self.http
                .delete(self.url(&format!("v1/control/machines/{machine_id}"))?),
        )
        .await
    }

    fn url(&self, path: &str) -> Result<Url, ExecutionGatewayError> {
        self.base_url
            .join(path)
            .map_err(ExecutionGatewayError::InvalidUrl)
    }

    async fn send_json<T>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ExecutionGatewayError>
    where
        T: DeserializeOwned,
    {
        let response = request
            .send()
            .await
            .map_err(ExecutionGatewayError::Request)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(ExecutionGatewayError::Request)?;
        if !status.is_success() {
            return Err(ExecutionGatewayError::Rejected {
                status,
                body: execution_gateway_rejected_body(&body),
            });
        }
        serde_json::from_slice(&body).map_err(ExecutionGatewayError::InvalidResponse)
    }

    async fn send_empty(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(), ExecutionGatewayError> {
        let response = request
            .send()
            .await
            .map_err(ExecutionGatewayError::Request)?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response
            .bytes()
            .await
            .map_err(ExecutionGatewayError::Request)?;
        Err(ExecutionGatewayError::Rejected {
            status,
            body: execution_gateway_rejected_body(&body),
        })
    }
}

fn normalized_base_url(mut base_url: Url) -> Url {
    if !base_url.path().ends_with('/') {
        let path = format!("{}/", base_url.path());
        base_url.set_path(&path);
    }
    base_url
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionGatewayClientError {
    #[error("PLATFORM_EXECUTION_GATEWAY_CONTROL_TOKEN is not a valid HTTP header value")]
    InvalidControlToken(#[source] reqwest::header::InvalidHeaderValue),
    #[error("could not build the execution gateway HTTP client")]
    Build(#[source] reqwest::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionGatewayError {
    #[error("could not construct an execution gateway URL")]
    InvalidUrl(#[source] url::ParseError),
    #[error("execution gateway request failed")]
    Request(#[source] reqwest::Error),
    #[error("execution gateway returned an invalid response")]
    InvalidResponse(#[source] serde_json::Error),
    #[error("execution gateway rejected the request with status {status}")]
    Rejected {
        status: StatusCode,
        body: serde_json::Value,
    },
}

#[cfg(test)]
#[path = "execution_gateway_tests.rs"]
mod tests;

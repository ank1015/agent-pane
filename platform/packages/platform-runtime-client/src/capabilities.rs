//! The same capability vocabulary exposed to site backends, under a run lease.
use crate::types::{
    CursorPage, Environment, Harness, MessagePage, Run, RunOutputsPage, Session, capabilities as c,
};
use crate::{Command, Result, RunClient};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::Uuid;

pub struct PlatformCapabilities<'a> {
    run: &'a RunClient,
}
impl RunClient {
    pub fn platform(&self) -> PlatformCapabilities<'_> {
        PlatformCapabilities { run: self }
    }
}
impl PlatformCapabilities<'_> {
    pub async fn list_execution_resources(&self) -> Result<c::ExecutionResources> {
        self.read("execution.listResources", json!({})).await
    }
    /// Dynamic adapter for registered code-mode tools. Scope and credentials are
    /// still supplied only by this RunClient; server validation remains authoritative.
    pub async fn call(&self, method: &str, args: Value) -> Result<Value> {
        self.read(method, args).await
    }
    pub async fn create_sandbox(
        &self,
        command: &Command<c::CreateSandboxFromSnapshot>,
    ) -> Result<c::Sandbox> {
        self.mutate("sandboxes.createFromSnapshot", command).await
    }
    pub async fn get_sandbox(&self, id: Uuid) -> Result<c::Sandbox> {
        self.read("sandboxes.get", json!({"sandboxId":id})).await
    }
    pub async fn terminate_sandbox(&self, command: &Command<c::SandboxId>) -> Result<c::Sandbox> {
        self.mutate("sandboxes.terminate", command).await
    }
    pub async fn bash(&self, command: &Command<c::Bash>) -> Result<c::Execution> {
        self.mutate("execution.bash", command).await
    }
    pub async fn get_execution(&self, id: Uuid) -> Result<c::Execution> {
        self.read("execution.get", json!({"executionId":id})).await
    }
    pub async fn execution_output(
        &self,
        id: Uuid,
        options: &c::ExecutionOutputOptions,
    ) -> Result<c::ExecutionOutput> {
        self.read(
            "execution.output",
            json!({"executionId":id,"options":options}),
        )
        .await
    }
    pub async fn cancel_execution(
        &self,
        command: &Command<c::ExecutionId>,
    ) -> Result<c::Execution> {
        self.mutate("execution.cancel", command).await
    }
    async fn read<T: DeserializeOwned>(&self, method: &str, args: Value) -> Result<T> {
        self.run
            .client
            .inner
            .transport
            .send(
                self.run
                    .request(reqwest::Method::POST, "/capabilities")?
                    .json(&c::CapabilityRequest {
                        method: method.into(),
                        args,
                    }),
                true,
            )
            .await
    }
    async fn mutate<T: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        command: &Command<T>,
    ) -> Result<R> {
        self.read(
            method,
            json!({"input":command.body(),"options":{"idempotencyKey":command.key().as_str()}}),
        )
        .await
    }
    pub async fn list_environments(
        &self,
        options: &c::PageOptions,
    ) -> Result<CursorPage<Environment>> {
        self.read("environments.list", json!({"options":options}))
            .await
    }
    pub async fn get_environment(&self, id: Uuid) -> Result<Environment> {
        self.read("environments.get", json!({"environmentId":id}))
            .await
    }
    pub async fn list_accounts(&self, options: &c::PageOptions) -> Result<CursorPage<c::Account>> {
        self.read("accounts.list", json!({"options":options})).await
    }
    pub async fn list_harnesses(&self, options: &c::PageOptions) -> Result<CursorPage<Harness>> {
        self.read("harnesses.list", json!({"options":options}))
            .await
    }
    pub async fn get_harness(&self, id: &str) -> Result<Harness> {
        self.read("harnesses.get", json!({"harnessId":id})).await
    }
    pub async fn start_options(&self, id: &str) -> Result<c::StartOptions> {
        self.read("harnesses.startOptions", json!({"harnessId":id}))
            .await
    }
    pub async fn create_session(
        &self,
        command: &Command<c::CreateSession>,
    ) -> Result<c::SessionCreated> {
        self.mutate("sessions.create", command).await
    }
    pub async fn list_sessions(&self, options: &c::PageOptions) -> Result<CursorPage<Session>> {
        self.read("sessions.list", json!({"options":options})).await
    }
    pub async fn get_session(&self, id: Uuid) -> Result<Session> {
        self.read("sessions.get", json!({"sessionId":id})).await
    }
    pub async fn messages(&self, id: Uuid, options: &c::MessageOptions) -> Result<MessagePage> {
        self.read(
            "sessions.messages",
            json!({"sessionId":id,"options":options}),
        )
        .await
    }
    pub async fn session_stats(&self, id: Uuid) -> Result<c::Statistics> {
        self.read("sessions.stats", json!({"sessionId":id})).await
    }
    pub async fn create_run(&self, command: &Command<c::CreateRun>) -> Result<c::RunCreated> {
        self.mutate("runs.create", command).await
    }
    pub async fn list_runs(
        &self,
        session: Uuid,
        options: &c::PageOptions,
    ) -> Result<CursorPage<Run>> {
        self.read("runs.list", json!({"sessionId":session,"options":options}))
            .await
    }
    pub async fn get_run(&self, id: Uuid) -> Result<Run> {
        self.read("runs.get", json!({"runId":id})).await
    }
    pub async fn steer_run(&self, command: &Command<c::SteerRun>) -> Result<c::InputAccepted> {
        self.mutate("runs.steer", command).await
    }
    pub async fn abort_run(&self, command: &Command<c::AbortRun>) -> Result<c::AbortAccepted> {
        self.mutate("runs.abort", command).await
    }
    pub async fn run_stats(&self, id: Uuid) -> Result<c::Statistics> {
        self.read("runs.stats", json!({"runId":id})).await
    }
    pub async fn run_outputs(
        &self,
        id: Uuid,
        options: &c::OutputOptions,
    ) -> Result<RunOutputsPage> {
        self.read("runs.outputs", json!({"runId":id,"options":options}))
            .await
    }
}

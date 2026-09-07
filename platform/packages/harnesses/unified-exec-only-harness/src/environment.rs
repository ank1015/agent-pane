//! A session owns one resolved target. The immutable config + session ID are
//! the durable creation plan, including a deterministic gateway idempotency key.
//! Forks do not copy session state, so a sandbox fork starts from its snapshot.
use std::time::Duration;

use execution_api::{CreateExecutionHostRequest, E2bHostSource};
use execution_client::ExecutionClient;
use execution_core::OperationContext;
use platform_runtime_client::{Error, RunClient, types::*};
use serde_json::{Value, json};

use crate::{Environment, ExecutionTarget, driver::command};

pub(crate) enum ResolveError {
    Retry(Error),
    Permanent(String),
}
impl From<Error> for ResolveError {
    fn from(value: Error) -> Self {
        Self::Retry(value)
    }
}

pub(crate) async fn resolve(
    gateway: &ExecutionClient,
    client: &RunClient,
    context: &mut RunContext,
    environment: &Environment,
) -> Result<ExecutionTarget, ResolveError> {
    let page = client
        .session_state(&SessionStateQuery {
            namespace: crate::ID.into(),
            key: Some("execution-target".into()),
            ..Default::default()
        })
        .await?;
    let entry = page.items.first();
    if let Some(value) = entry.and_then(|entry| entry.value.as_ref()) {
        return serde_json::from_value(Value::Object(value.clone()))
            .map_err(|_| ResolveError::Retry(Error::Invalid("invalid saved execution target")));
    }
    let host = match environment {
        Environment::Machine { machine_id, .. } => *machine_id,
        Environment::Sandbox { snapshot_id, .. } => {
            // Do not put run IDs or timestamps in this body: every retry and
            // future run must replay exactly the same session-level request.
            let request = CreateExecutionHostRequest {
                name: Some(format!("session-{}", context.session.id)),
                source: E2bHostSource::Snapshot {
                    snapshot_id: *snapshot_id,
                },
                timeout_seconds: None,
                network_access: Some(true),
                metadata: json!({"platform_session_id":context.session.id,"platform_project_id":context.session.project_id}),
            };
            gateway
                .create_host(
                    &OperationContext::with_timeout(Duration::from_secs(30)),
                    &format!("unified-exec-only-session:{}", context.session.id),
                    &request,
                )
                .await
                .map_err(|error| {
                    if crate::tools::uncertain(&error) {
                        ResolveError::Retry(Error::Invalid(
                            "sandbox creation uncertain; replay the session creation key",
                        ))
                    } else {
                        ResolveError::Permanent(error.to_string())
                    }
                })?
                .id
        }
    };
    let target = environment.target(host).map_err(ResolveError::Permanent)?;
    let mut commit = Commit::new(context.run.run.version, context.session.current_revision);
    commit.session_state.push(SessionStateWrite {
        namespace: crate::ID.into(),
        key: "execution-target".into(),
        expected_version: entry.map_or(0, |entry| entry.version),
        mutation: SessionStateMutation::Set {
            value: json!(target).as_object().unwrap().clone(),
        },
    });
    // Record the host before waiting for readiness. A lost acknowledgement or
    // lease is recovered by reading state/replaying the same gateway key.
    client.commit(&command(commit)?).await?;
    *context = client.context(&ContextQuery::default()).await?;
    Ok(target)
}

//! Optional public workspace output; private durable planning survives activation loss.
use platform_runtime_client::{Command, Error, RequestKey, Result, RunClient, types::*};
use serde_json::{Value, json};

pub async fn publish_workspace(
    client: &RunClient,
    context: &mut RunContext,
    environment: Value,
    mut workspace: ExecutionWorkspace,
) -> Result<()> {
    if !context
        .session
        .harness_contract
        .outputs
        .contains_key("workspace")
    {
        return Ok(());
    }
    let mut outputs = RunOutputsQuery::default();
    loop {
        let existing = client.run_outputs(client.lease().run_id, &outputs).await?;
        if existing.items.iter().any(|o| o.name == "workspace") {
            return Ok(());
        }
        let Some(after) = existing.next_after_sequence else {
            break;
        };
        outputs.after_sequence = Some(after);
    }
    let state_key = client.lease().run_id.to_string();
    let state = client
        .session_state(&SessionStateQuery {
            namespace: "platform.workspace".into(),
            key: Some(state_key.clone()),
            ..Default::default()
        })
        .await?;
    let mut plan = if let Some(value) = state.items.first().and_then(|s| s.value.clone()) {
        serde_json::from_value::<PublishRunOutput>(Value::Object(value))
            .map_err(|_| Error::Invalid("Invalid saved workspace publication"))?
    } else {
        let mut cursor = None;
        loop {
            let page = client
                .platform()
                .list_environments(&capabilities::PageOptions {
                    limit: Some(200),
                    cursor,
                })
                .await?;
            for e in page.items {
                let same = e.workspace_root == workspace.workspace_root
                    && e.path == workspace.path
                    && match e.kind {
                        EnvironmentType::Machine => {
                            environment["type"] == "machine"
                                && json!(e.machine_id) == environment["machine_id"]
                        }
                        EnvironmentType::Sandbox => {
                            environment["type"] == "sandbox"
                                && json!(e.snapshot_id) == environment["snapshot_id"]
                        }
                    };
                if same {
                    workspace.environment_id = Some(e.id);
                    break;
                }
            }
            if workspace.environment_id.is_some() || page.next_cursor.is_none() {
                break;
            }
            cursor = page.next_cursor;
        }
        let plan = PublishRunOutput {
            name: "workspace".into(),
            output: OutputValue::ExecutionWorkspace(workspace),
        };
        let mut commit = Commit::new(context.run.run.version, context.session.current_revision);
        commit.session_state.push(SessionStateWrite {
            namespace: "platform.workspace".into(),
            key: state_key,
            expected_version: state.items.first().map_or(0, |v| v.version),
            mutation: SessionStateMutation::Set {
                value: json!(plan).as_object().unwrap().clone(),
            },
        });
        client
            .commit(&Command::new(RequestKey::new("workspace-plan-v1")?, commit))
            .await?;
        *context = client.context(&ContextQuery::default()).await?;
        plan
    };
    if let OutputValue::ExecutionWorkspace(w) = &mut plan.output
        && let Some(environment_id) = w.environment_id
    {
        *w = client
            .bind_workspace(&Command::new(
                RequestKey::new("workspace-binding-v1")?,
                capabilities::BindHarnessWorkspace {
                    environment_id,
                    host_id: w.host_id,
                },
            ))
            .await?;
    }
    client
        .publish_output(&Command::new(RequestKey::new("workspace-output-v1")?, plan))
        .await?;
    Ok(())
}

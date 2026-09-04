//! A compile-checked recovery pattern, not an executable worker loop.
use platform_runtime_client::{Command, Error, RequestKey, Result, RunClient, types::*};
use uuid::Uuid;

/// The harness owns the checkpoint schema and phase transitions. Preserve
/// existing state and keep this command after success for crash-safe replay.
#[allow(dead_code)]
async fn ensure_child(run: &RunClient, task: Message) -> Result<ChildResponse> {
    let context = run.context(&ContextQuery::default()).await?;
    let mut state = context
        .checkpoint
        .as_ref()
        .map(|c| c.state.clone())
        .unwrap_or_default();
    let command: Command<Child> = if let Some(saved) = state.get("proof_child_request") {
        serde_json::from_value(saved.clone())
            .map_err(|_| Error::Invalid("invalid saved child request"))?
    } else {
        // Generate only for a genuinely new logical child. After a crash the
        // branch above restores this same key AND payload from Platform.
        let command = Command::new(
            RequestKey::new(Uuid::new_v4().to_string())?,
            Child {
                harness_id: None,
                title: Some("Check the proof".into()),
                fork_at_revision: Some(context.session.current_revision),
                initial_run: StartRun {
                    input: task,
                    expected_session_revision: context.session.current_revision,
                    config_override: Default::default(),
                },
            },
        );
        state.insert(
            "proof_child_request".into(),
            serde_json::to_value(&command)
                .map_err(|_| Error::Invalid("child request cannot be serialized"))?,
        );
        let mut commit = Commit::new(context.run.run.version, context.session.current_revision);
        commit.checkpoint = Some(Checkpoint {
            expected_version: context.checkpoint.as_ref().map_or(0, |c| c.version),
            state,
        });
        let save = Command::new(RequestKey::new(Uuid::new_v4().to_string())?, commit);
        // If uncertain, do not send the child yet. Retry `save` unchanged or
        // reload context on recovery. CAS prevents overwriting newer state.
        run.commit(&save).await?;
        command
    };
    // The worker supervisor maintains heartbeats independently throughout.
    run.create_child(&command).await
}

fn main() {}

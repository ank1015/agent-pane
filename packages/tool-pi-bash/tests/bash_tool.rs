use execution_contracts::{MachineId, WorkspaceRootId};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_runtime::OperationContext;
use llm_contracts::{ContentPart, ToolArguments, Validate as _};
use serde_json::{Value, json};
use tempfile::TempDir;
use tool_pi_bash::{BashArguments, BashToolContext, definition, execute, execute_bash_tool};

struct Fixture {
    _directory: TempDir,
    runtime: LocalExecutionRuntime,
    root_id: WorkspaceRootId,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        tokio::fs::create_dir_all(workspace.join("project"))
            .await
            .expect("create workspace");
        let workspace = tokio::fs::canonicalize(workspace)
            .await
            .expect("canonical workspace path");
        let root_id = id::<WorkspaceRootId>("root");
        let runtime = LocalExecutionRuntime::new(LocalRuntimeConfig {
            machine_id: id::<MachineId>("machine"),
            name: "Pi Bash tool tests".to_owned(),
            state_directory: directory.path().join("state"),
            workspace_roots: vec![LocalWorkspaceRoot {
                id: root_id.clone(),
                name: "workspace".to_owned(),
                path: workspace,
                read_only: false,
            }],
            native_grants: Vec::new(),
        })
        .await
        .expect("local execution runtime");
        Self {
            _directory: directory,
            runtime,
            root_id,
        }
    }

    fn context<'a>(&'a self, operation: &'a OperationContext) -> BashToolContext<'a> {
        BashToolContext::new(&self.runtime, operation, self.root_id.clone(), "project")
            .expect("Bash context")
    }
}

#[test]
fn exports_a_valid_pi_bash_definition() {
    let definition = definition();
    assert_eq!(definition.name(), "bash");
    definition.validate().expect("valid tool definition");
}

#[tokio::test]
async fn executes_typed_and_provider_neutral_commands_in_the_cwd() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let context = fixture.context(&operation);
    let typed = execute(
        BashArguments {
            command: "pwd | sed 's|.*/||'".to_owned(),
            timeout: None,
        },
        &context,
    )
    .await
    .expect("typed command");
    assert_eq!(text(&typed.content), "project\n");

    let Value::Object(arguments) = json!({ "command": "printf 'tool args'" }) else {
        unreachable!()
    };
    let untyped = execute_bash_tool(&ToolArguments::Object(arguments), &context)
        .await
        .expect("tool argument command");
    assert_eq!(text(&untyped.content), "tool args");
}

#[tokio::test]
async fn returns_structured_exit_and_timeout_failures() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let context = fixture.context(&operation);
    let failure = execute(
        BashArguments {
            command: "printf 'before failure'; exit 7".to_owned(),
            timeout: None,
        },
        &context,
    )
    .await
    .expect_err("non-zero command must fail");
    assert_eq!(failure.name(), "command_failed");
    assert!(failure.message().contains("before failure"));
    assert!(failure.message().contains("Command exited with code 7"));

    let timeout = execute(
        BashArguments {
            command: "sleep 1".to_owned(),
            timeout: Some(0.01),
        },
        &context,
    )
    .await
    .expect_err("timed out command must fail");
    assert_eq!(timeout.name(), "command_failed");
    assert!(timeout.message().contains("Command timed out"));
}

#[tokio::test]
async fn truncates_large_output_and_exposes_artifact_metadata() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let context = fixture.context(&operation);
    let output = execute(
        BashArguments {
            command: "i=0; while [ $i -le 2000 ]; do echo $i; i=$((i+1)); done".to_owned(),
            timeout: None,
        },
        &context,
    )
    .await
    .expect("large output command");

    assert!(!text(&output.content).starts_with("0\n"));
    assert!(text(&output.content).contains("Showing lines"));
    let details = output.details.expect("truncation details");
    assert_eq!(details["truncated"], true);
    assert!(details.get("full_output_artifact_id").is_some());
}

fn text(content: &[ContentPart]) -> &str {
    let ContentPart::Text(text) = &content[0] else {
        panic!("first content part must be text")
    };
    &text.content
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid identifier")
}

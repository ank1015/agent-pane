use std::path::Path;

use execution_contracts::{MachineId, WorkspaceRootId};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_runtime::OperationContext;
use llm_contracts::{ContentPart, ToolArguments, Validate as _};
use serde_json::{Value, json};
use tempfile::TempDir;
use tool_pi_write::{WriteArguments, WriteToolContext, definition, execute, execute_write_tool};

struct Fixture {
    _directory: TempDir,
    workspace: std::path::PathBuf,
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
            name: "Pi write tool tests".to_owned(),
            state_directory: directory.path().join("state"),
            workspace_roots: vec![LocalWorkspaceRoot {
                id: root_id.clone(),
                name: "workspace".to_owned(),
                path: workspace.clone(),
                read_only: false,
            }],
            native_grants: Vec::new(),
        })
        .await
        .expect("local execution runtime");
        Self {
            _directory: directory,
            workspace,
            runtime,
            root_id,
        }
    }

    fn context<'a>(&'a self, operation: &'a OperationContext) -> WriteToolContext<'a> {
        WriteToolContext::new(&self.runtime, operation, self.root_id.clone(), "project")
            .expect("write context")
    }
}

#[test]
fn exports_a_valid_pi_write_definition() {
    let definition = definition();
    assert_eq!(definition.name(), "write");
    definition.validate().expect("valid tool definition");
}

#[tokio::test]
async fn creates_parents_and_overwrites_with_typed_arguments() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let context = fixture.context(&operation);
    let path = fixture.workspace.join("project/nested/content.txt");

    let created = execute(
        WriteArguments {
            path: "nested/content.txt".to_owned(),
            content: "first".to_owned(),
        },
        &context,
    )
    .await
    .expect("create file");
    assert_eq!(
        text(&created.content),
        "Successfully wrote 5 bytes to nested/content.txt"
    );
    assert!(created.details.as_ref().unwrap().get("mutation").is_some());
    assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "first");

    execute(
        WriteArguments {
            path: "nested/content.txt".to_owned(),
            content: "replacement".to_owned(),
        },
        &context,
    )
    .await
    .expect("overwrite file");
    assert_eq!(
        tokio::fs::read_to_string(path).await.unwrap(),
        "replacement"
    );
}

#[tokio::test]
async fn parses_provider_neutral_arguments_and_accepts_absolute_workspace_paths() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let context = fixture.context(&operation);
    let path = fixture.workspace.join("project/absolute.txt");
    let Value::Object(arguments) = json!({
        "path": path.to_string_lossy(),
        "content": "absolute"
    }) else {
        unreachable!()
    };

    execute_write_tool(&ToolArguments::Object(arguments), &context)
        .await
        .expect("tool argument write");
    assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), "absolute");
}

#[tokio::test]
async fn rejects_paths_outside_the_workspace() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let context = fixture.context(&operation);
    let error = execute(
        WriteArguments {
            path: Path::new("/outside-the-active-workspace/file.txt")
                .to_string_lossy()
                .into_owned(),
            content: "outside".to_owned(),
        },
        &context,
    )
    .await
    .expect_err("outside path must fail");

    assert_eq!(error.name(), "invalid_path");
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

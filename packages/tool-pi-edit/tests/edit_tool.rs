use std::path::Path;

use execution_contracts::{MachineId, WorkspaceRootId};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_runtime::OperationContext;
use llm_contracts::{ContentPart, ToolArguments, Validate as _};
use serde_json::{Value, json};
use tempfile::TempDir;
use tool_pi_edit::{
    EditArguments, EditInput, EditToolContext, definition, execute, execute_edit_tool,
};

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
            name: "Pi edit tool tests".to_owned(),
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

    fn context<'a>(&'a self, operation: &'a OperationContext) -> EditToolContext<'a> {
        EditToolContext::new(&self.runtime, operation, self.root_id.clone(), "project")
            .expect("edit context")
    }
}

#[test]
fn exports_a_valid_pi_edit_definition() {
    let definition = definition();
    assert_eq!(definition.name(), "edit");
    definition.validate().expect("valid tool definition");
}

#[tokio::test]
async fn applies_typed_disjoint_edits_and_preserves_file_format() {
    let fixture = Fixture::new().await;
    let file = fixture.workspace.join("project/content.txt");
    tokio::fs::write(&file, "\u{feff}alpha\r\nbeta\r\ngamma\r\n")
        .await
        .expect("text fixture");
    let operation = OperationContext::new();
    let context = fixture.context(&operation);

    let output = execute(
        EditArguments {
            path: "content.txt".to_owned(),
            edits: vec![
                EditInput {
                    old_text: "alpha".to_owned(),
                    new_text: "ALPHA".to_owned(),
                },
                EditInput {
                    old_text: "gamma".to_owned(),
                    new_text: "GAMMA".to_owned(),
                },
            ],
        },
        &context,
    )
    .await
    .expect("typed edit");

    assert!(text(&output.content).contains("Successfully replaced 2 block(s)"));
    assert_eq!(output.details.as_ref().unwrap()["first_changed_line"], 1);
    assert_eq!(output.details.as_ref().unwrap()["replacements"], 2);
    assert_eq!(
        tokio::fs::read(&file).await.expect("edited file"),
        "\u{feff}ALPHA\r\nbeta\r\nGAMMA\r\n".as_bytes()
    );
}

#[tokio::test]
async fn parses_camel_case_tool_arguments() {
    let fixture = Fixture::new().await;
    let file = fixture.workspace.join("project/content.txt");
    tokio::fs::write(&file, "before\n")
        .await
        .expect("text fixture");
    let operation = OperationContext::new();
    let context = fixture.context(&operation);
    let Value::Object(arguments) = json!({
        "path": "content.txt",
        "edits": [{ "oldText": "before", "newText": "after" }]
    }) else {
        unreachable!()
    };

    execute_edit_tool(&ToolArguments::Object(arguments), &context)
        .await
        .expect("tool argument edit");
    assert_eq!(
        tokio::fs::read_to_string(file).await.expect("edited file"),
        "after\n"
    );
}

#[tokio::test]
async fn rejects_non_unique_overlapping_and_outside_targets() {
    let fixture = Fixture::new().await;
    tokio::fs::write(
        fixture.workspace.join("project/content.txt"),
        "same same\nabcdef\n",
    )
    .await
    .expect("text fixture");
    let operation = OperationContext::new();
    let context = fixture.context(&operation);

    let duplicate = execute(
        EditArguments {
            path: "content.txt".to_owned(),
            edits: vec![EditInput {
                old_text: "same".to_owned(),
                new_text: "changed".to_owned(),
            }],
        },
        &context,
    )
    .await
    .expect_err("duplicate target must fail");
    assert_eq!(duplicate.name(), "edit_failed");
    assert!(duplicate.message().contains("must be unique"));

    let overlap = execute(
        EditArguments {
            path: "content.txt".to_owned(),
            edits: vec![
                EditInput {
                    old_text: "abcdef".to_owned(),
                    new_text: "first".to_owned(),
                },
                EditInput {
                    old_text: "cde".to_owned(),
                    new_text: "second".to_owned(),
                },
            ],
        },
        &context,
    )
    .await
    .expect_err("overlapping targets must fail");
    assert_eq!(overlap.name(), "edit_failed");
    assert!(overlap.message().contains("overlap"));

    let outside = execute(
        EditArguments {
            path: Path::new("/outside-the-active-workspace/file.txt")
                .to_string_lossy()
                .into_owned(),
            edits: vec![EditInput {
                old_text: "old".to_owned(),
                new_text: "new".to_owned(),
            }],
        },
        &context,
    )
    .await
    .expect_err("outside path must fail");
    assert_eq!(outside.name(), "invalid_path");
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

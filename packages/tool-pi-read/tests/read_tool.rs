use std::path::Path;

use execution_contracts::{MachineId, WorkspaceRootId};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_runtime::OperationContext;
use llm_contracts::{ContentPart, ImageSource, ToolArguments, Validate as _};
use serde_json::{Value, json};
use tempfile::TempDir;
use tool_pi_read::{ReadArguments, ReadToolContext, definition, execute, execute_read_tool};

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
            name: "Pi read tool tests".to_owned(),
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

    fn context<'a>(&'a self, operation: &'a OperationContext) -> ReadToolContext<'a> {
        ReadToolContext::new(&self.runtime, operation, self.root_id.clone(), "project")
            .expect("read context")
    }
}

#[test]
fn exports_a_valid_pi_read_definition() {
    let definition = definition();
    assert_eq!(definition.name(), "read");
    definition.validate().expect("valid tool definition");
}

#[tokio::test]
async fn reads_typed_and_provider_neutral_arguments() {
    let fixture = Fixture::new().await;
    tokio::fs::write(
        fixture.workspace.join("project/content.txt"),
        "alpha\nbeta\ngamma\n",
    )
    .await
    .expect("text fixture");
    let operation = OperationContext::new();
    let context = fixture.context(&operation);

    let typed = execute(
        ReadArguments {
            path: "content.txt".to_owned(),
            offset: Some(2),
            limit: Some(1),
        },
        &context,
    )
    .await
    .expect("typed read");
    assert!(text(&typed.content).starts_with("beta"));
    assert!(text(&typed.content).contains("more lines in file"));
    assert_eq!(typed.details.as_ref().unwrap()["truncated"], true);

    let Value::Object(arguments) = json!({ "path": "content.txt" }) else {
        unreachable!()
    };
    let untyped = execute_read_tool(&ToolArguments::Object(arguments), &context)
        .await
        .expect("tool argument read");
    assert_eq!(text(&untyped.content), "alpha\nbeta\ngamma\n");
}

#[tokio::test]
async fn returns_images_and_confines_absolute_paths_to_the_workspace() {
    let fixture = Fixture::new().await;
    let image = fixture.workspace.join("project/image.png");
    tokio::fs::write(&image, b"image bytes")
        .await
        .expect("image fixture");
    let operation = OperationContext::new();
    let context = fixture.context(&operation);

    let image_output = execute(
        ReadArguments {
            path: image.to_string_lossy().into_owned(),
            offset: None,
            limit: None,
        },
        &context,
    )
    .await
    .expect("image read");
    let ContentPart::Image(image) = &image_output.content[1] else {
        panic!("second content part must be an image")
    };
    let ImageSource::Base64(source) = &image.source else {
        panic!("image must be returned inline")
    };
    assert_eq!(source.mime_type, "image/png");
    assert!(!source.data.is_empty());

    let error = execute(
        ReadArguments {
            path: Path::new("/outside-the-active-workspace/file.txt")
                .to_string_lossy()
                .into_owned(),
            offset: None,
            limit: None,
        },
        &context,
    )
    .await
    .expect_err("outside path must fail");
    assert_eq!(error.name(), "invalid_path");
}

#[tokio::test]
async fn exposes_structured_argument_errors() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let context = fixture.context(&operation);
    let error = execute(
        ReadArguments {
            path: "content.txt".to_owned(),
            offset: Some(0),
            limit: None,
        },
        &context,
    )
    .await
    .expect_err("zero offset must fail");

    assert_eq!(error.name(), "invalid_arguments");
    assert!(error.message().contains("offset must be one-based"));
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

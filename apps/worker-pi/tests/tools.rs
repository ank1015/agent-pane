use std::path::Path;

use execution_contracts::{EnvironmentId, MachineId, WorkspaceRootId};
use execution_local::{LocalExecutionConfig, LocalExecutionEnvironment, LocalWorkspaceRoot};
use execution_runtime::OperationContext;
use llm_contracts::{
    AssistantContent, ContentPart, ImageSource, ToolArguments, ToolCallId, ToolResultMessage,
    ToolResultOutcome,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use worker_pi::harness::tools::{ToolExecutionContext, WorkspaceCwd, execute_tool_call};

struct Fixture {
    _directory: TempDir,
    workspace: std::path::PathBuf,
    environment: LocalExecutionEnvironment,
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
        let environment = LocalExecutionEnvironment::new(LocalExecutionConfig {
            machine_id: id::<MachineId>("machine"),
            environment_id: id::<EnvironmentId>("environment"),
            name: "Pi tool tests".to_owned(),
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
        .expect("local execution environment");
        Self {
            _directory: directory,
            workspace,
            environment,
            root_id,
        }
    }

    fn cwd(&self) -> WorkspaceCwd {
        WorkspaceCwd::new(self.root_id.clone(), "project").expect("workspace cwd")
    }
}

#[tokio::test]
async fn executes_write_read_edit_and_bash() {
    let fixture = Fixture::new().await;
    let cwd = fixture.cwd();
    let operation = OperationContext::new();
    let context = ToolExecutionContext {
        environment: &fixture.environment,
        cwd: &cwd,
        operation: &operation,
    };

    let write = execute(
        &context,
        "write",
        json!({
            "path": "src/content.txt",
            "content": "\u{feff}alpha\r\nbeta\r\ngamma\r\n"
        }),
    )
    .await;
    assert_success(&write);
    assert!(text(&write).contains("Successfully wrote"));

    let read = execute(
        &context,
        "read",
        json!({
            "path": "src/content.txt",
            "offset": 2,
            "limit": 1
        }),
    )
    .await;
    assert_success(&read);
    assert!(text(&read).starts_with("beta"));
    assert!(text(&read).contains("more lines in file"));

    let edit = execute(
        &context,
        "edit",
        json!({
            "path": "src/content.txt",
            "edits": [
                { "oldText": "alpha", "newText": "ALPHA" },
                { "oldText": "gamma", "newText": "GAMMA" }
            ]
        }),
    )
    .await;
    assert_success(&edit);
    assert_eq!(
        tokio::fs::read(fixture.workspace.join("project/src/content.txt"))
            .await
            .expect("edited file"),
        "\u{feff}ALPHA\r\nbeta\r\nGAMMA\r\n".as_bytes()
    );

    let bash = execute(
        &context,
        "bash",
        json!({ "command": "test -f src/content.txt && printf 'shell output'" }),
    )
    .await;
    assert_success(&bash);
    assert_eq!(text(&bash), "shell output");
}

#[tokio::test]
async fn read_returns_images_as_base64_content() {
    let fixture = Fixture::new().await;
    let image_bytes = b"not-a-decoded-image-but-valid-tool-bytes";
    tokio::fs::write(
        fixture.workspace.join("project/image.png"),
        image_bytes.as_slice(),
    )
    .await
    .expect("image fixture");
    let cwd = fixture.cwd();
    let operation = OperationContext::new();
    let context = ToolExecutionContext {
        environment: &fixture.environment,
        cwd: &cwd,
        operation: &operation,
    };

    let result = execute(&context, "read", json!({ "path": "image.png" })).await;
    assert_success(&result);
    let ContentPart::Image(image) = &result.content[1] else {
        panic!("second content part must be an image");
    };
    let ImageSource::Base64(source) = &image.source else {
        panic!("image must be returned inline");
    };
    assert_eq!(source.mime_type, "image/png");
    assert!(!source.data.is_empty());
}

#[tokio::test]
async fn tool_failures_are_returned_to_the_model() {
    let fixture = Fixture::new().await;
    tokio::fs::write(
        fixture.workspace.join("project/duplicate.txt"),
        "same\nsame\n",
    )
    .await
    .expect("duplicate fixture");
    let cwd = fixture.cwd();
    let operation = OperationContext::new();
    let context = ToolExecutionContext {
        environment: &fixture.environment,
        cwd: &cwd,
        operation: &operation,
    };

    let edit = execute(
        &context,
        "edit",
        json!({
            "path": "duplicate.txt",
            "edits": [{ "oldText": "same", "newText": "changed" }]
        }),
    )
    .await;
    assert_error(&edit, "edit_failed");
    assert!(text(&edit).contains("must be unique"));

    let bash = execute(
        &context,
        "bash",
        json!({
            "command": "printf 'before failure'; exit 7"
        }),
    )
    .await;
    assert_error(&bash, "command_failed");
    assert!(text(&bash).contains("before failure"));
    assert!(text(&bash).contains("Command exited with code 7"));

    let malformed = execute(&context, "read", json!({ "offset": 1 })).await;
    assert_error(&malformed, "invalid_arguments");
}

#[tokio::test]
async fn resolves_absolute_paths_only_inside_the_active_root() {
    let fixture = Fixture::new().await;
    let file = fixture.workspace.join("project/absolute.txt");
    tokio::fs::write(&file, "absolute")
        .await
        .expect("absolute fixture");
    let cwd = fixture.cwd();
    let operation = OperationContext::new();
    let context = ToolExecutionContext {
        environment: &fixture.environment,
        cwd: &cwd,
        operation: &operation,
    };

    let result = execute(&context, "read", json!({ "path": file.to_string_lossy() })).await;
    assert_success(&result);
    assert_eq!(text(&result), "absolute");

    let outside = Path::new("/outside-the-active-workspace/file.txt");
    let result = execute(
        &context,
        "read",
        json!({ "path": outside.to_string_lossy() }),
    )
    .await;
    assert_error(&result, "invalid_path");
}

async fn execute(
    context: &ToolExecutionContext<'_>,
    name: &str,
    arguments: Value,
) -> ToolResultMessage {
    let Value::Object(arguments) = arguments else {
        panic!("test arguments must be an object");
    };
    execute_tool_call(
        &AssistantContent::ToolCall {
            name: name.to_owned(),
            arguments: ToolArguments::Object(arguments),
            tool_call_id: ToolCallId::new(format!("{name}-call")).expect("tool call id"),
        },
        context,
    )
    .await
    .expect("assistant tool call")
}

fn text(message: &ToolResultMessage) -> &str {
    let ContentPart::Text(content) = &message.content[0] else {
        panic!("first content part must be text");
    };
    &content.content
}

fn assert_success(message: &ToolResultMessage) {
    assert_eq!(message.outcome, ToolResultOutcome::Success);
}

fn assert_error(message: &ToolResultMessage, expected_name: &str) {
    let ToolResultOutcome::Error { error } = &message.outcome else {
        panic!("tool result must be an error");
    };
    assert_eq!(error.name.as_deref(), Some(expected_name));
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid identifier")
}

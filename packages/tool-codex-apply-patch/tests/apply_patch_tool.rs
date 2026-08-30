use execution_contracts::{MachineId, WorkspaceRootId};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_runtime::OperationContext;
use llm_contracts::{ContentPart, ToolArguments, ToolDefinition, Validate as _};
use tempfile::TempDir;
use tool_codex_apply_patch::{
    APPLY_PATCH_LARK_GRAMMAR, ApplyPatchToolContext, definition, execute, execute_apply_patch_tool,
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
            name: "Codex apply_patch tool tests".to_owned(),
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

    fn context<'a>(&'a self, operation: &'a OperationContext) -> ApplyPatchToolContext<'a> {
        ApplyPatchToolContext::new(&self.runtime, operation, self.root_id.clone(), "project")
            .expect("apply_patch context")
    }
}

#[test]
fn exports_codex_custom_tool_definition() {
    let definition = definition();
    assert_eq!(definition.name(), "apply_patch");
    definition.validate().expect("valid tool definition");
    let ToolDefinition::Custom(tool) = definition else {
        panic!("apply_patch must be a custom tool")
    };
    assert_eq!(tool.format.definition, APPLY_PATCH_LARK_GRAMMAR);
    assert!(tool.description.contains("FREEFORM"));
}

#[tokio::test]
async fn applies_add_update_delete_and_move_with_codex_summary() {
    let fixture = Fixture::new().await;
    tokio::fs::write(
        fixture.workspace.join("project/modify.txt"),
        "alpha\n  beta  \ngamma\n",
    )
    .await
    .expect("modify fixture");
    tokio::fs::write(fixture.workspace.join("project/delete.txt"), "gone\n")
        .await
        .expect("delete fixture");
    tokio::fs::write(fixture.workspace.join("project/old.txt"), "before\n")
        .await
        .expect("move fixture");

    let patch = "*** Begin Patch
*** Delete File: delete.txt
*** Update File: modify.txt
@@
-beta
+BETA
*** Add File: nested/added.txt
+created
*** Update File: old.txt
*** Move to: renamed/new.txt
@@
-before
+after
*** End Patch";
    let operation = OperationContext::new();
    let output = execute(patch, &fixture.context(&operation))
        .await
        .expect("apply patch");

    assert_eq!(
        text(&output.content),
        "Success. Updated the following files:\nA nested/added.txt\nM modify.txt\nM renamed/new.txt\nD delete.txt\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(fixture.workspace.join("project/modify.txt"))
            .await
            .expect("modified file"),
        "alpha\nBETA\ngamma\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(fixture.workspace.join("project/nested/added.txt"))
            .await
            .expect("added file"),
        "created\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(fixture.workspace.join("project/renamed/new.txt"))
            .await
            .expect("moved file"),
        "after\n"
    );
    assert!(!fixture.workspace.join("project/old.txt").exists());
    assert!(!fixture.workspace.join("project/delete.txt").exists());
    assert_eq!(
        output.details.as_ref().unwrap()["modified"][1],
        "renamed/new.txt"
    );
}

#[tokio::test]
async fn custom_string_arguments_execute_and_object_arguments_fail() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    execute_apply_patch_tool(
        &ToolArguments::String(
            "*** Begin Patch\n*** Add File: created.txt\n+hello\n*** End Patch".to_owned(),
        ),
        &fixture.context(&operation),
    )
    .await
    .expect("custom string call");
    assert_eq!(
        tokio::fs::read_to_string(fixture.workspace.join("project/created.txt"))
            .await
            .expect("created file"),
        "hello\n"
    );

    let error = execute_apply_patch_tool(
        &ToolArguments::Object(Default::default()),
        &fixture.context(&operation),
    )
    .await
    .expect_err("JSON object must fail");
    assert_eq!(error.name(), "invalid_arguments");
}

#[tokio::test]
async fn verification_failure_leaves_all_files_unchanged() {
    let fixture = Fixture::new().await;
    let target = fixture.workspace.join("project/target.txt");
    tokio::fs::write(&target, "actual\n")
        .await
        .expect("target fixture");
    let patch = "*** Begin Patch
*** Add File: should-not-exist.txt
+created
*** Update File: target.txt
@@
-missing
+replacement
*** End Patch";
    let operation = OperationContext::new();
    let error = execute(patch, &fixture.context(&operation))
        .await
        .expect_err("verification must fail");

    assert_eq!(error.name(), "apply_patch_failed");
    assert!(error.message().contains("Failed to find expected lines"));
    assert!(
        !fixture
            .workspace
            .join("project/should-not-exist.txt")
            .exists()
    );
    assert_eq!(
        tokio::fs::read_to_string(target)
            .await
            .expect("unchanged target"),
        "actual\n"
    );
}

#[tokio::test]
async fn rejects_paths_outside_the_active_workspace() {
    let fixture = Fixture::new().await;
    let operation = OperationContext::new();
    let error = execute(
        "*** Begin Patch\n*** Add File: ../../outside.txt\n+bad\n*** End Patch",
        &fixture.context(&operation),
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

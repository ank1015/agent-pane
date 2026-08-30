use std::{io::Cursor, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_contracts::{MachineId, WorkspaceRootId};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_runtime::OperationContext;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use llm_contracts::{ContentPart, ImageDetail, ImageSource, ToolArguments, Validate as _};
use serde_json::{Value, json};
use tempfile::TempDir;
use tool_codex_view_image::{
    ViewImageArguments, ViewImageDetail, ViewImageToolContext, definition, execute,
    execute_view_image_tool, parse_arguments,
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
            name: "Codex view-image tool tests".to_owned(),
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

    fn context<'a>(&'a self, operation: &'a OperationContext) -> ViewImageToolContext<'a> {
        ViewImageToolContext::new(&self.runtime, operation, self.root_id.clone(), "project")
            .expect("view-image context")
    }

    async fn write_png(&self, name: &str) -> Vec<u8> {
        let image = RgbImage::from_pixel(2, 1, Rgb([12, 34, 56]));
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut bytes, ImageFormat::Png)
            .expect("encode PNG fixture");
        let bytes = bytes.into_inner();
        tokio::fs::write(self.workspace.join("project").join(name), &bytes)
            .await
            .expect("write PNG fixture");
        bytes
    }
}

#[test]
fn exports_the_codex_view_image_definition() {
    let definition = definition();
    assert_eq!(definition.name(), "view_image");
    definition.validate().expect("valid tool definition");

    let llm_contracts::ToolDefinition::Function(tool) = definition else {
        panic!("view_image must be a function tool")
    };
    assert_eq!(
        tool.description,
        "View a local image file from the filesystem when visual inspection is needed. Use this for images already available on disk."
    );
    assert_eq!(
        Value::Object(tool.parameters),
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Local filesystem path to an image file."
                },
                "detail": {
                    "type": "string",
                    "enum": ["high", "original"],
                    "description": "Image detail level. Defaults to `high`; use `original` to preserve exact resolution."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    );
}

#[test]
fn parses_object_and_string_arguments_with_codex_detail_values() {
    let Value::Object(object) = json!({"path": "diagram.png"}) else {
        unreachable!()
    };
    assert_eq!(
        parse_arguments(&ToolArguments::Object(object)).expect("object arguments"),
        ViewImageArguments {
            path: "diagram.png".to_owned(),
            detail: None,
        }
    );
    assert_eq!(
        parse_arguments(&ToolArguments::String(
            r#"{"path":"diagram.png","detail":"original"}"#.to_owned()
        ))
        .expect("string arguments"),
        ViewImageArguments {
            path: "diagram.png".to_owned(),
            detail: Some(ViewImageDetail::Original),
        }
    );

    let error = parse_arguments(&ToolArguments::String(
        r#"{"path":"diagram.png","detail":"low"}"#.to_owned(),
    ))
    .expect_err("unsupported detail must fail");
    assert_eq!(error.name(), "invalid_arguments");
}

#[tokio::test]
async fn returns_validated_image_content_with_default_high_detail() {
    let fixture = Fixture::new().await;
    let expected = fixture.write_png("diagram.png").await;
    let operation = OperationContext::new();
    let context = fixture.context(&operation);
    let Value::Object(arguments) = json!({"path": "diagram.png"}) else {
        unreachable!()
    };

    let output = execute_view_image_tool(&ToolArguments::Object(arguments), &context)
        .await
        .expect("view image");
    let ContentPart::Image(image) = &output.content[0] else {
        panic!("view_image must return image content")
    };
    let ImageSource::Base64(source) = &image.source else {
        panic!("workspace image must be returned inline")
    };
    assert_eq!(source.mime_type, "image/png");
    assert_eq!(
        STANDARD.decode(&source.data).expect("base64 image"),
        expected
    );
    assert_eq!(image.detail, Some(ImageDetail::High));

    let details = output.details.expect("image details");
    assert_eq!(details["mime_type"], "image/png");
    assert_eq!(details["width"], 2);
    assert_eq!(details["height"], 1);
    assert_eq!(details["detail"], "high");
}

#[tokio::test]
async fn preserves_original_detail_and_accepts_workspace_absolute_paths() {
    let fixture = Fixture::new().await;
    fixture.write_png("original.png").await;
    let absolute = fixture.workspace.join("project/original.png");
    let operation = OperationContext::new();
    let context = fixture.context(&operation);

    let output = execute(
        ViewImageArguments {
            path: absolute.to_string_lossy().into_owned(),
            detail: Some(ViewImageDetail::Original),
        },
        &context,
    )
    .await
    .expect("original image");
    let ContentPart::Image(image) = &output.content[0] else {
        panic!("view_image must return image content")
    };
    assert_eq!(image.detail, Some(ImageDetail::Original));
}

#[tokio::test]
async fn rejects_invalid_images_and_paths_outside_the_workspace() {
    let fixture = Fixture::new().await;
    tokio::fs::write(
        fixture.workspace.join("project/not-an-image.png"),
        b"not image bytes",
    )
    .await
    .expect("invalid image fixture");
    let operation = OperationContext::new();
    let context = fixture.context(&operation);

    let invalid = execute(
        ViewImageArguments {
            path: "not-an-image.png".to_owned(),
            detail: None,
        },
        &context,
    )
    .await
    .expect_err("invalid image must fail");
    assert_eq!(invalid.name(), "invalid_image");
    assert!(invalid.message().contains("unable to process image"));

    let outside = execute(
        ViewImageArguments {
            path: Path::new("/outside-the-active-workspace/image.png")
                .to_string_lossy()
                .into_owned(),
            detail: None,
        },
        &context,
    )
    .await
    .expect_err("outside path must fail");
    assert_eq!(outside.name(), "invalid_path");
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid identifier")
}

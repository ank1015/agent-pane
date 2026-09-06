//! Remote image viewing through an injected execution runtime.
#![doc = include_str!("../README.md")]

mod delivery;
mod processing;
mod reader;
mod schema;

use execution_core::{
    ExecutionError, ExecutionErrorCode as Code, ExecutionPath, ExecutionResult, ExecutionRuntime,
    OperationContext,
};
use llm_contracts::{ContentPart, ToolArguments};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use delivery::{ImageAsset, ImageDelivery, ImagePublisher, PublishImageFuture};
pub use schema::{
    input_schema, input_schema_with_options, output_schema, output_schema_with_options,
};

pub const NAME: &str = "view_image";
pub const DESCRIPTION: &str = "View a local image file from the filesystem when visual inspection is needed. Use this for images already available on disk.";

/// Host selection belongs to the caller. Like Codex, parsing accepts unknown
/// properties and legacy detail hints even when the current schema hides them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ViewImageInput {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ViewImageDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewImageDetail {
    #[default]
    High,
    Original,
}

impl<'de> Deserialize<'de> for ViewImageDetail {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "high" => Ok(Self::High),
            "original" => Ok(Self::Original),
            _ => Err(serde::de::Error::custom(format!(
                "view_image.detail only supports `high` or `original`; omit `detail` for default high resized behavior, got `{value}`"
            ))),
        }
    }
}

/// Supplied from the selected model/turn, never inferred from its name here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewImageOptions {
    pub supports_images: bool,
    pub can_request_original_detail: bool,
    /// Caller resolves feature/model eligibility (Codex enables this for
    /// Responses Lite or models supporting original detail).
    pub unified_image_budget: bool,
}

impl Default for ViewImageOptions {
    fn default() -> Self {
        Self {
            supports_images: true,
            can_request_original_detail: false,
            unified_image_budget: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ViewImageConfig {
    /// Application read limit, distinct from prompt-image preparation limits.
    pub max_image_bytes: u64,
    pub chunk_bytes: u64,
    pub options: ViewImageOptions,
    pub delivery: ImageDelivery,
}

impl Default for ViewImageConfig {
    fn default() -> Self {
        Self {
            max_image_bytes: u64::MAX,
            chunk_bytes: 1024 * 1024,
            options: ViewImageOptions::default(),
            delivery: ImageDelivery::Inline,
        }
    }
}

/// Code-mode output only. Hosted delivery contains no base64 data. Prompt
/// resizing is performed separately by `execute_for_model` before history insertion.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ViewImageOutput {
    pub image_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ViewImageDetail>,
}

impl std::fmt::Debug for ViewImageOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewImageOutput")
            .field("image_url", &self.log_output())
            .field("detail", &self.detail)
            .finish()
    }
}

impl ViewImageOutput {
    #[must_use]
    pub fn log_output(&self) -> String {
        if self.image_url.starts_with("data:") {
            format!("<image data URL omitted: {} bytes>", self.image_url.len())
        } else {
            "<hosted image URL omitted>".into()
        }
    }
    #[must_use]
    pub fn code_mode_result(&self) -> Value {
        serde_json::to_value(self).expect("image output contains only strings and an enum")
    }
}

pub struct ViewImageTool<'a> {
    runtime: &'a dyn ExecutionRuntime,
    cwd: ExecutionPath,
    config: ViewImageConfig,
}

impl<'a> ViewImageTool<'a> {
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        cwd: ExecutionPath,
        config: ViewImageConfig,
    ) -> ExecutionResult<Self> {
        tool_filesystem::validate_cwd(runtime.descriptor(), &cwd)?;
        if config.max_image_bytes == 0 || config.chunk_bytes == 0 {
            return Err(error(
                Code::InvalidRequest,
                "max_image_bytes and chunk_bytes must be positive",
            ));
        }
        Ok(Self {
            runtime,
            cwd,
            config,
        })
    }
    #[must_use]
    pub fn input_schema(&self) -> Value {
        input_schema_with_options(self.config.options)
    }
    #[must_use]
    pub fn output_schema(&self) -> Value {
        let mut schema = output_schema_with_options(self.config.options);
        if matches!(self.config.delivery, ImageDelivery::Hosted(_)) {
            schema["properties"]["image_url"]["description"] =
                Value::String("HTTP or HTTPS URL for the loaded image.".into());
        }
        schema
    }
    #[must_use]
    pub fn definition(&self) -> llm_contracts::ToolDefinition {
        llm_contracts::ToolDefinition::Function(llm_contracts::FunctionTool {
            name: NAME.into(),
            description: DESCRIPTION.into(),
            parameters: self
                .input_schema()
                .as_object()
                .expect("object schema")
                .clone(),
            output_schema: Some(
                self.output_schema()
                    .as_object()
                    .expect("object schema")
                    .clone(),
            ),
            strict: Some(false),
        })
    }
    #[must_use]
    pub const fn description(&self) -> &'static str {
        DESCRIPTION
    }

    pub fn resolve_path(&self, path: &str) -> ExecutionResult<ExecutionPath> {
        tool_filesystem::resolve_path_normalized(self.runtime.descriptor(), &self.cwd, path)
    }

    /// Parse function arguments separately so callers can choose code-mode or
    /// model-history delivery. Check image support before argument validation.
    pub fn parse_arguments(&self, arguments: &ToolArguments) -> ExecutionResult<ViewImageInput> {
        self.ensure_image_support()?;
        let ToolArguments::Object(arguments) = arguments else {
            return Err(error(
                Code::InvalidRequest,
                "view_image handler received unsupported payload",
            ));
        };
        serde_json::from_value(Value::Object(arguments.clone()))
            .map_err(|failure| error(Code::InvalidRequest, failure.to_string()))
    }

    /// Code mode: validate and return unchanged original bytes (or a URL to
    /// those bytes). This method never performs prompt-image resizing.
    pub async fn execute(
        &self,
        context: &OperationContext,
        input: ViewImageInput,
    ) -> ExecutionResult<ViewImageOutput> {
        let (bytes, mime, detail) = self.read_validated(context, input).await?;
        let image_url = self.config.delivery.raw_url(context, &bytes, mime).await?;
        Ok(ViewImageOutput {
            image_url,
            detail: (!self.config.options.unified_image_budget).then_some(detail),
        })
    }

    /// Direct model-history adapter. Prepare once at history insertion and
    /// persist the returned content part; hosted mode uploads prepared bytes
    /// and returns only their URL. Do not serialize `execute()` as tool text.
    pub async fn execute_for_model(
        &self,
        context: &OperationContext,
        input: ViewImageInput,
    ) -> ExecutionResult<ContentPart> {
        let (bytes, _, detail) = self.read_validated(context, input).await?;
        let prepared = processing::prepare(bytes, detail)?;
        context.checkpoint()?;
        self.config
            .delivery
            .model_content(context, &prepared.bytes, prepared.mime, detail)
            .await
    }

    fn ensure_image_support(&self) -> ExecutionResult<()> {
        if !self.config.options.supports_images {
            return Err(error(
                Code::Unsupported,
                "view_image is not allowed because you do not support image inputs",
            ));
        }
        Ok(())
    }

    async fn read_validated(
        &self,
        context: &OperationContext,
        input: ViewImageInput,
    ) -> ExecutionResult<(Vec<u8>, &'static str, ViewImageDetail)> {
        self.ensure_image_support()?;
        context.checkpoint()?;
        if input.environment_id.is_some() {
            return Err(error(
                Code::InvalidRequest,
                "select the execution environment before calling this host-bound view_image tool",
            ));
        }
        let path = self.resolve_path(&input.path)?;
        let bytes = reader::read_image(
            self.runtime,
            context,
            path,
            self.config.max_image_bytes,
            self.config.chunk_bytes,
        )
        .await?;
        context.checkpoint()?;
        let mime = processing::validate(&bytes)?;
        context.checkpoint()?;
        let options = self.config.options;
        let detail = if options.unified_image_budget
            || (options.can_request_original_detail
                && input.detail == Some(ViewImageDetail::Original))
        {
            ViewImageDetail::Original
        } else {
            ViewImageDetail::High
        };
        Ok((bytes, mime, detail))
    }
}

pub(crate) fn error(code: Code, message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(code, message).with_detail("source", "tool-view-image")
}

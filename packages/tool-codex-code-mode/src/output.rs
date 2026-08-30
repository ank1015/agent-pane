use std::time::Duration;

use codex_code_mode_runtime::{
    DEFAULT_MAX_OUTPUT_TOKENS_PER_EXEC_CALL, FunctionCallOutputContentItem,
    ImageDetail as RuntimeImageDetail, RuntimeResponse,
};
use llm_contracts::{
    Base64ImageSource, ContentPart, ImageContent, ImageDetail, ImageSource, TextContent,
};
use serde_json::{Value, json};

/// Transcript-facing output shared by `exec` and `wait`.
#[derive(Clone, Debug, PartialEq)]
pub struct CodeModeToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl CodeModeToolOutput {
    pub(crate) fn from_runtime(
        response: RuntimeResponse,
        max_output_tokens: Option<usize>,
        elapsed: Duration,
    ) -> Self {
        let (cell_id, status, success, live, mut items) = match response {
            RuntimeResponse::Yielded {
                cell_id,
                content_items,
            } => (cell_id, "running", true, true, content_items),
            RuntimeResponse::Terminated {
                cell_id,
                content_items,
            } => (cell_id, "terminated", true, false, content_items),
            RuntimeResponse::Result {
                cell_id,
                content_items,
                error_text,
            } => {
                let success = error_text.is_none();
                let status = if success { "completed" } else { "failed" };
                let mut content_items = content_items;
                if let Some(error_text) = error_text {
                    content_items.push(FunctionCallOutputContentItem::InputText {
                        text: format!("Script error:\n{error_text}"),
                    });
                }
                (cell_id, status, success, false, content_items)
            }
        };

        items = truncate_items(items, max_output_tokens);
        let audio_urls = items
            .iter()
            .filter_map(|item| match item {
                FunctionCallOutputContentItem::InputAudio { audio_url } => Some(audio_url.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let wall_time_seconds = rounded_wall_time(elapsed);
        let mut content = Vec::with_capacity(items.len() + 1);
        content.push(text_part(format!(
            "{}\nWall time {wall_time_seconds:.1} seconds\nOutput:\n",
            status_heading(status, &cell_id)
        )));
        content.extend(items.into_iter().map(runtime_item));

        Self {
            content,
            details: Some(json!({
                "cell_id": cell_id.as_str(),
                "status": status,
                "success": success,
                "live": live,
                "wall_time_seconds": wall_time_seconds,
                "audio_urls": audio_urls,
            })),
        }
    }
}

/// Structured adapter failure that a harness can place in its tool result.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CodeModeToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl CodeModeToolError {
    pub(crate) fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
            details: None,
        }
    }

    pub(crate) fn invalid_arguments(message: impl std::fmt::Display) -> Self {
        Self::new("invalid_arguments", message.to_string())
    }

    pub(crate) fn runtime(message: impl Into<String>) -> Self {
        Self::new("code_mode_runtime_error", message)
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }

    #[must_use]
    pub fn into_parts(self) -> (&'static str, String, Option<Value>) {
        (self.name, self.message, self.details)
    }
}

fn status_heading(status: &str, cell_id: &codex_code_mode_runtime::CellId) -> String {
    match status {
        "running" => format!("Script running with cell ID {cell_id}"),
        "terminated" => "Script terminated".to_string(),
        "completed" => "Script completed".to_string(),
        "failed" => "Script failed".to_string(),
        _ => format!("Script {status}"),
    }
}

fn rounded_wall_time(elapsed: Duration) -> f32 {
    (elapsed.as_secs_f32() * 10.0).round() / 10.0
}

fn runtime_item(item: FunctionCallOutputContentItem) -> ContentPart {
    match item {
        FunctionCallOutputContentItem::InputText { text } => text_part(text),
        FunctionCallOutputContentItem::InputImage { image_url, detail } => {
            image_part(&image_url, detail).unwrap_or_else(|| text_part(image_url))
        }
        // `llm-contracts` does not yet have a provider-neutral audio content
        // variant. Preserve the data URL in both model-visible text and
        // structured details rather than silently dropping it.
        FunctionCallOutputContentItem::InputAudio { audio_url } => {
            text_part(format!("Audio output: {audio_url}"))
        }
    }
}

fn image_part(image_url: &str, detail: Option<RuntimeImageDetail>) -> Option<ContentPart> {
    let data = image_url.strip_prefix("data:")?;
    let (metadata, payload) = data.split_once(',')?;
    let mime_type = metadata.strip_suffix(";base64")?;
    if mime_type.is_empty() || payload.is_empty() {
        return None;
    }
    Some(ContentPart::Image(ImageContent {
        source: ImageSource::Base64(Base64ImageSource {
            data: payload.to_string(),
            mime_type: mime_type.to_string(),
        }),
        detail: detail.map(runtime_image_detail),
        metadata: None,
    }))
}

const fn runtime_image_detail(detail: RuntimeImageDetail) -> ImageDetail {
    match detail {
        RuntimeImageDetail::Auto => ImageDetail::Auto,
        RuntimeImageDetail::Low => ImageDetail::Low,
        RuntimeImageDetail::High => ImageDetail::High,
        RuntimeImageDetail::Original => ImageDetail::Original,
    }
}

fn text_part(content: impl Into<String>) -> ContentPart {
    ContentPart::Text(TextContent {
        content: content.into(),
        metadata: None,
    })
}

fn truncate_items(
    items: Vec<FunctionCallOutputContentItem>,
    max_output_tokens: Option<usize>,
) -> Vec<FunctionCallOutputContentItem> {
    let max_tokens = max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS_PER_EXEC_CALL);
    let text_bytes = items
        .iter()
        .map(|item| match item {
            FunctionCallOutputContentItem::InputText { text } => text.len(),
            FunctionCallOutputContentItem::InputAudio { audio_url } => audio_url.len(),
            FunctionCallOutputContentItem::InputImage { .. } => 0,
        })
        .sum::<usize>();
    let budget = max_tokens.saturating_mul(4);
    if text_bytes <= budget {
        return items;
    }

    items
        .into_iter()
        .map(|item| match item {
            FunctionCallOutputContentItem::InputText { text } => {
                let item_budget = proportional_budget(text.len(), text_bytes, budget);
                FunctionCallOutputContentItem::InputText {
                    text: truncate_middle(&text, item_budget),
                }
            }
            FunctionCallOutputContentItem::InputAudio { audio_url } => {
                let item_budget = proportional_budget(audio_url.len(), text_bytes, budget);
                FunctionCallOutputContentItem::InputAudio {
                    audio_url: truncate_middle(&audio_url, item_budget),
                }
            }
            image @ FunctionCallOutputContentItem::InputImage { .. } => image,
        })
        .collect()
}

fn proportional_budget(item_bytes: usize, total_bytes: usize, budget: usize) -> usize {
    if total_bytes == 0 {
        return 0;
    }
    item_bytes.saturating_mul(budget) / total_bytes
}

fn truncate_middle(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let left_budget = max_bytes / 2;
    let right_budget = max_bytes.saturating_sub(left_budget);
    let mut left_end = left_budget.min(value.len());
    while !value.is_char_boundary(left_end) {
        left_end = left_end.saturating_sub(1);
    }
    let mut right_start = value.len().saturating_sub(right_budget);
    while right_start < value.len() && !value.is_char_boundary(right_start) {
        right_start += 1;
    }
    right_start = right_start.max(left_end);
    let removed_tokens = right_start.saturating_sub(left_end).saturating_add(3) / 4;
    format!(
        "{}…{removed_tokens} tokens truncated…{}",
        &value[..left_end],
        &value[right_start..]
    )
}

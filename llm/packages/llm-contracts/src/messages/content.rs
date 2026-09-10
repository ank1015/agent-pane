use serde::{Deserialize, Serialize};

use crate::{
    JsonObject, Validate, ValidationError,
    validation::{finish, issue, require_non_empty},
};

/// Text plus optional application-owned metadata.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TextContent {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonObject>,
}

/// Provider-facing image quality hint.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ImageDetail {
    Auto,
    Low,
    High,
    Original,
}

/// Base64-encoded image input.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Base64ImageSource {
    pub data: String,
    pub mime_type: String,
}

/// Remotely hosted image input.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UrlImageSource {
    pub url: String,
}

/// Source of image bytes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ImageSource {
    Base64(Base64ImageSource),
    Url(UrlImageSource),
}

/// Image plus optional provider and application hints.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ImageContent {
    pub source: ImageSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ImageDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonObject>,
}

/// User or tool-result content.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ContentPart {
    Text(TextContent),
    Image(ImageContent),
    Audio(AudioContent),
}

/// Inline audio emitted by code-mode tools, preserving the provider data URL.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AudioContent {
    pub audio_url: String,
}

impl Validate for ContentPart {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if let Self::Image(image) = self {
            validate_image(image, &mut issues);
        }
        if let Self::Audio(audio) = self {
            if !audio.audio_url.starts_with("data:audio/") || !audio.audio_url.contains(";base64,")
            {
                issue(&mut issues, "audio_url", "must be a base64 audio data URL");
            }
        }
        finish(issues)
    }
}

pub(crate) fn validate_content(content: &[ContentPart], path: &str) -> Result<(), ValidationError> {
    let mut issues = Vec::new();
    if content.is_empty() {
        issue(&mut issues, path, "must contain at least one item");
    }
    for (index, part) in content.iter().enumerate() {
        if let Err(error) = part.validate() {
            for nested in error.issues {
                issue(
                    &mut issues,
                    format!("{path}[{index}].{}", nested.path),
                    nested.message,
                );
            }
        }
    }
    finish(issues)
}

fn validate_image(image: &ImageContent, issues: &mut Vec<crate::ValidationIssue>) {
    match &image.source {
        ImageSource::Base64(source) => {
            require_non_empty(issues, "source.data", &source.data);
            require_non_empty(issues, "source.mime_type", &source.mime_type);
            if source.mime_type.len() > 255 {
                issue(issues, "source.mime_type", "must not exceed 255 bytes");
            }
        }
        ImageSource::Url(source) => match url::Url::parse(&source.url) {
            Ok(url) if url.has_host() && matches!(url.scheme(), "http" | "https") => {}
            _ => issue(
                issues,
                "source.url",
                "must be a fully qualified HTTP or HTTPS URL",
            ),
        },
    }
}

//! Provider-neutral preparation of images emitted in tool results.
//!
//! Codex validates and prepares images at the durable history boundary, after
//! nested tools have observed their raw result but before the next provider
//! request. Agent's transcript is immutable, so this harness applies the same
//! policy to the normalized request copy instead.

use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{
    ColorType, DynamicImage, GenericImageView as _, ImageDecoder as _, ImageEncoder, ImageFormat,
    ImageReader,
    codecs::{jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
    imageops::FilterType,
};
use llm_contracts::{
    Base64ImageSource, ContentPart, ImageContent, ImageDetail, ImageSource, Message, TextContent,
};

const IMAGE_PROCESSING_ERROR_PLACEHOLDER: &str =
    "image content omitted because it could not be processed";
const IMAGE_TOO_LARGE_PLACEHOLDER: &str =
    "image content omitted because it exceeded the supported size limit; use a smaller image";
const UNSUPPORTED_LOW_DETAIL_PLACEHOLDER: &str = "image content omitted because detail 'low' is not supported; use 'high', 'original', or 'auto'";
const REMOTE_IMAGE_URL_PLACEHOLDER: &str =
    "image content omitted because remote image URLs are not supported";

const PROMPT_IMAGE_PATCH_SIZE: u32 = 32;
const MAX_PROMPT_IMAGE_INPUT_BYTES: usize = 1024 * 1024 * 1024;
const HIGH_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 2048,
    max_patches: 2_500,
};
const ORIGINAL_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 6000,
    max_patches: 10_000,
};

#[derive(Clone, Copy)]
struct PromptImageResizeLimits {
    max_dimension: u32,
    max_patches: usize,
}

struct ImageMetadata {
    icc_profile: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug)]
enum ImagePreparationFailure {
    RemoteUrl,
    UnsupportedLowDetail,
    TooLarge,
    Processing,
}

impl ImagePreparationFailure {
    const fn placeholder(self) -> &'static str {
        match self {
            Self::RemoteUrl => REMOTE_IMAGE_URL_PLACEHOLDER,
            Self::UnsupportedLowDetail => UNSUPPORTED_LOW_DETAIL_PLACEHOLDER,
            Self::TooLarge => IMAGE_TOO_LARGE_PLACEHOLDER,
            Self::Processing => IMAGE_PROCESSING_ERROR_PLACEHOLDER,
        }
    }
}

/// Prepares only tool-result images. This catches direct `view_image` output
/// and images emitted by a top-level code-mode `exec` result while leaving the
/// raw nested `{ image_url, detail }` return value unchanged.
pub(super) fn prepare_tool_result_images(messages: &mut [Message]) {
    for message in messages {
        let Message::ToolResult(result) = message else {
            continue;
        };
        for part in &mut result.content {
            let ContentPart::Image(image) = part else {
                continue;
            };
            if let Err(error) = prepare_image(image) {
                tracing::warn!(?error, "failed to prepare tool output image");
                *part = ContentPart::Text(TextContent {
                    content: error.placeholder().to_owned(),
                    metadata: None,
                });
            }
        }
    }
}

fn prepare_image(image: &mut ImageContent) -> Result<(), ImagePreparationFailure> {
    let ImageSource::Base64(source) = &image.source else {
        return Err(ImagePreparationFailure::RemoteUrl);
    };
    let limits = match image.detail {
        None | Some(ImageDetail::Auto | ImageDetail::High) => HIGH_DETAIL_LIMITS,
        Some(ImageDetail::Original) => ORIGINAL_DETAIL_LIMITS,
        Some(ImageDetail::Low) => return Err(ImagePreparationFailure::UnsupportedLowDetail),
    };
    if source.data.len() > MAX_PROMPT_IMAGE_INPUT_BYTES {
        return Err(ImagePreparationFailure::TooLarge);
    }
    let bytes = STANDARD
        .decode(&source.data)
        .map_err(|_| ImagePreparationFailure::Processing)?;
    if bytes.len() > MAX_PROMPT_IMAGE_INPUT_BYTES {
        return Err(ImagePreparationFailure::TooLarge);
    }
    let prepared = prepare_bytes(bytes, limits)?;
    image.source = ImageSource::Base64(Base64ImageSource {
        data: STANDARD.encode(prepared.bytes),
        mime_type: prepared.mime_type.to_owned(),
    });
    Ok(())
}

struct PreparedImage {
    bytes: Vec<u8>,
    mime_type: &'static str,
}

fn prepare_bytes(
    bytes: Vec<u8>,
    limits: PromptImageResizeLimits,
) -> Result<PreparedImage, ImagePreparationFailure> {
    let guessed_format =
        image::guess_format(&bytes).map_err(|_| ImagePreparationFailure::Processing)?;
    let source_format = match guessed_format {
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP => {
            Some(guessed_format)
        }
        _ => None,
    };

    let mut decoder = ImageReader::with_format(Cursor::new(&bytes), guessed_format)
        .into_decoder()
        .map_err(|_| ImagePreparationFailure::Processing)?;
    let metadata = ImageMetadata {
        // RGB profiles are safe across every re-encoding path. Decoding a
        // CMYK/YCCK JPEG can convert pixels to RGB while retaining its source
        // profile, so copying that profile would mislabel the output.
        icc_profile: decoder
            .icc_profile()
            .ok()
            .flatten()
            .filter(|profile| profile.get(16..20) == Some(b"RGB ")),
        exif: decoder.exif_metadata().ok().flatten(),
    };
    let dynamic =
        DynamicImage::from_decoder(decoder).map_err(|_| ImagePreparationFailure::Processing)?;
    let (width, height) = dynamic.dimensions();
    let target_dimensions = prompt_image_output_dimensions(width, height, limits);

    if target_dimensions == (width, height) && source_format.is_some_and(can_preserve_source_bytes)
    {
        let format = source_format.expect("preservable source format is present");
        return Ok(PreparedImage {
            bytes,
            mime_type: format_to_mime(format),
        });
    }

    let prepared = if target_dimensions == (width, height) {
        dynamic
    } else {
        dynamic.resize_exact(
            target_dimensions.0,
            target_dimensions.1,
            FilterType::Triangle,
        )
    };
    let target_format = source_format
        .filter(|format| can_preserve_source_bytes(*format))
        .unwrap_or(ImageFormat::Png);
    let bytes = encode_image(&prepared, target_format, metadata)?;
    Ok(PreparedImage {
        bytes,
        mime_type: format_to_mime(target_format),
    })
}

fn prompt_image_output_dimensions(
    width: u32,
    height: u32,
    limits: PromptImageResizeLimits,
) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    if dimensions_fit(width, height, limits) {
        return (width, height);
    }

    let max_dimension_scale =
        (f64::from(limits.max_dimension) / f64::from(width.max(height))).min(1.0);
    let width = ((f64::from(width) * max_dimension_scale).round() as u32).max(1);
    let height = ((f64::from(height) * max_dimension_scale).round() as u32).max(1);
    if dimensions_fit(width, height, limits) {
        return (width, height);
    }

    let width_f64 = f64::from(width);
    let height_f64 = f64::from(height);
    let patch_size = f64::from(PROMPT_IMAGE_PATCH_SIZE);
    let mut scale =
        (patch_size * patch_size * limits.max_patches as f64 / width_f64 / height_f64).sqrt();
    let scaled_patches_wide = width_f64 * scale / patch_size;
    let scaled_patches_high = height_f64 * scale / patch_size;
    scale *= (scaled_patches_wide.floor() / scaled_patches_wide)
        .min(scaled_patches_high.floor() / scaled_patches_high);

    (
        ((width_f64 * scale).floor() as u32).max(1),
        ((height_f64 * scale).floor() as u32).max(1),
    )
}

fn dimensions_fit(width: u32, height: u32, limits: PromptImageResizeLimits) -> bool {
    let patches_wide = width.div_ceil(PROMPT_IMAGE_PATCH_SIZE);
    let patches_high = height.div_ceil(PROMPT_IMAGE_PATCH_SIZE);
    let patch_count = u64::from(patches_wide) * u64::from(patches_high);
    width <= limits.max_dimension
        && height <= limits.max_dimension
        && patch_count <= limits.max_patches as u64
}

const fn can_preserve_source_bytes(format: ImageFormat) -> bool {
    // GIF is decoded but intentionally not passed through. The API supports
    // only non-animated GIF input, so Codex sends its decoded first frame PNG.
    matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    )
}

fn encode_image(
    image: &DynamicImage,
    format: ImageFormat,
    metadata: ImageMetadata,
) -> Result<Vec<u8>, ImagePreparationFailure> {
    let mut buffer = Vec::new();
    let ImageMetadata { icc_profile, exif } = metadata;
    match format {
        ImageFormat::Png => {
            let rgba = image.to_rgba8();
            let mut encoder = PngEncoder::new(&mut buffer);
            apply_image_metadata(&mut encoder, icc_profile, exif)?;
            encoder
                .write_image(
                    rgba.as_raw(),
                    image.width(),
                    image.height(),
                    ColorType::Rgba8.into(),
                )
                .map_err(|_| ImagePreparationFailure::Processing)?;
        }
        ImageFormat::Jpeg => {
            let mut encoder = JpegEncoder::new_with_quality(&mut buffer, 85);
            apply_image_metadata(&mut encoder, icc_profile, exif)?;
            encoder
                .encode_image(image)
                .map_err(|_| ImagePreparationFailure::Processing)?;
        }
        ImageFormat::WebP => {
            let rgba = image.to_rgba8();
            let mut encoder = WebPEncoder::new_lossless(&mut buffer);
            apply_image_metadata(&mut encoder, icc_profile, exif)?;
            encoder
                .write_image(
                    rgba.as_raw(),
                    image.width(),
                    image.height(),
                    ColorType::Rgba8.into(),
                )
                .map_err(|_| ImagePreparationFailure::Processing)?;
        }
        _ => unreachable!("unsupported target image format"),
    }
    Ok(buffer)
}

fn apply_image_metadata(
    encoder: &mut impl ImageEncoder,
    icc_profile: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
) -> Result<(), ImagePreparationFailure> {
    if let Some(icc_profile) = icc_profile {
        encoder
            .set_icc_profile(icc_profile)
            .map_err(|_| ImagePreparationFailure::Processing)?;
    }
    if let Some(exif) = exif {
        encoder
            .set_exif_metadata(exif)
            .map_err(|_| ImagePreparationFailure::Processing)?;
    }
    Ok(())
}

const fn format_to_mime(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => "image/png",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};
    use llm_contracts::{
        MessageId, Timestamp, ToolCallId, ToolResultMessage, ToolResultOutcome, UrlImageSource,
    };

    fn encoded_image(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let image = ImageBuffer::from_pixel(width, height, Rgba([10_u8, 20, 30, 255]));
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, format)
            .expect("encode image fixture");
        encoded.into_inner()
    }

    fn image_part(bytes: &[u8], detail: ImageDetail) -> ContentPart {
        ContentPart::Image(ImageContent {
            source: ImageSource::Base64(Base64ImageSource {
                data: STANDARD.encode(bytes),
                mime_type: "application/octet-stream".to_owned(),
            }),
            detail: Some(detail),
            metadata: None,
        })
    }

    fn tool_result(content: Vec<ContentPart>) -> Message {
        Message::ToolResult(ToolResultMessage {
            id: MessageId::new("result").expect("message ID"),
            tool_name: "exec".to_owned(),
            tool_call_id: ToolCallId::new("call").expect("tool call ID"),
            content,
            details: None,
            timestamp: Timestamp(0),
            outcome: ToolResultOutcome::Success,
        })
    }

    fn prepared_image(message: &Message, index: usize) -> (&Base64ImageSource, ImageDetail) {
        let Message::ToolResult(result) = message else {
            panic!("expected tool result")
        };
        let ContentPart::Image(image) = &result.content[index] else {
            panic!("expected prepared image")
        };
        let ImageSource::Base64(source) = &image.source else {
            panic!("expected inline image")
        };
        (source, image.detail.expect("image detail"))
    }

    #[test]
    fn applies_codex_high_and_original_resize_budgets() {
        let high = encoded_image(ImageFormat::Png, 2048, 2048);
        let original = encoded_image(ImageFormat::Png, 6401, 100);
        let mut messages = vec![tool_result(vec![
            image_part(&high, ImageDetail::High),
            image_part(&original, ImageDetail::Original),
        ])];

        prepare_tool_result_images(&mut messages);

        let (high, high_detail) = prepared_image(&messages[0], 0);
        let high = image::load_from_memory(&STANDARD.decode(&high.data).expect("base64"))
            .expect("prepared high image");
        assert_eq!(high.dimensions(), (1600, 1600));
        assert_eq!(high_detail, ImageDetail::High);

        let (original, original_detail) = prepared_image(&messages[0], 1);
        let original = image::load_from_memory(&STANDARD.decode(&original.data).expect("base64"))
            .expect("prepared original image");
        assert_eq!(original.dimensions(), (6000, 94));
        assert_eq!(original_detail, ImageDetail::Original);
    }

    #[test]
    fn decodes_gif_and_transcodes_its_first_frame_to_png() {
        let gif = encoded_image(ImageFormat::Gif, 3, 2);
        let mut messages = vec![tool_result(vec![image_part(&gif, ImageDetail::High)])];

        prepare_tool_result_images(&mut messages);

        let (prepared, detail) = prepared_image(&messages[0], 0);
        assert_eq!(prepared.mime_type, "image/png");
        assert_eq!(detail, ImageDetail::High);
        assert_eq!(
            image::guess_format(&STANDARD.decode(&prepared.data).expect("base64"))
                .expect("prepared format"),
            ImageFormat::Png
        );
    }

    #[test]
    fn replaces_only_failed_images_with_codex_placeholders() {
        let valid = encoded_image(ImageFormat::Png, 2, 1);
        let mut messages = vec![tool_result(vec![
            ContentPart::Text(TextContent {
                content: "before".to_owned(),
                metadata: None,
            }),
            ContentPart::Image(ImageContent {
                source: ImageSource::Base64(Base64ImageSource {
                    data: "%%%".to_owned(),
                    mime_type: "image/png".to_owned(),
                }),
                detail: Some(ImageDetail::High),
                metadata: None,
            }),
            ContentPart::Image(ImageContent {
                source: ImageSource::Base64(Base64ImageSource {
                    data: STANDARD.encode(b"not an image"),
                    mime_type: "image/png".to_owned(),
                }),
                detail: Some(ImageDetail::High),
                metadata: None,
            }),
            ContentPart::Image(ImageContent {
                source: ImageSource::Url(UrlImageSource {
                    url: "https://example.com/image.png".to_owned(),
                }),
                detail: Some(ImageDetail::High),
                metadata: None,
            }),
            image_part(&valid, ImageDetail::Low),
            image_part(&valid, ImageDetail::High),
        ])];

        prepare_tool_result_images(&mut messages);

        let Message::ToolResult(result) = &messages[0] else {
            panic!("expected tool result")
        };
        let texts = result
            .content
            .iter()
            .map(|part| match part {
                ContentPart::Text(text) => Some(text.content.as_str()),
                ContentPart::Image(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            vec![
                Some("before"),
                Some(IMAGE_PROCESSING_ERROR_PLACEHOLDER),
                Some(IMAGE_PROCESSING_ERROR_PLACEHOLDER),
                Some(REMOTE_IMAGE_URL_PLACEHOLDER),
                Some(UNSUPPORTED_LOW_DETAIL_PLACEHOLDER),
                None,
            ]
        );
        assert_eq!(prepared_image(&messages[0], 5).0.mime_type, "image/png");
    }
}

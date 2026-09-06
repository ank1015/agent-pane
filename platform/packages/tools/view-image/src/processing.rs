use crate::{ViewImageDetail, error};
use execution_core::{ExecutionErrorCode as Code, ExecutionResult};
use image::{
    ColorType, DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat,
    ImageReader,
    codecs::{jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
    imageops::FilterType,
};
use std::io::Cursor;

const PATCH_SIZE: u32 = 32;
const HIGH_MAX_DIMENSION: u32 = 2048;
const HIGH_MAX_PATCHES: usize = 2_500;
const ORIGINAL_MAX_DIMENSION: u32 = 6000;
const ORIGINAL_MAX_PATCHES: usize = 10_000;

#[derive(Debug)]
pub(crate) struct PreparedImage {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
}

struct ImageMetadata {
    icc_profile: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
}

pub(crate) fn prepare(
    file_bytes: Vec<u8>,
    detail: ViewImageDetail,
) -> ExecutionResult<PreparedImage> {
    ensure_prompt_input_size(file_bytes.len())?;
    let guessed = image::guess_format(&file_bytes).map_err(|_| invalid_image())?;
    if !matches!(
        guessed,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return Err(invalid_image());
    }

    let mut decoder = ImageReader::with_format(Cursor::new(&file_bytes), guessed)
        .into_decoder()
        .map_err(|_| invalid_image())?;
    let metadata = ImageMetadata {
        // Copy only RGB profiles: decoding JPEG can turn CMYK/YCCK pixels into
        // RGB while retaining a profile that no longer describes the output.
        icc_profile: decoder
            .icc_profile()
            .ok()
            .flatten()
            .filter(|profile| profile.get(16..20) == Some(b"RGB ")),
        exif: decoder.exif_metadata().ok().flatten(),
    };
    let dynamic = DynamicImage::from_decoder(decoder).map_err(|_| invalid_image())?;
    let (source_width, source_height) = dynamic.dimensions();
    let (max_dimension, max_patches) = match detail {
        ViewImageDetail::High => (HIGH_MAX_DIMENSION, HIGH_MAX_PATCHES),
        ViewImageDetail::Original => (ORIGINAL_MAX_DIMENSION, ORIGINAL_MAX_PATCHES),
    };
    let (width, height) =
        output_dimensions(source_width, source_height, max_dimension, max_patches);

    // Codex passes PNG/JPEG/WebP through when no conversion is needed. GIF is
    // decoded to its first display frame and normalized to PNG.
    if (width, height) == (source_width, source_height)
        && matches!(
            guessed,
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
        )
    {
        return Ok(PreparedImage {
            bytes: file_bytes,
            mime: mime(guessed),
        });
    }

    let image = if (width, height) == (source_width, source_height) {
        dynamic
    } else {
        dynamic.resize_exact(width, height, FilterType::Triangle)
    };
    let target = match guessed {
        ImageFormat::Jpeg => ImageFormat::Jpeg,
        ImageFormat::WebP => ImageFormat::WebP,
        _ => ImageFormat::Png,
    };
    let bytes = encode(&image, target, metadata)?;
    Ok(PreparedImage {
        bytes,
        mime: mime(target),
    })
}

// Match Codex's history data-URL limit without allocating a base64 copy.
// Apply the same preparation budget to hosted delivery as to inline delivery.
fn ensure_prompt_input_size(byte_count: usize) -> ExecutionResult<()> {
    const MAX_INPUT: usize = 1024 * 1024 * 1024;
    let encoded_len = byte_count.div_ceil(3).checked_mul(4);
    if byte_count > MAX_INPUT || encoded_len.is_none_or(|len| len > MAX_INPUT) {
        return Err(error(
            Code::ResourceExhausted,
            "image content omitted because it exceeded the supported size limit; use a smaller image",
        ));
    }
    Ok(())
}

fn output_dimensions(
    width: u32,
    height: u32,
    max_dimension: u32,
    max_patches: usize,
) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    if dimensions_fit(width, height, max_dimension, max_patches) {
        return (width, height);
    }

    let dimension_scale = (f64::from(max_dimension) / f64::from(width.max(height))).min(1.0);
    let width = ((f64::from(width) * dimension_scale).round() as u32).max(1);
    let height = ((f64::from(height) * dimension_scale).round() as u32).max(1);
    if dimensions_fit(width, height, max_dimension, max_patches) {
        return (width, height);
    }

    let width_f64 = f64::from(width);
    let height_f64 = f64::from(height);
    let patch_size = f64::from(PATCH_SIZE);
    let mut scale = (patch_size * patch_size * max_patches as f64 / width_f64 / height_f64).sqrt();
    let scaled_patches_wide = width_f64 * scale / patch_size;
    let scaled_patches_high = height_f64 * scale / patch_size;
    scale *= (scaled_patches_wide.floor() / scaled_patches_wide)
        .min(scaled_patches_high.floor() / scaled_patches_high);

    (
        ((width_f64 * scale).floor() as u32).max(1),
        ((height_f64 * scale).floor() as u32).max(1),
    )
}

fn dimensions_fit(width: u32, height: u32, max_dimension: u32, max_patches: usize) -> bool {
    let patches_wide = width.div_ceil(PATCH_SIZE);
    let patches_high = height.div_ceil(PATCH_SIZE);
    let patch_count = u64::from(patches_wide) * u64::from(patches_high);
    width <= max_dimension && height <= max_dimension && patch_count <= max_patches as u64
}

fn encode(
    image: &DynamicImage,
    format: ImageFormat,
    metadata: ImageMetadata,
) -> ExecutionResult<Vec<u8>> {
    let mut output = Vec::new();
    let ImageMetadata { icc_profile, exif } = metadata;
    match format {
        ImageFormat::Png => {
            let rgba = image.to_rgba8();
            let mut encoder = PngEncoder::new(&mut output);
            apply_metadata(&mut encoder, icc_profile, exif)?;
            encoder
                .write_image(
                    rgba.as_raw(),
                    image.width(),
                    image.height(),
                    ColorType::Rgba8.into(),
                )
                .map_err(encode_error)?;
        }
        ImageFormat::Jpeg => {
            let mut encoder = JpegEncoder::new_with_quality(&mut output, 85);
            apply_metadata(&mut encoder, icc_profile, exif)?;
            encoder.encode_image(image).map_err(encode_error)?;
        }
        ImageFormat::WebP => {
            let rgba = image.to_rgba8();
            let mut encoder = WebPEncoder::new_lossless(&mut output);
            apply_metadata(&mut encoder, icc_profile, exif)?;
            encoder
                .write_image(
                    rgba.as_raw(),
                    image.width(),
                    image.height(),
                    ColorType::Rgba8.into(),
                )
                .map_err(encode_error)?;
        }
        _ => unreachable!("target format is restricted above"),
    }
    Ok(output)
}

fn apply_metadata(
    encoder: &mut impl ImageEncoder,
    icc_profile: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
) -> ExecutionResult<()> {
    if let Some(profile) = icc_profile {
        encoder
            .set_icc_profile(profile)
            .map_err(|_| encode_failure())?;
    }
    if let Some(exif) = exif {
        encoder
            .set_exif_metadata(exif)
            .map_err(|_| encode_failure())?;
    }
    Ok(())
}

const fn mime(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => "image/png",
    }
}

pub(crate) fn validate(bytes: &[u8]) -> ExecutionResult<&'static str> {
    image::load_from_memory(bytes).map_err(|_| invalid_image())?;
    match image::guess_format(bytes).map_err(|_| invalid_image())? {
        ImageFormat::Png => Ok("image/png"),
        ImageFormat::Jpeg => Ok("image/jpeg"),
        ImageFormat::Gif => Ok("image/gif"),
        ImageFormat::WebP => Ok("image/webp"),
        _ => Err(invalid_image()),
    }
}

fn invalid_image() -> execution_core::ExecutionError {
    error(
        Code::Unsupported,
        "unable to process image: invalid or unsupported image data",
    )
}

fn encode_error(_: image::ImageError) -> execution_core::ExecutionError {
    encode_failure()
}

fn encode_failure() -> execution_core::ExecutionError {
    error(Code::Internal, "unable to encode the prepared image")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = ImageBuffer::from_pixel(width, height, Rgba([20u8, 80, 220, 255]));
        let mut output = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut output), ImageFormat::Png)
            .expect("encode fixture");
        output
    }

    #[test]
    fn prompt_size_limit_matches_codex_base64_boundary_without_large_allocations() {
        let largest = (1024 * 1024 * 1024 / 4) * 3;
        ensure_prompt_input_size(largest).unwrap();
        assert_eq!(
            ensure_prompt_input_size(largest + 1).unwrap_err().code,
            Code::ResourceExhausted
        );
        assert!(ensure_prompt_input_size(usize::MAX).is_err());
    }

    #[test]
    fn high_detail_uses_codex_dimension_and_patch_budgets() {
        let prepared = prepare(png(2304, 864), ViewImageDetail::High).expect("prepare image");
        let image = image::load_from_memory(&prepared.bytes).expect("decode result");
        assert_eq!(image.dimensions(), (2048, 768));

        let prepared = prepare(png(2048, 2048), ViewImageDetail::High).expect("prepare image");
        let image = image::load_from_memory(&prepared.bytes).expect("decode result");
        assert_eq!(image.dimensions(), (1600, 1600));
    }

    #[test]
    fn original_preserves_ordinary_source_bytes_and_dimensions() {
        let source = png(2304, 864);
        let prepared = prepare(source.clone(), ViewImageDetail::Original).expect("prepare image");
        assert_eq!(prepared.bytes, source);
        assert_eq!(prepared.mime, "image/png");
    }

    #[test]
    fn rejects_invalid_and_unsupported_images() {
        let failure =
            prepare(b"not an image".to_vec(), ViewImageDetail::High).expect_err("invalid input");
        assert_eq!(failure.code, Code::Unsupported);
        assert_eq!(failure.details["source"], "tool-view-image");
    }
}

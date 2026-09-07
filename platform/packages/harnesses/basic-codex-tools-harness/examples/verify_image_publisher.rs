//! Explicit live diagnostic: publishes a generated 2x2 PNG, replays the upload,
//! then verifies anonymous retrieval. No model call or workspace image upload.
use basic_codex_tools_harness::GcsImagePublisher;
use execution_core::OperationContext;
use sha2::{Digest, Sha256};
use tool_view_image::{ImageAsset, ImagePublisher};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let publisher = GcsImagePublisher::new(std::env::var("BASIC_CODEX_IMAGE_BUCKET")?)?;
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        2,
        2,
        image::Rgb([30, 90, 160]),
    ));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png)?;
    let bytes = bytes.into_inner();
    let context = OperationContext::with_timeout(std::time::Duration::from_secs(60));
    let asset = || ImageAsset {
        bytes: &bytes,
        mime_type: "image/png",
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    };
    let url = publisher.publish(&context, asset()).await?;
    assert_eq!(publisher.publish(&context, asset()).await?, url);
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .get(&url)
        .send()
        .await?
        .error_for_status()?;
    assert_eq!(
        response.headers()[reqwest::header::CONTENT_TYPE],
        "image/png"
    );
    assert_eq!(response.bytes().await?.as_ref(), bytes);
    println!("Verified immutable upload, duplicate replay, and anonymous PNG retrieval: {url}");
    Ok(())
}

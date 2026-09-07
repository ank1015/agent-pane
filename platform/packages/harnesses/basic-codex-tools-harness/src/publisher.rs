//! Public, immutable GCS image publication. Credentials never enter harness state.
use execution_core::{
    ExecutionError, ExecutionErrorCode as Code, ExecutionResult, OperationContext,
};
use std::time::Duration;
use tool_view_image::{ImageAsset, ImagePublisher, PublishImageFuture};

/// Uses the worker host's gcloud credentials (including configured impersonation).
/// Provision the bucket separately and grant public object reads. Uploads use a
/// generation precondition: replay can create an object, but never overwrite it.
pub struct GcsImagePublisher {
    bucket: String,
    client: reqwest::Client,
}
impl GcsImagePublisher {
    pub fn new(bucket: impl Into<String>) -> ExecutionResult<Self> {
        let bucket = bucket.into();
        if !(3..=63).contains(&bucket.len())
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !bucket.as_bytes()[0].is_ascii_alphanumeric()
            || !bucket.as_bytes()[bucket.len() - 1].is_ascii_alphanumeric()
        {
            return Err(error(
                "Image bucket must be a 3–63 character GCS bucket name using lowercase letters, numbers and hyphens",
            ));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| error("Could not create image upload client"))?;
        Ok(Self { bucket, client })
    }
    async fn upload(
        &self,
        context: &OperationContext,
        asset: ImageAsset<'_>,
    ) -> ExecutionResult<String> {
        context.checkpoint()?;
        let token = tokio::process::Command::new("gcloud")
            .args(["auth", "print-access-token", "--quiet"])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|_| error("Image publishing requires gcloud on the worker host"))?;
        if !token.status.success() {
            return Err(error(
                "gcloud could not obtain image-storage credentials; authenticate the worker identity",
            ));
        }
        let token = zeroize::Zeroizing::new(token.stdout);
        let token = std::str::from_utf8(&token)
            .map_err(|_| error("Invalid image-storage credentials"))?
            .trim();
        context.checkpoint()?;
        let key = format!("images/{}", asset.sha256);
        let response = self
            .client
            .post(format!(
                "https://storage.googleapis.com/upload/storage/v1/b/{}/o",
                self.bucket
            ))
            .query(&[
                ("uploadType", "media"),
                ("name", key.as_str()),
                ("ifGenerationMatch", "0"),
            ])
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, asset.mime_type)
            .body(asset.bytes.to_vec())
            .send()
            .await
            .map_err(|_| error("Image upload failed; retry view_image"))?;
        // A duplicate content-addressed upload is successful. This identity is
        // application-owned; neither model paths nor filenames select objects.
        if !response.status().is_success()
            && response.status() != reqwest::StatusCode::PRECONDITION_FAILED
        {
            return Err(error(&format!(
                "Image upload returned HTTP {}; check bucket and worker storage permissions",
                response.status().as_u16()
            )));
        }
        Ok(format!(
            "https://storage.googleapis.com/{}/{key}",
            self.bucket
        ))
    }
}
impl ImagePublisher for GcsImagePublisher {
    fn publish<'a>(
        &'a self,
        context: &'a OperationContext,
        asset: ImageAsset<'a>,
    ) -> PublishImageFuture<'a> {
        Box::pin(async move {
            let timeout = context
                .remaining()
                .unwrap_or(Duration::from_secs(60))
                .min(Duration::from_secs(60));
            tokio::select! {
                _ = context.cancelled() => Err(ExecutionError::cancelled()),
                result = tokio::time::timeout(timeout, self.upload(context, asset)) => result.unwrap_or_else(|_| Err(error("Image publication timed out; retry view_image"))),
            }
        })
    }
}
fn error(message: &str) -> ExecutionError {
    ExecutionError::new(Code::Io, message)
}

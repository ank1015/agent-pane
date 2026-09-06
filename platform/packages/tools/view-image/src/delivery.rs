use std::{fmt, future::Future, pin::Pin, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_core::{ExecutionErrorCode as Code, ExecutionResult, OperationContext};
use llm_contracts::{
    Base64ImageSource, ContentPart, ImageContent, ImageDetail, ImageSource, UrlImageSource,
    Validate,
};
use sha2::{Digest, Sha256};

use crate::{ViewImageDetail, error};

/// A validated image to publish. Publishers must store these exact bytes and
/// return an immutable HTTP(S) URL accessible to the selected model provider.
/// Scope object ownership/retention in the publisher; do not use model paths as keys.
pub struct ImageAsset<'a> {
    pub bytes: &'a [u8],
    pub mime_type: &'static str,
    /// SHA-256 of the bytes, suitable for an idempotent, content-addressed key.
    pub sha256: String,
}

impl fmt::Debug for ImageAsset<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageAsset")
            .field("byte_count", &self.bytes.len())
            .field("mime_type", &self.mime_type)
            .field("sha256", &self.sha256)
            .finish()
    }
}

pub type PublishImageFuture<'a> =
    Pin<Box<dyn Future<Output = ExecutionResult<String>> + Send + 'a>>;

/// Storage adapter supplied by the application (for example, its bucket client).
/// Respect cancellation/deadlines, and make repeated publication idempotent.
/// The tool does not fetch returned URLs or silently fall back to inline data.
pub trait ImagePublisher: Send + Sync {
    fn publish<'a>(
        &'a self,
        context: &'a OperationContext,
        asset: ImageAsset<'a>,
    ) -> PublishImageFuture<'a>;
}

#[derive(Clone, Default)]
pub enum ImageDelivery {
    #[default]
    Inline,
    Hosted(Arc<dyn ImagePublisher>),
}

impl fmt::Debug for ImageDelivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Inline => "Inline",
            Self::Hosted(_) => "Hosted(<publisher>)",
        })
    }
}

impl ImageDelivery {
    async fn publish(
        &self,
        context: &OperationContext,
        bytes: &[u8],
        mime_type: &'static str,
    ) -> ExecutionResult<String> {
        let Self::Hosted(publisher) = self else {
            unreachable!("only called for hosted delivery")
        };
        context.checkpoint()?;
        let url = publisher
            .publish(
                context,
                ImageAsset {
                    bytes,
                    mime_type,
                    sha256: format!("{:x}", Sha256::digest(bytes)),
                },
            )
            .await?;
        context.checkpoint()?;
        // Validate without echoing URLs: signed URLs may contain credentials.
        let content = ContentPart::Image(ImageContent {
            source: ImageSource::Url(UrlImageSource { url: url.clone() }),
            detail: None,
            metadata: None,
        });
        content.validate().map_err(|_| {
            error(
                Code::InvalidRequest,
                "image publisher must return an absolute HTTP or HTTPS URL",
            )
        })?;
        Ok(url)
    }

    pub(crate) async fn raw_url(
        &self,
        context: &OperationContext,
        bytes: &[u8],
        mime_type: &'static str,
    ) -> ExecutionResult<String> {
        context.checkpoint()?;
        let url = match self {
            Self::Inline => format!(
                "data:application/octet-stream;base64,{}",
                STANDARD.encode(bytes)
            ),
            Self::Hosted(_) => self.publish(context, bytes, mime_type).await?,
        };
        context.checkpoint()?;
        Ok(url)
    }

    pub(crate) async fn model_content(
        &self,
        context: &OperationContext,
        bytes: &[u8],
        mime_type: &'static str,
        detail: ViewImageDetail,
    ) -> ExecutionResult<ContentPart> {
        context.checkpoint()?;
        let source = match self {
            Self::Inline => ImageSource::Base64(Base64ImageSource {
                data: STANDARD.encode(bytes),
                mime_type: mime_type.into(),
            }),
            Self::Hosted(_) => ImageSource::Url(UrlImageSource {
                url: self.publish(context, bytes, mime_type).await?,
            }),
        };
        context.checkpoint()?;
        Ok(ContentPart::Image(ImageContent {
            source,
            detail: Some(match detail {
                ViewImageDetail::High => ImageDetail::High,
                ViewImageDetail::Original => ImageDetail::Original,
            }),
            metadata: None,
        }))
    }
}

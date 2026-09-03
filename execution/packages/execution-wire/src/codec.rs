use std::io;

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::{RequestEnvelope, ResponseEnvelope};

/// Default maximum JSON payload size, excluding the trailing newline.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum WireCodecError {
    #[error("wire I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("wire JSON was invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("wire frame was empty")]
    EmptyFrame,
    #[error("wire frame exceeded the {max_bytes}-byte limit")]
    FrameTooLarge { max_bytes: usize },
    #[error("maximum wire frame size must be greater than zero")]
    InvalidFrameLimit,
}

/// Bounded newline-delimited JSON framing for execution requests and responses.
#[derive(Clone, Copy, Debug)]
pub struct NdjsonCodec {
    max_frame_bytes: usize,
}

impl NdjsonCodec {
    pub fn new(max_frame_bytes: usize) -> Result<Self, WireCodecError> {
        if max_frame_bytes == 0 {
            return Err(WireCodecError::InvalidFrameLimit);
        }
        Ok(Self { max_frame_bytes })
    }

    #[must_use]
    pub const fn max_frame_bytes(&self) -> usize {
        self.max_frame_bytes
    }

    pub async fn read_request<R>(
        &self,
        reader: &mut R,
    ) -> Result<Option<RequestEnvelope>, WireCodecError>
    where
        R: AsyncBufRead + Unpin,
    {
        self.read_json(reader).await
    }

    pub async fn write_request<W>(
        &self,
        writer: &mut W,
        request: &RequestEnvelope,
    ) -> Result<(), WireCodecError>
    where
        W: AsyncWrite + Unpin,
    {
        self.write_json(writer, request).await
    }

    pub async fn read_response<R>(
        &self,
        reader: &mut R,
    ) -> Result<Option<ResponseEnvelope>, WireCodecError>
    where
        R: AsyncBufRead + Unpin,
    {
        self.read_json(reader).await
    }

    pub async fn write_response<W>(
        &self,
        writer: &mut W,
        response: &ResponseEnvelope,
    ) -> Result<(), WireCodecError>
    where
        W: AsyncWrite + Unpin,
    {
        self.write_json(writer, response).await
    }

    async fn read_json<T, R>(&self, reader: &mut R) -> Result<Option<T>, WireCodecError>
    where
        T: DeserializeOwned,
        R: AsyncBufRead + Unpin,
    {
        let Some(frame) = self.read_frame(reader).await? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(&frame)?))
    }

    async fn write_json<T, W>(&self, writer: &mut W, value: &T) -> Result<(), WireCodecError>
    where
        T: Serialize,
        W: AsyncWrite + Unpin,
    {
        let frame = serde_json::to_vec(value)?;
        if frame.len() > self.max_frame_bytes {
            return Err(WireCodecError::FrameTooLarge {
                max_bytes: self.max_frame_bytes,
            });
        }
        writer.write_all(&frame).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }

    async fn read_frame<R>(&self, reader: &mut R) -> Result<Option<Vec<u8>>, WireCodecError>
    where
        R: AsyncBufRead + Unpin,
    {
        let mut frame = Vec::new();

        loop {
            let (consumed, found_newline, reached_eof) = {
                let available = reader.fill_buf().await?;
                if available.is_empty() {
                    (0, false, true)
                } else if let Some(index) = available.iter().position(|byte| *byte == b'\n') {
                    let payload = &available[..index];
                    if frame.len().saturating_add(payload.len()) > self.max_frame_bytes {
                        return Err(WireCodecError::FrameTooLarge {
                            max_bytes: self.max_frame_bytes,
                        });
                    }
                    frame.extend_from_slice(payload);
                    (index + 1, true, false)
                } else {
                    if frame.len().saturating_add(available.len()) > self.max_frame_bytes {
                        return Err(WireCodecError::FrameTooLarge {
                            max_bytes: self.max_frame_bytes,
                        });
                    }
                    frame.extend_from_slice(available);
                    (available.len(), false, false)
                }
            };

            reader.consume(consumed);

            if found_newline || reached_eof {
                if reached_eof && frame.is_empty() {
                    return Ok(None);
                }
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                if frame.is_empty() {
                    return Err(WireCodecError::EmptyFrame);
                }
                return Ok(Some(frame));
            }
        }
    }
}

impl Default for NdjsonCodec {
    fn default() -> Self {
        Self {
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use crate::{Operation, RequestId};

    use super::*;

    #[tokio::test]
    async fn requests_round_trip_through_ndjson() {
        let codec = NdjsonCodec::default();
        let request = RequestEnvelope::new(
            RequestId::new("request-1").expect("valid request ID"),
            Operation::Describe,
        );
        let (client, server) = tokio::io::duplex(4096);
        let (_client_reader, mut client_writer) = tokio::io::split(client);
        let mut server_reader = BufReader::new(server);

        codec
            .write_request(&mut client_writer, &request)
            .await
            .expect("write request");
        let decoded = codec
            .read_request(&mut server_reader)
            .await
            .expect("read request")
            .expect("request frame");
        assert_eq!(decoded, request);
    }

    #[tokio::test]
    async fn final_frame_may_end_at_eof_without_a_newline() {
        let codec = NdjsonCodec::default();
        let request = RequestEnvelope::new(
            RequestId::new("request-1").expect("valid request ID"),
            Operation::Describe,
        );
        let bytes = serde_json::to_vec(&request).expect("serialize request");
        let mut reader = BufReader::new(bytes.as_slice());
        assert_eq!(
            codec.read_request(&mut reader).await.expect("read request"),
            Some(request)
        );
        assert!(
            codec
                .read_request(&mut reader)
                .await
                .expect("read EOF")
                .is_none()
        );
    }

    #[tokio::test]
    async fn codec_accepts_crlf_and_rejects_blank_frames() {
        let codec = NdjsonCodec::default();
        let request = RequestEnvelope::new(
            RequestId::new("request-1").expect("valid request ID"),
            Operation::Describe,
        );
        let mut bytes = serde_json::to_vec(&request).expect("serialize request");
        bytes.extend_from_slice(b"\r\n");
        let mut reader = BufReader::new(bytes.as_slice());
        assert_eq!(
            codec.read_request(&mut reader).await.expect("read request"),
            Some(request)
        );

        let mut blank = BufReader::new(b"\n".as_slice());
        assert!(matches!(
            codec.read_request(&mut blank).await,
            Err(WireCodecError::EmptyFrame)
        ));
    }

    #[tokio::test]
    async fn codec_enforces_frame_limits_on_read_and_write() {
        let codec = NdjsonCodec::new(8).expect("valid frame limit");
        let mut oversized = BufReader::new(b"123456789\n".as_slice());
        assert!(matches!(
            codec.read_request(&mut oversized).await,
            Err(WireCodecError::FrameTooLarge { max_bytes: 8 })
        ));

        let request = RequestEnvelope::new(
            RequestId::new("request-1").expect("valid request ID"),
            Operation::Describe,
        );
        let mut output = Vec::new();
        assert!(matches!(
            codec.write_request(&mut output, &request).await,
            Err(WireCodecError::FrameTooLarge { max_bytes: 8 })
        ));
    }
}

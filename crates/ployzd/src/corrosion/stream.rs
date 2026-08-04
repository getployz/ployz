use std::fmt;
use std::pin::Pin;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};

type ResponseBytes = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

pub(crate) struct NdjsonStream {
    body: ResponseBytes,
    buffered: BytesMut,
    idle_timeout: Duration,
    max_frame_bytes: usize,
    reached_eof: bool,
}

impl fmt::Debug for NdjsonStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NdjsonStream")
            .field("buffered_bytes", &self.buffered.len())
            .field("idle_timeout", &self.idle_timeout)
            .field("max_frame_bytes", &self.max_frame_bytes)
            .field("reached_eof", &self.reached_eof)
            .finish_non_exhaustive()
    }
}

impl NdjsonStream {
    pub(crate) fn new(
        response: reqwest::Response,
        idle_timeout: Duration,
        max_frame_bytes: usize,
    ) -> Self {
        Self {
            body: Box::pin(response.bytes_stream()),
            buffered: BytesMut::new(),
            idle_timeout,
            max_frame_bytes,
            reached_eof: false,
        }
    }

    pub(crate) async fn next(&mut self) -> Result<Option<serde_json::Value>, StreamReadError> {
        let Some(frame) = self.next_frame().await? else {
            return Ok(None);
        };

        serde_json::from_slice(&frame)
            .map(Some)
            .map_err(|source| StreamReadError::InvalidJson {
                detail: source.to_string(),
            })
    }

    async fn next_frame(&mut self) -> Result<Option<Bytes>, StreamReadError> {
        loop {
            if let Some(newline) = self.buffered.iter().position(|byte| *byte == b'\n') {
                if newline > self.max_frame_bytes {
                    return Err(StreamReadError::FrameTooLarge {
                        limit: self.max_frame_bytes,
                    });
                }

                let mut frame = self.buffered.split_to(newline + 1);
                frame.truncate(newline);
                if frame.last().copied() == Some(b'\r') {
                    frame.truncate(frame.len() - 1);
                }
                if frame.is_empty() {
                    return Err(StreamReadError::InvalidJson {
                        detail: "empty NDJSON frame".to_owned(),
                    });
                }
                return Ok(Some(frame.freeze()));
            }

            if self.buffered.len() > self.max_frame_bytes {
                return Err(StreamReadError::FrameTooLarge {
                    limit: self.max_frame_bytes,
                });
            }

            if self.reached_eof {
                if self.buffered.is_empty() {
                    return Ok(None);
                }
                return Err(StreamReadError::TruncatedFrame);
            }

            let next = tokio::time::timeout(self.idle_timeout, self.body.next())
                .await
                .map_err(|_| StreamReadError::IdleTimeout {
                    timeout: self.idle_timeout,
                })?;

            match next {
                Some(Ok(chunk)) => self.buffered.extend_from_slice(&chunk),
                Some(Err(source)) => {
                    return Err(StreamReadError::Transport {
                        detail: source.without_url().to_string(),
                    });
                }
                None => self.reached_eof = true,
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StreamReadError {
    #[error("Corrosion stream was idle for {timeout:?}")]
    IdleTimeout { timeout: Duration },
    #[error("Corrosion NDJSON frame exceeded the {limit}-byte limit")]
    FrameTooLarge { limit: usize },
    #[error("Corrosion stream ended with an unterminated NDJSON frame")]
    TruncatedFrame,
    #[error("Corrosion stream contained invalid JSON: {detail}")]
    InvalidJson { detail: String },
    #[error("Corrosion stream transport failed: {detail}")]
    Transport { detail: String },
}

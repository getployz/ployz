//! Docker-owned containerd content access for pushed OCI image blobs.

use containerd_client::services::v1::{
    CreateRequest, DeleteRequest, InfoRequest, ReadContentRequest, WriteAction,
    WriteContentRequest, WriteContentResponse,
};
use containerd_client::tonic::metadata::{Ascii, MetadataValue};
use containerd_client::tonic::transport::Channel;
use containerd_client::tonic::{Code, Request, Status};
use futures_util::{Stream, StreamExt, stream};
use ployz_core::image::OciDigest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use time::format_description::well_known::Rfc3339;
use time::{Duration as TimeDuration, OffsetDateTime};

const CONTAINERD_NAMESPACE: &str = "moby";
const CONTAINERD_NAMESPACE_HEADER: &str = "containerd-namespace";
const CONTAINERD_LEASE_HEADER: &str = "containerd-lease";
const CONTAINERD_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const INGEST_LEASE_LIFETIME: TimeDuration = TimeDuration::hours(1);
const DOCKER_CONTAINERD_SOCKETS: [&str; 4] = [
    "/var/run/docker/containerd/containerd.sock",
    "/run/docker/containerd/containerd.sock",
    "/var/run/containerd/containerd.sock",
    "/run/containerd/containerd.sock",
];

#[derive(Clone)]
pub struct ContainerdContentStore {
    channel: Channel,
}

impl ContainerdContentStore {
    pub async fn connect_docker_default() -> Result<Self, ContainerdContentError> {
        let Some(socket) = DOCKER_CONTAINERD_SOCKETS
            .iter()
            .map(PathBuf::from)
            .find(|path| path.exists())
        else {
            return Err(ContainerdContentError::SocketNotFound {
                searched: DOCKER_CONTAINERD_SOCKETS.map(PathBuf::from).to_vec(),
            });
        };
        Self::connect(socket).await
    }

    pub async fn connect(socket: impl AsRef<Path>) -> Result<Self, ContainerdContentError> {
        let socket = socket.as_ref().to_path_buf();
        let channel =
            tokio::time::timeout(CONTAINERD_RPC_TIMEOUT, containerd_client::connect(&socket))
                .await
                .map_err(|_| ContainerdContentError::ConnectTimedOut {
                    socket: socket.clone(),
                })?
                .map_err(|error| ContainerdContentError::Connect {
                    socket,
                    message: error.to_string(),
                })?;
        Ok(Self { channel })
    }

    pub async fn blob_info(
        &self,
        digest: &OciDigest,
    ) -> Result<Option<ContentBlobInfo>, ContainerdContentError> {
        let request = scoped_request(InfoRequest {
            digest: digest.as_str().to_owned(),
        })?;
        let response = containerd_client::Client::from(self.channel.clone())
            .content()
            .info(request)
            .await;
        let response = match response {
            Ok(response) => response.into_inner(),
            Err(status) if status.code() == Code::NotFound => return Ok(None),
            Err(status) => return Err(rpc_error("stat content", status)),
        };
        let Some(info) = response.info else {
            return Err(ContainerdContentError::Protocol {
                message: "content info response omitted info".to_owned(),
            });
        };
        let size = u64::try_from(info.size).map_err(|_| ContainerdContentError::Protocol {
            message: format!("content {} reported negative size {}", digest, info.size),
        })?;
        Ok(Some(ContentBlobInfo { size }))
    }

    pub async fn read_blob(
        &self,
        digest: &OciDigest,
    ) -> Result<Option<Vec<u8>>, ContainerdContentError> {
        let request = scoped_request(ReadContentRequest {
            digest: digest.as_str().to_owned(),
            offset: 0,
            size: 0,
        })?;
        let response = containerd_client::Client::from(self.channel.clone())
            .content()
            .read(request)
            .await;
        let mut response = match response {
            Ok(response) => response.into_inner(),
            Err(status) if status.code() == Code::NotFound => return Ok(None),
            Err(status) => return Err(rpc_error("read content", status)),
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .message()
            .await
            .map_err(|status| rpc_error("read content stream", status))?
        {
            let expected =
                i64::try_from(bytes.len()).map_err(|_| ContainerdContentError::Protocol {
                    message: format!("content {} exceeds supported size", digest),
                })?;
            if chunk.offset != expected {
                return Err(ContainerdContentError::Protocol {
                    message: format!(
                        "content {} returned offset {}, expected {}",
                        digest, chunk.offset, expected
                    ),
                });
            }
            bytes.extend_from_slice(&chunk.data);
        }
        Ok(Some(bytes))
    }

    pub async fn acquire_lease(&self) -> Result<ContentLease, ContainerdContentError> {
        let expires_at = (OffsetDateTime::now_utc() + INGEST_LEASE_LIFETIME)
            .format(&Rfc3339)
            .map_err(|error| ContainerdContentError::Protocol {
                message: format!("format lease expiration: {error}"),
            })?;
        let id = format!("ployz-image-{}", nuid::next().to_ascii_lowercase());
        let request = scoped_request(CreateRequest {
            id,
            labels: HashMap::from([("containerd.io/gc.expire".to_owned(), expires_at)]),
        })?;
        let response = containerd_client::Client::from(self.channel.clone())
            .leases()
            .create(request)
            .await
            .map_err(|status| rpc_error("create lease", status))?
            .into_inner();
        let Some(lease) = response.lease else {
            return Err(ContainerdContentError::Protocol {
                message: "lease create response omitted lease".to_owned(),
            });
        };
        if lease.id.is_empty() {
            return Err(ContainerdContentError::Protocol {
                message: "lease create response returned an empty id".to_owned(),
            });
        }
        Ok(ContentLease { id: lease.id })
    }

    pub async fn release_lease(&self, lease: ContentLease) -> Result<(), ContainerdContentError> {
        let request = scoped_request(DeleteRequest {
            id: lease.id,
            sync: true,
        })?;
        containerd_client::Client::from(self.channel.clone())
            .leases()
            .delete(request)
            .await
            .map_err(|status| rpc_error("delete lease", status))?;
        Ok(())
    }

    pub async fn write_ingest_chunk(
        &self,
        ingest: &ContentIngest,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<u64, ContainerdContentError> {
        validate_chunk(ingest.total_size, offset, bytes.len())?;
        let response = self
            .write_message(
                ingest,
                WriteContentRequest {
                    action: WriteAction::Write.into(),
                    r#ref: ingest.reference.clone(),
                    total: checked_i64(ingest.total_size, "ingest total size")?,
                    expected: ingest.digest.as_str().to_owned(),
                    offset: checked_i64(offset, "ingest offset")?,
                    data: bytes,
                    labels: HashMap::new(),
                },
            )
            .await?;
        checked_u64(response.offset, "containerd write offset")
    }

    pub async fn commit_ingest(
        &self,
        ingest: &ContentIngest,
        offset: u64,
    ) -> Result<(), ContainerdContentError> {
        if offset != ingest.total_size {
            return Err(ContainerdContentError::SizeMismatch {
                expected: ingest.total_size,
                actual: offset,
            });
        }
        let result = self
            .write_message(
                ingest,
                WriteContentRequest {
                    action: WriteAction::Commit.into(),
                    r#ref: ingest.reference.clone(),
                    total: checked_i64(ingest.total_size, "ingest total size")?,
                    expected: ingest.digest.as_str().to_owned(),
                    offset: checked_i64(offset, "ingest offset")?,
                    data: Vec::new(),
                    labels: HashMap::new(),
                },
            )
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(ContainerdContentError::Rpc {
                code: Code::AlreadyExists,
                ..
            }) => {
                let Some(info) = self.blob_info(&ingest.digest).await? else {
                    return Err(ContainerdContentError::Protocol {
                        message: format!(
                            "containerd reported existing content {} but stat missed it",
                            ingest.digest
                        ),
                    });
                };
                if info.size != ingest.total_size {
                    return Err(ContainerdContentError::SizeMismatch {
                        expected: ingest.total_size,
                        actual: info.size,
                    });
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    async fn write_message(
        &self,
        ingest: &ContentIngest,
        message: WriteContentRequest,
    ) -> Result<WriteContentResponse, ContainerdContentError> {
        let messages = stream::iter([message]);
        let mut request = scoped_request(messages)?;
        let lease = metadata_value(&ingest.lease.id, "lease id")?;
        request
            .metadata_mut()
            .insert(CONTAINERD_LEASE_HEADER, lease);
        let mut responses = containerd_client::Client::from(self.channel.clone())
            .content()
            .write(request)
            .await
            .map_err(|status| rpc_error("write content", status))?
            .into_inner();
        first_write_response_after_eof(&mut responses).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentBlobInfo {
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentLease {
    id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentIngest {
    reference: String,
    digest: OciDigest,
    total_size: u64,
    lease: ContentLease,
}

impl ContentIngest {
    #[must_use]
    pub fn new(digest: OciDigest, total_size: u64, lease: ContentLease) -> Self {
        Self {
            reference: format!("ployz-image-upload-{}", nuid::next().to_ascii_lowercase()),
            digest,
            total_size,
            lease,
        }
    }

    #[must_use]
    pub fn digest(&self) -> &OciDigest {
        &self.digest
    }

    #[must_use]
    pub const fn total_size(&self) -> u64 {
        self.total_size
    }

    #[must_use]
    pub fn lease(&self) -> ContentLease {
        self.lease.clone()
    }
}

fn scoped_request<T>(value: T) -> Result<Request<T>, ContainerdContentError> {
    let mut request = Request::new(value);
    request.set_timeout(CONTAINERD_RPC_TIMEOUT);
    request.metadata_mut().insert(
        CONTAINERD_NAMESPACE_HEADER,
        metadata_value(CONTAINERD_NAMESPACE, "containerd namespace")?,
    );
    Ok(request)
}

fn metadata_value(
    value: &str,
    field: &'static str,
) -> Result<MetadataValue<Ascii>, ContainerdContentError> {
    MetadataValue::try_from(value).map_err(|error| ContainerdContentError::Protocol {
        message: format!("invalid {field}: {error}"),
    })
}

fn validate_chunk(
    total_size: u64,
    offset: u64,
    chunk_size: usize,
) -> Result<(), ContainerdContentError> {
    let chunk_size = u64::try_from(chunk_size).map_err(|_| ContainerdContentError::Protocol {
        message: "chunk size exceeds u64".to_owned(),
    })?;
    let Some(end) = offset.checked_add(chunk_size) else {
        return Err(ContainerdContentError::ChunkOutOfRange {
            offset,
            chunk_size,
            total_size,
        });
    };
    if end > total_size {
        return Err(ContainerdContentError::ChunkOutOfRange {
            offset,
            chunk_size,
            total_size,
        });
    }
    Ok(())
}

fn checked_i64(value: u64, field: &'static str) -> Result<i64, ContainerdContentError> {
    i64::try_from(value).map_err(|_| ContainerdContentError::Protocol {
        message: format!("{field} exceeds i64"),
    })
}

fn checked_u64(value: i64, field: &'static str) -> Result<u64, ContainerdContentError> {
    u64::try_from(value).map_err(|_| ContainerdContentError::Protocol {
        message: format!("{field} is negative"),
    })
}

async fn first_write_response_after_eof<S>(
    responses: &mut S,
) -> Result<WriteContentResponse, ContainerdContentError>
where
    S: Stream<Item = Result<WriteContentResponse, Status>> + Unpin,
{
    let mut first_response = None;
    let mut first_error = None;
    while let Some(response) = responses.next().await {
        match response {
            Ok(response) if first_response.is_none() => first_response = Some(response),
            Ok(_) => {}
            Err(status) if first_error.is_none() => first_error = Some(status),
            Err(_) => {}
        }
    }
    if let Some(status) = first_error {
        return Err(rpc_error("write content response", status));
    }
    first_response.ok_or_else(|| ContainerdContentError::Protocol {
        message: "content write returned no status".to_owned(),
    })
}

fn rpc_error(action: &'static str, status: Status) -> ContainerdContentError {
    ContainerdContentError::Rpc {
        action,
        code: status.code(),
        message: status.message().to_owned(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContainerdContentError {
    #[error("Docker containerd socket was not found; searched {searched:?}")]
    SocketNotFound { searched: Vec<PathBuf> },
    #[error("timed out connecting to Docker containerd at {socket}")]
    ConnectTimedOut { socket: PathBuf },
    #[error("failed to connect to Docker containerd at {socket}: {message}")]
    Connect { socket: PathBuf, message: String },
    #[error("containerd {action} failed ({code:?}): {message}")]
    Rpc {
        action: &'static str,
        code: Code,
        message: String,
    },
    #[error("containerd content protocol error: {message}")]
    Protocol { message: String },
    #[error("ingest chunk offset {offset} size {chunk_size} exceeds declared total {total_size}")]
    ChunkOutOfRange {
        offset: u64,
        chunk_size: u64,
        total_size: u64,
    },
    #[error("ingest size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
}

#[cfg(test)]
mod tests {
    use containerd_client::services::v1::{WriteAction, WriteContentResponse};
    use containerd_client::tonic::{Code, Status};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Poll;

    use super::{ContainerdContentError, first_write_response_after_eof, validate_chunk};

    fn write_response(offset: i64) -> WriteContentResponse {
        WriteContentResponse {
            action: WriteAction::Write.into(),
            started_at: None,
            updated_at: None,
            offset,
            total: 0,
            digest: String::new(),
        }
    }

    #[tokio::test]
    async fn write_response_waits_for_stream_eof() {
        let reached_eof = Arc::new(AtomicBool::new(false));
        let stream_reached_eof = Arc::clone(&reached_eof);
        let mut response = Some(Ok(write_response(7)));
        let mut responses = futures_util::stream::poll_fn(move |_| {
            if let Some(response) = response.take() {
                Poll::Ready(Some(response))
            } else {
                stream_reached_eof.store(true, Ordering::SeqCst);
                Poll::Ready(None)
            }
        });

        let response = first_write_response_after_eof(&mut responses)
            .await
            .expect("response stream should succeed");

        assert_eq!(
            (response.offset, reached_eof.load(Ordering::SeqCst)),
            (7, true)
        );
    }

    #[tokio::test]
    async fn write_response_propagates_trailing_stream_error() {
        let mut responses = futures_util::stream::iter([
            Ok(write_response(7)),
            Err(Status::internal("trailing response error")),
        ]);

        let error = first_write_response_after_eof(&mut responses)
            .await
            .expect_err("trailing response error should fail the write");

        assert!(matches!(
            error,
            ContainerdContentError::Rpc {
                action: "write content response",
                code: Code::Internal,
                message,
            } if message == "trailing response error"
        ));
    }

    #[test]
    fn chunk_validation_accepts_the_final_exact_chunk() {
        assert!(validate_chunk(10, 6, 4).is_ok());
    }

    #[test]
    fn chunk_validation_rejects_bytes_past_the_declared_size() {
        assert!(matches!(
            validate_chunk(10, 8, 4),
            Err(ContainerdContentError::ChunkOutOfRange {
                offset: 8,
                chunk_size: 4,
                total_size: 10,
            })
        ));
    }
}

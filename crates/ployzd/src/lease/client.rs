use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::certificate::{LeaseBearerToken, ManagedLeaseAcquireRequest, ManagedLeaseAcquired};
use reqwest::{StatusCode, Url};

use crate::adapters::atomic_file::{
    restrict_secret_file_permissions, write_secret_file_atomically,
};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LeaseWorkerOrigin(Url);

impl LeaseWorkerOrigin {
    pub(crate) fn default_worker() -> Self {
        Self(
            Url::parse(ployz_core::certificate::DEFAULT_LEASE_WORKER_URL)
                .expect("the fixed managed lease worker origin is valid"),
        )
    }

    pub(crate) fn try_new(value: impl AsRef<str>) -> Result<Self, LeaseWorkerOriginError> {
        let value = value.as_ref();
        let url = Url::parse(value).map_err(|_| LeaseWorkerOriginError::Invalid)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(LeaseWorkerOriginError::Invalid);
        }
        Ok(Self(url))
    }

    fn acquire_url(&self) -> Url {
        self.0
            .join("v1/leases")
            .expect("a validated origin accepts the fixed acquisition path")
    }
}

impl fmt::Debug for LeaseWorkerOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LeaseWorkerOriginError {
    #[error("managed lease worker must be an HTTP(S) origin without credentials, query, or path")]
    Invalid,
}

#[derive(Debug, Clone)]
pub(crate) struct LeaseClient {
    origin: LeaseWorkerOrigin,
    http: reqwest::Client,
    max_response_bytes: usize,
}

impl LeaseClient {
    pub(crate) fn new(
        origin: LeaseWorkerOrigin,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, LeaseClientError> {
        if timeout.is_zero() || max_response_bytes == 0 {
            return Err(LeaseClientError::InvalidBounds);
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| LeaseClientError::Transport {
                detail: error.without_url().to_string(),
            })?;
        Ok(Self {
            origin,
            http,
            max_response_bytes,
        })
    }

    pub(crate) async fn acquire(
        &self,
        request: ManagedLeaseAcquireRequest,
    ) -> Result<ManagedLeaseAcquired, LeaseClientError> {
        let response = self
            .http
            .post(self.origin.acquire_url())
            .json(&request)
            .send()
            .await
            .map_err(request_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(match status {
                StatusCode::UNAUTHORIZED => LeaseClientError::Unauthorized,
                StatusCode::NOT_FOUND => LeaseClientError::NotFound,
                _ => LeaseClientError::Http {
                    status: status.as_u16(),
                },
            });
        }
        let body = read_bounded_response(response, self.max_response_bytes).await?;
        serde_json::from_slice(&body).map_err(|error| LeaseClientError::Decode {
            detail: error.to_string(),
        })
    }
}

async fn read_bounded_response(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, LeaseClientError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(request_error)? {
        let Some(total) = body.len().checked_add(chunk.len()) else {
            return Err(LeaseClientError::ResponseTooLarge { limit });
        };
        if total > limit {
            return Err(LeaseClientError::ResponseTooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn request_error(error: reqwest::Error) -> LeaseClientError {
    LeaseClientError::Transport {
        detail: error.without_url().to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeaseClientError {
    #[error("managed lease client bounds must be positive")]
    InvalidBounds,
    #[error("managed lease worker rejected the request credential")]
    Unauthorized,
    #[error("managed lease worker resource was not found")]
    NotFound,
    #[error("managed lease worker returned HTTP status {status}")]
    Http { status: u16 },
    #[error("managed lease worker response exceeds {limit} bytes")]
    ResponseTooLarge { limit: usize },
    #[error("managed lease worker transport failed: {detail}")]
    Transport { detail: String },
    #[error("managed lease worker response was invalid: {detail}")]
    Decode { detail: String },
}

pub(crate) fn load_or_create_token(path: &Path) -> Result<LeaseBearerToken, LeaseTokenFileError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            restrict_secret_file_permissions(path).map_err(|error| LeaseTokenFileError::File {
                path: path.to_path_buf(),
                detail: error.to_string(),
            })?;
            decode_token(path, bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut random = [0_u8; 32];
            getrandom::fill(&mut random).map_err(|error| LeaseTokenFileError::Random {
                detail: error.to_string(),
            })?;
            let encoded = encode_hex(random);
            let token = LeaseBearerToken::try_new(encoded.clone()).map_err(|error| {
                LeaseTokenFileError::Invalid {
                    path: path.to_path_buf(),
                    detail: error.to_string(),
                }
            })?;
            write_secret_file_atomically(path, encoded.as_bytes()).map_err(|error| {
                LeaseTokenFileError::File {
                    path: path.to_path_buf(),
                    detail: error.to_string(),
                }
            })?;
            Ok(token)
        }
        Err(error) => Err(LeaseTokenFileError::File {
            path: path.to_path_buf(),
            detail: error.to_string(),
        }),
    }
}

fn decode_token(path: &Path, bytes: Vec<u8>) -> Result<LeaseBearerToken, LeaseTokenFileError> {
    if bytes.len() > 256 {
        return Err(LeaseTokenFileError::Invalid {
            path: path.to_path_buf(),
            detail: "token file exceeds 256 bytes".to_owned(),
        });
    }
    let value = String::from_utf8(bytes).map_err(|error| LeaseTokenFileError::Invalid {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;
    LeaseBearerToken::try_new(value).map_err(|error| LeaseTokenFileError::Invalid {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })
}

fn encode_hex(bytes: [u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LeaseTokenFileError {
    #[error("managed lease token randomness failed: {detail}")]
    Random { detail: String },
    #[error("managed lease token file {path} failed: {detail}")]
    File { path: PathBuf, detail: String },
    #[error("managed lease token file {path} is invalid: {detail}")]
    Invalid { path: PathBuf, detail: String },
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use ployz_core::certificate::{LeaseBearerToken, ManagedLeaseAcquisitionId, ManagedLeaseName};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn worker_origin_accepts_only_an_http_origin() {
        assert!(LeaseWorkerOrigin::try_new("https://dns.ployz.app").is_ok());
        assert!(LeaseWorkerOrigin::try_new("http://127.0.0.1:8089/").is_ok());
        assert!(LeaseWorkerOrigin::try_new("ftp://dns.ployz.app").is_err());
        assert!(LeaseWorkerOrigin::try_new("https://dns.ployz.app/v1").is_err());
        assert!(LeaseWorkerOrigin::try_new("https://dns.ployz.app?token=secret").is_err());
    }

    #[tokio::test]
    async fn acquisition_uses_the_typed_worker_wire_contract() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("listener address");
        let worker = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("worker request");
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let read = stream.read(&mut chunk).await.expect("request bytes");
                if read == 0 {
                    break;
                }
                let received = chunk
                    .get(..read)
                    .expect("socket reads cannot exceed the destination buffer");
                bytes.extend_from_slice(received);
                if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let header_end = header_end + 4;
                    let headers = String::from_utf8_lossy(
                        bytes
                            .get(..header_end)
                            .expect("the header boundary was found in these bytes"),
                    );
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|value| value.parse::<usize>().ok())
                        })
                        .expect("content length");
                    if bytes.len() >= header_end + length {
                        break;
                    }
                }
            }
            let request = String::from_utf8(bytes).expect("UTF-8 request");
            assert!(request.starts_with("POST /v1/leases HTTP/1.1\r\n"));
            assert!(request.contains("\r\ncontent-type: application/json\r\n"));
            assert!(request.contains(
                r#"{"acquisition_id":"a1","token":"client-token","ipv4":["10.20.30.40"],"ipv6":[]}"#
            ));

            let body = r#"{"lease":{"name":"cluster-one","token":"worker-token","issued_at":"100","expires_at":"200"},"bundle":null}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("response");
        });
        let client = LeaseClient::new(
            LeaseWorkerOrigin::try_new(format!("http://{address}")).expect("origin"),
            Duration::from_secs(2),
            4096,
        )
        .expect("client");

        let acquired = client
            .acquire(ManagedLeaseAcquireRequest {
                acquisition_id: ManagedLeaseAcquisitionId::try_new("a1").expect("acquisition id"),
                token: LeaseBearerToken::try_new("client-token").expect("token"),
                ipv4: vec!["10.20.30.40".parse().expect("IPv4")],
                ipv6: Vec::new(),
            })
            .await
            .expect("acquired lease");

        assert_eq!(
            acquired.lease.name,
            ManagedLeaseName::try_new("cluster-one").expect("lease name")
        );
        worker.await.expect("worker task");
    }

    #[test]
    fn token_file_round_trip_is_stable_and_private() {
        let root = tempfile::tempdir().expect("temporary root");
        let path = root.path().join("api").join("lease-token");

        let first = load_or_create_token(&path).expect("first token");
        let second = load_or_create_token(&path).expect("same token");

        assert_eq!(first, second);
        assert_eq!(
            std::fs::metadata(path)
                .expect("token metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

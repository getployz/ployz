//! Bounded HTTP/JSON requests over an already-connected mesh stream.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, LengthLimitError, Limited};
use hyper::body::Body as _;
use hyper::client::conn::http1;
use hyper::header::{CONNECTION, CONTENT_TYPE, HOST};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use ployz_core::{ApiRefusal, LensCollection, LensSnapshot, lens_route};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncWrite};

pub const DEFAULT_MESH_API_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_MESH_API_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct MeshApiClient {
    request_timeout: Duration,
    max_response_bytes: usize,
}

impl MeshApiClient {
    #[must_use]
    pub const fn new(request_timeout: Duration, max_response_bytes: usize) -> Self {
        Self {
            request_timeout,
            max_response_bytes,
        }
    }

    pub async fn lens<Stream>(
        &self,
        stream: Stream,
        collection: LensCollection,
    ) -> Result<LensSnapshot, MeshApiClientError>
    where
        Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let snapshot = self
            .request_json::<Stream, (), LensSnapshot>(
                stream,
                Method::GET,
                &lens_route(collection),
                None,
            )
            .await?;
        let actual = snapshot_collection(&snapshot);
        if actual != collection {
            return Err(MeshApiClientError::WrongLens {
                expected: collection,
                actual,
            });
        }
        Ok(snapshot)
    }

    /// Sends one bounded HTTP/JSON exchange over an already-connected stream.
    pub async fn request_json<Stream, RequestBody, ResponseBody>(
        &self,
        stream: Stream,
        method: Method,
        route: &str,
        body: Option<&RequestBody>,
    ) -> Result<ResponseBody, MeshApiClientError>
    where
        Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        RequestBody: Serialize + ?Sized,
        ResponseBody: DeserializeOwned,
    {
        match self
            .request_json_with_refusal::<Stream, RequestBody, ResponseBody, ApiRefusal>(
                stream, method, route, body,
            )
            .await?
        {
            JsonReply::Success(reply) => Ok(reply),
            JsonReply::Refused(refusal) => Err(MeshApiClientError::Refused { refusal }),
        }
    }

    /// Sends one bounded exchange while preserving an endpoint-specific refusal union.
    pub async fn request_json_with_refusal<Stream, RequestBody, ResponseBody, Refusal>(
        &self,
        stream: Stream,
        method: Method,
        route: &str,
        body: Option<&RequestBody>,
    ) -> Result<JsonReply<ResponseBody, Refusal>, MeshApiClientError>
    where
        Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        RequestBody: Serialize + ?Sized,
        ResponseBody: DeserializeOwned,
        Refusal: DeserializeOwned,
    {
        tokio::time::timeout(
            self.request_timeout,
            self.request_json_without_deadline(stream, method, route, body),
        )
        .await
        .map_err(|_| MeshApiClientError::TimedOut)?
    }

    async fn request_json_without_deadline<Stream, RequestBody, ResponseBody, Refusal>(
        &self,
        stream: Stream,
        method: Method,
        route: &str,
        body: Option<&RequestBody>,
    ) -> Result<JsonReply<ResponseBody, Refusal>, MeshApiClientError>
    where
        Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        RequestBody: Serialize + ?Sized,
        ResponseBody: DeserializeOwned,
        Refusal: DeserializeOwned,
    {
        let request_body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(MeshApiClientError::EncodeJson)?
            .map_or_else(Bytes::new, Bytes::from);
        let has_body = !request_body.is_empty();
        let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
            .await
            .map_err(MeshApiClientError::Handshake)?;
        let _connection_task = AbortConnectionTask(tokio::spawn(connection));
        let mut builder = Request::builder()
            .method(method)
            .uri(route)
            .header(HOST, "ployz.mesh")
            .header(CONNECTION, "close");
        if has_body {
            builder = builder.header(CONTENT_TYPE, "application/json");
        }
        let request = builder
            .body(Full::new(request_body))
            .map_err(MeshApiClientError::InvalidRequest)?;
        let response = sender
            .send_request(request)
            .await
            .map_err(MeshApiClientError::Request)?;
        let status = response.status();
        validate_json_content_type(response.headers().get(CONTENT_TYPE))?;
        let body = response.into_body();
        if body.size_hint().lower() > self.max_response_bytes as u64 {
            return Err(MeshApiClientError::ResponseTooLarge {
                limit: self.max_response_bytes,
            });
        }
        let collected = Limited::new(body, self.max_response_bytes)
            .collect()
            .await
            .map_err(|error| map_body_error(error, self.max_response_bytes))?;
        let body = collected.to_bytes();

        if !status.is_success() {
            if let Ok(refusal) = serde_json::from_slice::<Refusal>(&body) {
                return Ok(JsonReply::Refused(refusal));
            }
            return match serde_json::from_slice::<ApiRefusal>(&body) {
                Ok(refusal) => Err(MeshApiClientError::Refused { refusal }),
                Err(_) => Err(MeshApiClientError::UnexpectedStatus { status }),
            };
        }

        serde_json::from_slice(&body)
            .map_err(MeshApiClientError::InvalidJson)
            .map(JsonReply::Success)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonReply<Success, Refusal> {
    Success(Success),
    Refused(Refusal),
}

impl Default for MeshApiClient {
    fn default() -> Self {
        Self::new(
            DEFAULT_MESH_API_REQUEST_TIMEOUT,
            MAX_MESH_API_RESPONSE_BYTES,
        )
    }
}

fn validate_json_content_type(
    content_type: Option<&hyper::header::HeaderValue>,
) -> Result<(), MeshApiClientError> {
    let found = content_type
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let is_json = found
        .as_deref()
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"));
    if is_json {
        Ok(())
    } else {
        Err(MeshApiClientError::UnexpectedContentType { found })
    }
}

fn map_body_error(
    error: Box<dyn std::error::Error + Send + Sync>,
    limit: usize,
) -> MeshApiClientError {
    if error.downcast_ref::<LengthLimitError>().is_some() {
        MeshApiClientError::ResponseTooLarge { limit }
    } else {
        MeshApiClientError::Body { source: error }
    }
}

const fn snapshot_collection(snapshot: &LensSnapshot) -> LensCollection {
    match snapshot {
        LensSnapshot::Machines { .. } => LensCollection::Machines,
        LensSnapshot::Services { .. } => LensCollection::Services,
        LensSnapshot::Containers { .. } => LensCollection::Containers,
        LensSnapshot::MachineStatus { .. } => LensCollection::MachineStatus,
        LensSnapshot::Operations { .. } => LensCollection::Operations,
    }
}

struct AbortConnectionTask(tokio::task::JoinHandle<Result<(), hyper::Error>>);

impl Drop for AbortConnectionTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MeshApiClientError {
    #[error("cluster API request exceeded its bounded deadline")]
    TimedOut,
    #[error("cluster API HTTP handshake failed: {0}")]
    Handshake(hyper::Error),
    #[error("cluster API request could not be built: {0}")]
    InvalidRequest(hyper::http::Error),
    #[error("cluster API request body could not be encoded as JSON: {0}")]
    EncodeJson(serde_json::Error),
    #[error("cluster API request failed: {0}")]
    Request(hyper::Error),
    #[error("cluster API response exceeded {limit} bytes")]
    ResponseTooLarge { limit: usize },
    #[error("cluster API response body failed: {source}")]
    Body {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("cluster API response content type is not application/json: {found:?}")]
    UnexpectedContentType { found: Option<String> },
    #[error("cluster API returned HTTP {status} without a typed refusal")]
    UnexpectedStatus { status: StatusCode },
    #[error("cluster API response was not valid JSON: {0}")]
    InvalidJson(serde_json::Error),
    #[error("cluster API returned {actual:?} instead of the requested {expected:?} lens")]
    WrongLens {
        expected: LensCollection,
        actual: LensCollection,
    },
    #[error("cluster API refused the request: {refusal:?}")]
    Refused { refusal: ApiRefusal },
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    #[derive(Debug, PartialEq, Eq, Serialize)]
    struct ExampleRequest {
        name: &'static str,
    }

    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct ExampleReply {
        accepted: bool,
    }

    #[derive(Debug, PartialEq, Eq, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum ExampleRefusal {
        Conflict { repair_command: String },
    }

    #[tokio::test]
    async fn sends_and_decodes_typed_json_over_an_arbitrarily_chunked_stream() {
        let (stream, mut server) = tokio::io::duplex(4096);
        let server_task = tokio::spawn(async move {
            let request = read_request_with_body(&mut server).await;
            assert!(request.starts_with("POST /v2/example HTTP/1.1\r\n"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("content-type: application/json\r\n")
            );
            assert!(request.ends_with("\r\n\r\n{\"name\":\"ares\"}"));

            for chunk in [
                &b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"[..],
                &b"Content-Length: 17\r\nConnection: close\r\n\r\n"[..],
                &b"{\"accepted\":true}"[..],
            ] {
                server.write_all(chunk).await.expect("write response chunk");
                tokio::task::yield_now().await;
            }
        });

        let actual: ExampleReply = MeshApiClient::default()
            .request_json(
                stream,
                Method::POST,
                "/v2/example",
                Some(&ExampleRequest { name: "ares" }),
            )
            .await
            .expect("typed response");
        assert_eq!(actual, ExampleReply { accepted: true });
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn preserves_an_endpoint_specific_refusal_union() {
        let (stream, mut server) = tokio::io::duplex(4096);
        let server_task = tokio::spawn(async move {
            let _request = read_request_with_body(&mut server).await;
            let body = br#"{"kind":"conflict","repair_command":"ployz machine reset"}"#;
            server
                .write_all(
                    format!(
                        "HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write response head");
            server.write_all(body).await.expect("write response body");
        });

        let actual = MeshApiClient::default()
            .request_json_with_refusal::<_, _, ExampleReply, ExampleRefusal>(
                stream,
                Method::POST,
                "/v2/example",
                Some(&ExampleRequest { name: "ares" }),
            )
            .await
            .expect("typed exchange");
        assert_eq!(
            actual,
            JsonReply::Refused(ExampleRefusal::Conflict {
                repair_command: "ployz machine reset".to_owned(),
            })
        );
        server_task.await.expect("server task");
    }

    #[tokio::test]
    async fn requests_the_canonical_lens_route_over_an_arbitrary_stream() {
        let snapshot = LensSnapshot::Services { rows: Vec::new() };
        let body = serde_json::to_vec(&snapshot).expect("snapshot JSON");
        let actual = request_against_response(
            MeshApiClient::default(),
            LensCollection::Services,
            "200 OK",
            Some("application/json; charset=utf-8"),
            &body,
        )
        .await
        .expect("snapshot");
        assert_eq!(actual, snapshot);
    }

    #[tokio::test]
    async fn preserves_typed_refusals() {
        let body = serde_json::to_vec(&ApiRefusal::MissingCluster).expect("refusal JSON");
        assert!(matches!(
            request_against_response(
                MeshApiClient::default(),
                LensCollection::Machines,
                "503 Service Unavailable",
                Some("application/json"),
                &body,
            )
            .await
            .expect_err("typed refusal"),
            MeshApiClientError::Refused {
                refusal: ApiRefusal::MissingCluster
            }
        ));
    }

    #[tokio::test]
    async fn rejects_the_wrong_lens_and_non_json_responses() {
        let body = serde_json::to_vec(&LensSnapshot::Services { rows: Vec::new() })
            .expect("snapshot JSON");
        assert!(matches!(
            request_against_response(
                MeshApiClient::default(),
                LensCollection::Machines,
                "200 OK",
                Some("application/json"),
                &body,
            )
            .await
            .expect_err("wrong lens"),
            MeshApiClientError::WrongLens {
                expected: LensCollection::Machines,
                actual: LensCollection::Services,
            }
        ));
        assert!(matches!(
            request_against_response(
                MeshApiClient::default(),
                LensCollection::Services,
                "200 OK",
                Some("text/plain"),
                &body,
            )
            .await
            .expect_err("wrong content type"),
            MeshApiClientError::UnexpectedContentType { .. }
        ));
    }

    #[tokio::test]
    async fn limits_declared_and_streamed_response_bodies() {
        let client = MeshApiClient::new(Duration::from_secs(1), 8);
        let (stream, mut server) = tokio::io::duplex(4096);
        let server_task = tokio::spawn(async move {
            read_request(&mut server, "/lenses/services").await;
            server
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 9\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write declared oversize response");
        });
        assert!(matches!(
            client
                .lens(stream, LensCollection::Services)
                .await
                .expect_err("declared oversize response"),
            MeshApiClientError::ResponseTooLarge { limit: 8 }
        ));
        server_task.await.expect("server task");

        let streamed = br#"{"collection":"services","rows":[]}"#;
        assert!(matches!(
            request_against_chunked_response(client, LensCollection::Services, streamed)
                .await
                .expect_err("streamed oversize response"),
            MeshApiClientError::ResponseTooLarge { limit: 8 }
        ));
    }

    #[tokio::test]
    async fn one_deadline_bounds_handshake_request_and_body() {
        let client = MeshApiClient::new(Duration::from_millis(10), MAX_MESH_API_RESPONSE_BYTES);
        let (stream, _server) = tokio::io::duplex(4096);
        assert!(matches!(
            client
                .lens(stream, LensCollection::Machines)
                .await
                .expect_err("whole request timeout"),
            MeshApiClientError::TimedOut
        ));

        let (stream, mut server) = tokio::io::duplex(4096);
        let server_task = tokio::spawn(async move {
            read_request(&mut server, "/lenses/machines").await;
            server
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{",
                )
                .await
                .expect("write partial response");
            std::future::pending::<()>().await;
        });
        assert!(matches!(
            client
                .lens(stream, LensCollection::Machines)
                .await
                .expect_err("stalled body timeout"),
            MeshApiClientError::TimedOut
        ));
        server_task.abort();
    }

    async fn request_against_response(
        client: MeshApiClient,
        collection: LensCollection,
        status: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<LensSnapshot, MeshApiClientError> {
        let (stream, mut server) = tokio::io::duplex(MAX_MESH_API_RESPONSE_BYTES + 4096);
        let status = status.to_owned();
        let content_type = content_type.map(str::to_owned);
        let body = body.to_vec();
        let expected_path = lens_route(collection);
        let server_task = tokio::spawn(async move {
            read_request(&mut server, &expected_path).await;
            let content_type = content_type
                .map(|value| format!("Content-Type: {value}\r\n"))
                .unwrap_or_default();
            server
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write response head");
            server.write_all(&body).await.expect("write response body");
        });
        let result = client.lens(stream, collection).await;
        server_task.await.expect("server task");
        result
    }

    async fn request_against_chunked_response(
        client: MeshApiClient,
        collection: LensCollection,
        body: &[u8],
    ) -> Result<LensSnapshot, MeshApiClientError> {
        let (stream, mut server) = tokio::io::duplex(4096);
        let body = body.to_vec();
        let expected_path = lens_route(collection);
        let server_task = tokio::spawn(async move {
            read_request(&mut server, &expected_path).await;
            server
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write response head");
            server
                .write_all(format!("{:x}\r\n", body.len()).as_bytes())
                .await
                .expect("write chunk length");
            server.write_all(&body).await.expect("write chunk body");
            server
                .write_all(b"\r\n0\r\n\r\n")
                .await
                .expect("finish chunks");
        });
        let result = client.lens(stream, collection).await;
        server_task.await.expect("server task");
        result
    }

    async fn read_request<Stream>(stream: &mut Stream, expected_path: &str)
    where
        Stream: tokio::io::AsyncRead + Unpin,
    {
        let mut request = Vec::new();
        loop {
            let mut byte = [0];
            stream.read_exact(&mut byte).await.expect("read request");
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8(request).expect("request text");
        assert!(request.starts_with(&format!("GET {expected_path} HTTP/1.1\r\n")));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("host: ployz.mesh\r\n")
        );
    }

    async fn read_request_with_body<Stream>(stream: &mut Stream) -> String
    where
        Stream: tokio::io::AsyncRead + Unpin,
    {
        let mut request = Vec::new();
        let header_end = loop {
            let mut byte = [0];
            stream.read_exact(&mut byte).await.expect("read request");
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break request.len();
            }
        };
        let headers = String::from_utf8(
            request
                .get(..header_end)
                .expect("header boundary comes from the request length")
                .to_vec(),
        )
        .expect("request headers");
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .expect("content length");
        request.resize(header_end + content_length, 0);
        let body = request
            .get_mut(header_end..)
            .expect("resized request contains the declared body range");
        stream.read_exact(body).await.expect("read request body");
        String::from_utf8(request).expect("request text")
    }
}

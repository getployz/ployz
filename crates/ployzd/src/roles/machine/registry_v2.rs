//! Digest-only Registry V2 reads over the Ployz Native Mesh.

use crate::adapters::containerd_content::ContainerdContentStore;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderName, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use ployz_core::image::OciDigest;
use serde::Deserialize;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
const DISTRIBUTION_API_VERSION: HeaderName =
    HeaderName::from_static("docker-distribution-api-version");
const DOCKER_CONTENT_DIGEST: HeaderName = HeaderName::from_static("docker-content-digest");

#[derive(Clone)]
struct RegistryV2 {
    content: ContainerdContentStore,
    health: Arc<RegistryV2HealthCounters>,
}

pub struct RunningRegistryV2 {
    listen_addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    health: Arc<RegistryV2HealthCounters>,
}

impl RunningRegistryV2 {
    pub async fn start(
        listen_addr: SocketAddr,
        content: ContainerdContentStore,
    ) -> Result<Self, RegistryV2StartError> {
        let listener =
            TcpListener::bind(listen_addr)
                .await
                .map_err(|error| RegistryV2StartError::Bind {
                    listen_addr,
                    message: error.to_string(),
                })?;
        let listen_addr =
            listener
                .local_addr()
                .map_err(|error| RegistryV2StartError::LocalAddress {
                    message: error.to_string(),
                })?;
        let health = Arc::new(RegistryV2HealthCounters::default());
        let registry = RegistryV2 {
            content,
            health: Arc::clone(&health),
        };
        health.bound.store(true, Ordering::Relaxed);
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let task =
            tokio::spawn(async move { run_listener(listener, registry, &mut shutdown_rx).await });
        Ok(Self {
            listen_addr,
            shutdown,
            task,
            health,
        })
    }

    #[must_use]
    pub fn spawn_retrying(listen_addr: SocketAddr, content: ContainerdContentStore) -> Self {
        let health = Arc::new(RegistryV2HealthCounters::default());
        let registry = RegistryV2 {
            content,
            health: Arc::clone(&health),
        };
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let listener = loop {
                match TcpListener::bind(listen_addr).await {
                    Ok(listener) => break listener,
                    Err(_) => {
                        registry
                            .health
                            .bind_failures
                            .fetch_add(1, Ordering::Relaxed);
                        tokio::select! {
                            () = tokio::time::sleep(Duration::from_secs(1)) => {}
                            _ = &mut shutdown_rx => return,
                        }
                    }
                }
            };
            registry.health.bound.store(true, Ordering::Relaxed);
            run_listener(listener, registry, &mut shutdown_rx).await;
        });
        Self {
            listen_addr,
            shutdown,
            task,
            health,
        }
    }

    #[must_use]
    pub const fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    #[must_use]
    pub fn health(&self) -> RegistryV2Health {
        self.health.snapshot()
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run_listener(
    listener: TcpListener,
    registry: RegistryV2,
    shutdown_rx: &mut oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    registry.health.accepted.fetch_add(1, Ordering::Relaxed);
                    let registry = registry.clone();
                    connections.spawn(async move {
                        let health = Arc::clone(&registry.health);
                        let registry_for_service = registry.clone();
                        let service = service_fn(move |request| {
                            let registry = registry_for_service.clone();
                            async move { Ok::<_, Infallible>(registry.handle(request).await) }
                        });
                        let connection = http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service);
                        if tokio::time::timeout(CONNECTION_TIMEOUT, connection).await.is_err() {
                            health.connection_timeouts.fetch_add(1, Ordering::Relaxed);
                        }
                    });
                }
                Err(_) => {
                    registry.health.accept_failures.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                if result.is_err() {
                    registry.health.connection_failures.fetch_add(1, Ordering::Relaxed);
                }
            }
            _ = &mut *shutdown_rx => break,
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

impl RegistryV2 {
    async fn handle(&self, request: Request<Incoming>) -> Response<Full<Bytes>> {
        self.health.requests.fetch_add(1, Ordering::Relaxed);
        let method = request.method();
        if method != Method::GET && method != Method::HEAD {
            return empty_response(StatusCode::METHOD_NOT_ALLOWED);
        }
        let route = match parse_route(request.uri().path()) {
            Ok(route) => route,
            Err(status) => return empty_response(status),
        };
        match route {
            RegistryRoute::Ping => registry_response(StatusCode::OK, None, None, None),
            RegistryRoute::Read { kind, digest } => self.read_content(method, kind, &digest).await,
        }
    }

    async fn read_content(
        &self,
        method: &Method,
        kind: RegistryObjectKind,
        digest: &OciDigest,
    ) -> Response<Full<Bytes>> {
        let info = match self.content.blob_info(digest).await {
            Ok(Some(info)) => info,
            Ok(None) => return empty_response(StatusCode::NOT_FOUND),
            Err(_) => {
                self.health.storage_failures.fetch_add(1, Ordering::Relaxed);
                return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
            }
        };
        if method == Method::HEAD {
            return registry_response(
                StatusCode::OK,
                Some(info.size),
                Some(kind.default_media_type()),
                Some(digest),
            );
        }
        let bytes = match self.content.read_blob(digest).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return empty_response(StatusCode::NOT_FOUND),
            Err(_) => {
                self.health.storage_failures.fetch_add(1, Ordering::Relaxed);
                return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
            }
        };
        let media_type = match kind {
            RegistryObjectKind::Blob => RegistryObjectKind::Blob.default_media_type(),
            RegistryObjectKind::Manifest => manifest_media_type(&bytes)
                .unwrap_or_else(|| RegistryObjectKind::Manifest.default_media_type()),
        };
        registry_response(
            StatusCode::OK,
            Some(u64::try_from(bytes.len()).unwrap_or(info.size)),
            Some(media_type),
            Some(digest),
        )
        .map(|_| Full::new(Bytes::from(bytes)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistryObjectKind {
    Blob,
    Manifest,
}

impl RegistryObjectKind {
    const fn default_media_type(self) -> &'static str {
        match self {
            Self::Blob => "application/octet-stream",
            Self::Manifest => "application/vnd.oci.image.manifest.v1+json",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RegistryRoute {
    Ping,
    Read {
        kind: RegistryObjectKind,
        digest: OciDigest,
    },
}

fn parse_route(path: &str) -> Result<RegistryRoute, StatusCode> {
    let Some(path) = path.strip_prefix("/v2/") else {
        return Err(StatusCode::NOT_FOUND);
    };
    if path.is_empty() {
        return Ok(RegistryRoute::Ping);
    }
    let (repository, kind, digest) = if let Some((repository, digest)) = path.rsplit_once("/blobs/")
    {
        (repository, RegistryObjectKind::Blob, digest)
    } else if let Some((repository, digest)) = path.rsplit_once("/manifests/") {
        (repository, RegistryObjectKind::Manifest, digest)
    } else {
        return Err(StatusCode::NOT_FOUND);
    };
    if !valid_repository(repository) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let digest = OciDigest::try_new(digest.to_owned()).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(RegistryRoute::Read { kind, digest })
}

fn valid_repository(repository: &str) -> bool {
    !repository.is_empty()
        && repository.split('/').all(|component| {
            component
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                && component
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && component.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'-')
                })
        })
}

#[derive(Deserialize)]
struct ManifestMediaType {
    #[serde(rename = "mediaType")]
    media_type: String,
}

fn manifest_media_type(bytes: &[u8]) -> Option<&str> {
    let manifest = serde_json::from_slice::<ManifestMediaType>(bytes).ok()?;
    match manifest.media_type.as_str() {
        "application/vnd.oci.image.manifest.v1+json" => {
            Some("application/vnd.oci.image.manifest.v1+json")
        }
        "application/vnd.docker.distribution.manifest.v2+json" => {
            Some("application/vnd.docker.distribution.manifest.v2+json")
        }
        _ => None,
    }
}

fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    registry_response(status, Some(0), None, None)
}

fn registry_response(
    status: StatusCode,
    size: Option<u64>,
    media_type: Option<&str>,
    digest: Option<&OciDigest>,
) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        DISTRIBUTION_API_VERSION,
        HeaderValue::from_static("registry/2.0"),
    );
    if let Some(size) = size {
        response
            .headers_mut()
            .insert(CONTENT_LENGTH, HeaderValue::from(size));
    }
    if let Some(media_type) = media_type
        && let Ok(value) = HeaderValue::from_str(media_type)
    {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    if let Some(digest) = digest
        && let Ok(value) = HeaderValue::from_str(digest.as_str())
    {
        response
            .headers_mut()
            .insert(DOCKER_CONTENT_DIGEST, value.clone());
        response.headers_mut().insert(hyper::header::ETAG, value);
    }
    response
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryV2Health {
    pub bound: bool,
    pub bind_failures: u64,
    pub accepted: u64,
    pub requests: u64,
    pub accept_failures: u64,
    pub connection_failures: u64,
    pub connection_timeouts: u64,
    pub storage_failures: u64,
}

#[derive(Default)]
struct RegistryV2HealthCounters {
    bound: AtomicBool,
    bind_failures: AtomicU64,
    accepted: AtomicU64,
    requests: AtomicU64,
    accept_failures: AtomicU64,
    connection_failures: AtomicU64,
    connection_timeouts: AtomicU64,
    storage_failures: AtomicU64,
}

impl RegistryV2HealthCounters {
    fn snapshot(&self) -> RegistryV2Health {
        RegistryV2Health {
            bound: self.bound.load(Ordering::Relaxed),
            bind_failures: self.bind_failures.load(Ordering::Relaxed),
            accepted: self.accepted.load(Ordering::Relaxed),
            requests: self.requests.load(Ordering::Relaxed),
            accept_failures: self.accept_failures.load(Ordering::Relaxed),
            connection_failures: self.connection_failures.load(Ordering::Relaxed),
            connection_timeouts: self.connection_timeouts.load(Ordering::Relaxed),
            storage_failures: self.storage_failures.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryV2StartError {
    #[error("failed to bind image registry at {listen_addr}: {message}")]
    Bind {
        listen_addr: SocketAddr,
        message: String,
    },
    #[error("failed to read image registry local address: {message}")]
    LocalAddress { message: String },
}

#[cfg(test)]
mod tests {
    use super::{RegistryObjectKind, RegistryRoute, parse_route};
    use hyper::StatusCode;

    #[test]
    fn route_accepts_nested_repository_and_digest() {
        let digest = format!("sha256:{}", "a".repeat(64));

        let route = parse_route(&format!("/v2/team/api/manifests/{digest}"));

        assert!(matches!(
            route,
            Ok(RegistryRoute::Read {
                kind: RegistryObjectKind::Manifest,
                ..
            })
        ));
    }

    #[test]
    fn route_rejects_mutable_manifest_tags() {
        assert_eq!(
            parse_route("/v2/team/api/manifests/latest"),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn route_rejects_repository_traversal() {
        let digest = format!("sha256:{}", "a".repeat(64));

        assert_eq!(
            parse_route(&format!("/v2/team/../api/blobs/{digest}")),
            Err(StatusCode::BAD_REQUEST)
        );
    }
}

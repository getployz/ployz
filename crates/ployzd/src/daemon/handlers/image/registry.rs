//! Ephemeral OCI Distribution receiver used by image push/distribute orchestration.
//!
//! The receiver is daemon-owned and session-gated. It stores transfer cache for
//! later image placement orchestration, but it is not durable deploy truth.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::StreamExt;
use ployz_runtime_api::RuntimeHandle;
use ployz_types::model::MachineId;
use serde::Serialize;
use sha2::{Digest as ShaDigest, Sha256};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) const REGISTRY_OPERATION_HEADER: &str = "x-ployz-image-operation";
pub(crate) const REGISTRY_SOURCE_MACHINE_HEADER: &str = "x-ployz-source-machine";
pub(crate) const REGISTRY_SESSION_HEADER: &str = "x-ployz-image-session";

const API_VERSION_HEADER: &str = "docker-distribution-api-version";
const API_VERSION: &str = "registry/2.0";
const CONTENT_DIGEST_HEADER: &str = "docker-content-digest";
const UPLOAD_UUID_HEADER: &str = "docker-upload-uuid";
const RANGE_HEADER: &str = "range";
const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MANIFEST_CONTENT_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const SESSION_TTL_SECS: u64 = 60 * 60;

pub(crate) struct ImageRegistryListenerHandle {
    #[cfg(test)]
    bind_addr: SocketAddr,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl ImageRegistryListenerHandle {
    #[must_use]
    pub(crate) fn noop() -> Self {
        Self {
            #[cfg(test)]
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            cancel: CancellationToken::new(),
            task: tokio::spawn(async {}),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn bind_addr(&self) -> SocketAddr {
        self.bind_addr
    }

    pub(crate) async fn shutdown(self) {
        self.cancel.cancel();
        if let Err(error) = self.task.await {
            if !error.is_cancelled() {
                tracing::warn!(?error, "image registry listener join failed");
            }
        }
    }
}

#[async_trait::async_trait]
impl RuntimeHandle for ImageRegistryListenerHandle {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String> {
        ImageRegistryListenerHandle::shutdown(*self).await;
        Ok(())
    }
}

pub(crate) async fn serve(
    bind_addr: SocketAddr,
    registry: ImageRegistry,
) -> Result<ImageRegistryListenerHandle, String> {
    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|error| format!("bind image registry listener {bind_addr}: {error}"))?;
    let bind_addr = listener
        .local_addr()
        .map_err(|error| format!("read image registry listener address: {error}"))?;
    tracing::info!(listen = %bind_addr, "image registry listener running");
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, registry.router())
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
        {
            tracing::warn!(%error, "image registry listener failed");
        }
    });
    Ok(ImageRegistryListenerHandle {
        #[cfg(test)]
        bind_addr,
        cancel,
        task,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct ImageRegistry {
    root: PathBuf,
    sessions: Arc<RwLock<BTreeMap<String, ImageRegistrySession>>>,
    uploads: Arc<RwLock<BTreeMap<String, UploadState>>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ImageRegistrySession {
    pub(crate) operation_id: String,
    pub(crate) source_machine: MachineId,
    pub(crate) repository: String,
    pub(crate) token: String,
    pub(crate) expires_at_unix_secs: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct UploadStarted {
    pub(crate) uuid: String,
    pub(crate) location: String,
}

#[derive(Debug, Clone)]
pub(crate) struct BlobStored {
    pub(crate) digest: String,
    pub(crate) bytes: u64,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct ManifestStored {
    pub(crate) digest: String,
    pub(crate) bytes: u64,
    pub(crate) media_type: String,
}

#[derive(Debug, Clone)]
struct UploadState {
    repository: String,
    operation_id: String,
    source_machine: MachineId,
    token: String,
    path: PathBuf,
    bytes: u64,
    failed: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ImageRegistryError {
    #[error("registry session header '{name}' is missing")]
    MissingHeader { name: &'static str },
    #[error("registry session header '{name}' was not valid UTF-8")]
    InvalidHeader { name: &'static str },
    #[error("registry session is not active")]
    Unauthorized,
    #[error("repository path is invalid: {value}")]
    InvalidRepository { value: String },
    #[error("manifest reference is invalid: {value}")]
    InvalidReference { value: String },
    #[error("blob digest is invalid: {value}")]
    InvalidDigest { value: String },
    #[error("blob '{digest}' was not found")]
    BlobUnknown { digest: String },
    #[error("manifest '{repository}:{reference}' was not found")]
    ManifestUnknown {
        repository: String,
        reference: String,
    },
    #[error("upload '{uuid}' was not found")]
    UploadNotFound { uuid: String },
    #[error("upload '{uuid}' does not belong to this session")]
    UploadSessionMismatch { uuid: String },
    #[error("expected digest {expected}, calculated {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("{operation}: {message}")]
    Io {
        operation: &'static str,
        message: String,
    },
    #[error("request body failed: {message}")]
    Body { message: String },
    #[error("unsupported registry path '{path}'")]
    UnsupportedPath { path: String },
    #[error("unsupported registry method {method} for '{path}'")]
    UnsupportedMethod { method: Method, path: String },
}

impl ImageRegistry {
    #[must_use]
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            sessions: Arc::new(RwLock::new(BTreeMap::new())),
            uploads: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    #[must_use]
    pub(crate) fn router(self) -> Router {
        Router::new()
            .route("/v2/", any(registry_root))
            .route("/v2/{*path}", any(registry_entry))
            .with_state(self)
    }

    pub(crate) async fn register_session(
        &self,
        operation_id: impl Into<String>,
        source_machine: MachineId,
        repository: impl Into<String>,
    ) -> ImageRegistrySession {
        self.cleanup_expired_sessions().await;
        let session = ImageRegistrySession {
            operation_id: operation_id.into(),
            source_machine,
            repository: repository.into(),
            token: Uuid::new_v4().to_string(),
            expires_at_unix_secs: current_unix_secs().saturating_add(SESSION_TTL_SECS),
        };
        self.sessions
            .write()
            .await
            .insert(session.token.clone(), session.clone());
        session
    }

    pub(crate) async fn revoke_session(&self, token: &str) -> Result<(), ImageRegistryError> {
        self.sessions.write().await.remove(token);
        let upload_ids = self
            .uploads
            .read()
            .await
            .iter()
            .filter_map(|(uuid, upload)| (upload.token == token).then(|| uuid.clone()))
            .collect::<Vec<_>>();
        for uuid in upload_ids {
            if let Some(upload) = self.uploads.write().await.remove(&uuid) {
                remove_file_if_exists(&upload.path).await?;
                remove_file_if_exists(&self.failed_upload_path(&uuid)).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn revoke_all_sessions(&self) -> Result<(), ImageRegistryError> {
        let tokens = self
            .sessions
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for token in tokens {
            self.revoke_session(&token).await?;
        }
        Ok(())
    }

    async fn cleanup_expired_sessions(&self) {
        let now = current_unix_secs();
        let expired_tokens = self
            .sessions
            .read()
            .await
            .iter()
            .filter_map(|(token, session)| {
                (session.expires_at_unix_secs <= now).then(|| token.clone())
            })
            .collect::<Vec<_>>();
        for token in expired_tokens {
            if let Err(error) = self.revoke_session(&token).await {
                tracing::warn!(%error, "cleanup expired image registry session");
            }
        }
    }

    async fn validate_session(
        &self,
        headers: &HeaderMap,
        repository: &str,
    ) -> Result<ImageRegistrySession, ImageRegistryError> {
        let token = header_value(headers, REGISTRY_SESSION_HEADER)?;
        let operation_id = header_value(headers, REGISTRY_OPERATION_HEADER)?;
        let source_machine = header_value(headers, REGISTRY_SOURCE_MACHINE_HEADER)?;
        let session = {
            let sessions = self.sessions.read().await;
            let Some(session) = sessions.get(token) else {
                return Err(ImageRegistryError::Unauthorized);
            };
            session.clone()
        };
        if session.expires_at_unix_secs <= current_unix_secs() {
            self.revoke_session(token).await?;
            return Err(ImageRegistryError::Unauthorized);
        }
        if session.operation_id != operation_id
            || session.source_machine.0 != source_machine
            || session.repository != repository
        {
            return Err(ImageRegistryError::Unauthorized);
        }
        Ok(session)
    }

    async fn start_upload(
        &self,
        repository: &str,
        headers: &HeaderMap,
    ) -> Result<UploadStarted, ImageRegistryError> {
        validate_repository(repository)?;
        let session = self.validate_session(headers, repository).await?;
        let uuid = Uuid::new_v4().to_string();
        let upload_path = self.upload_path(&uuid);
        ensure_parent(&upload_path).await?;
        File::create(&upload_path)
            .await
            .map_err(|error| ImageRegistryError::io("create registry upload file", error))?;
        let state = UploadState {
            repository: repository.to_string(),
            operation_id: session.operation_id,
            source_machine: session.source_machine,
            token: session.token,
            path: upload_path,
            bytes: 0,
            failed: false,
        };
        self.uploads.write().await.insert(uuid.clone(), state);
        Ok(UploadStarted {
            uuid: uuid.clone(),
            location: upload_location(repository, &uuid),
        })
    }

    async fn append_upload(
        &self,
        repository: &str,
        uuid: &str,
        headers: &HeaderMap,
        body: Body,
    ) -> Result<u64, ImageRegistryError> {
        validate_repository(repository)?;
        let session = self.validate_session(headers, repository).await?;
        let mut upload = self.upload_for_session(uuid, repository, &session).await?;
        let appended = append_body_to_file(&upload.path, body).await?;
        upload.bytes = upload.bytes.saturating_add(appended);
        self.uploads
            .write()
            .await
            .insert(uuid.to_string(), upload.clone());
        Ok(upload.bytes)
    }

    async fn finish_upload(
        &self,
        repository: &str,
        uuid: &str,
        digest: &str,
        headers: &HeaderMap,
        body: Body,
    ) -> Result<BlobStored, ImageRegistryError> {
        validate_repository(repository)?;
        let expected = validate_sha256_digest(digest)?;
        let session = self.validate_session(headers, repository).await?;
        let mut upload = self.upload_for_session(uuid, repository, &session).await?;
        let appended = append_body_to_file(&upload.path, body).await?;
        upload.bytes = upload.bytes.saturating_add(appended);

        let actual = file_sha256_digest(&upload.path).await?;
        if actual != expected {
            let failed_path = self.failed_upload_path(uuid);
            remove_file_if_exists(&failed_path).await?;
            fs::rename(&upload.path, &failed_path)
                .await
                .map_err(|error| {
                    ImageRegistryError::io("preserve failed registry upload", error)
                })?;
            upload.path = failed_path;
            upload.failed = true;
            self.uploads
                .write()
                .await
                .insert(uuid.to_string(), upload.clone());
            return Err(ImageRegistryError::DigestMismatch { expected, actual });
        }

        let blob_path = self.blob_path(&actual)?;
        ensure_parent(&blob_path).await?;
        if fs::metadata(&blob_path).await.is_ok() {
            remove_file_if_exists(&upload.path).await?;
        } else {
            fs::rename(&upload.path, &blob_path)
                .await
                .map_err(|error| ImageRegistryError::io("commit registry blob", error))?;
        }
        self.uploads.write().await.remove(uuid);
        Ok(BlobStored {
            digest: actual,
            bytes: upload.bytes,
            path: blob_path,
        })
    }

    async fn put_monolithic_blob(
        &self,
        repository: &str,
        digest: &str,
        headers: &HeaderMap,
        body: Body,
    ) -> Result<BlobStored, ImageRegistryError> {
        let started = self.start_upload(repository, headers).await?;
        self.finish_upload(repository, &started.uuid, digest, headers, body)
            .await
    }

    async fn has_blob(&self, digest: &str) -> Result<Option<BlobStored>, ImageRegistryError> {
        let digest = validate_sha256_digest(digest)?;
        let path = self.blob_path(&digest)?;
        match fs::metadata(&path).await {
            Ok(metadata) => Ok(Some(BlobStored {
                digest,
                bytes: metadata.len(),
                path,
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ImageRegistryError::io("stat registry blob", error)),
        }
    }

    async fn get_blob(&self, digest: &str) -> Result<Option<Response>, ImageRegistryError> {
        let Some(blob) = self.has_blob(digest).await? else {
            return Ok(None);
        };
        let file = File::open(&blob.path)
            .await
            .map_err(|error| ImageRegistryError::io("open registry blob", error))?;
        let body = Body::from_stream(ReaderStream::new(file));
        let mut headers = base_headers();
        insert_header(
            &mut headers,
            CONTENT_LENGTH.as_str(),
            &blob.bytes.to_string(),
        )?;
        insert_header(&mut headers, CONTENT_DIGEST_HEADER, &blob.digest)?;
        Ok(Some((StatusCode::OK, headers, body).into_response()))
    }

    async fn put_manifest(
        &self,
        repository: &str,
        reference: &str,
        headers: &HeaderMap,
        body: Body,
    ) -> Result<ManifestStored, ImageRegistryError> {
        validate_repository(repository)?;
        validate_reference(reference)?;
        let _session = self.validate_session(headers, repository).await?;
        let media_type = manifest_media_type(headers)?;
        let body = read_limited_body(body).await?;
        let digest = sha256_digest(&body);
        let path = self.manifest_path(repository, reference)?;
        ensure_parent(&path).await?;
        fs::write(&path, &body)
            .await
            .map_err(|error| ImageRegistryError::io("write registry manifest", error))?;
        fs::write(
            self.manifest_media_type_path(repository, reference)?,
            media_type.as_bytes(),
        )
        .await
        .map_err(|error| ImageRegistryError::io("write registry manifest media type", error))?;
        Ok(ManifestStored {
            digest,
            bytes: body.len() as u64,
            media_type,
        })
    }

    async fn get_manifest(
        &self,
        repository: &str,
        reference: &str,
    ) -> Result<Option<Response>, ImageRegistryError> {
        validate_repository(repository)?;
        validate_reference(reference)?;
        let path = self.manifest_path(repository, reference)?;
        let body = match fs::read(&path).await {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ImageRegistryError::io("read registry manifest", error)),
        };
        let media_type = self
            .read_manifest_media_type(repository, reference)
            .await?
            .unwrap_or_else(|| DEFAULT_MANIFEST_CONTENT_TYPE.into());
        let digest = sha256_digest(&body);
        let mut headers = base_headers();
        insert_header(
            &mut headers,
            CONTENT_LENGTH.as_str(),
            &body.len().to_string(),
        )?;
        insert_header(&mut headers, CONTENT_DIGEST_HEADER, &digest)?;
        insert_header(&mut headers, CONTENT_TYPE.as_str(), &media_type)?;
        Ok(Some(
            (StatusCode::OK, headers, Body::from(body)).into_response(),
        ))
    }

    fn upload_path(&self, uuid: &str) -> PathBuf {
        self.root.join("uploads").join(uuid)
    }

    fn failed_upload_path(&self, uuid: &str) -> PathBuf {
        self.root.join("uploads").join(format!("{uuid}.failed"))
    }

    fn blob_path(&self, digest: &str) -> Result<PathBuf, ImageRegistryError> {
        let encoded =
            digest
                .strip_prefix("sha256:")
                .ok_or_else(|| ImageRegistryError::InvalidDigest {
                    value: digest.to_string(),
                })?;
        let (prefix, suffix) = encoded.split_at(2);
        Ok(self
            .root
            .join("blobs")
            .join("sha256")
            .join(prefix)
            .join(suffix))
    }

    fn manifest_path(
        &self,
        repository: &str,
        reference: &str,
    ) -> Result<PathBuf, ImageRegistryError> {
        let mut path = self.root.join("manifests");
        for segment in repository.split('/') {
            path = path.join(segment);
        }
        Ok(path.join(format!("{reference}.json")))
    }

    fn manifest_media_type_path(
        &self,
        repository: &str,
        reference: &str,
    ) -> Result<PathBuf, ImageRegistryError> {
        Ok(self
            .manifest_path(repository, reference)?
            .with_extension("media_type"))
    }

    async fn read_manifest_media_type(
        &self,
        repository: &str,
        reference: &str,
    ) -> Result<Option<String>, ImageRegistryError> {
        let path = self.manifest_media_type_path(repository, reference)?;
        match fs::read_to_string(&path).await {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ImageRegistryError::io(
                "read registry manifest media type",
                error,
            )),
        }
    }

    async fn upload_for_session(
        &self,
        uuid: &str,
        repository: &str,
        session: &ImageRegistrySession,
    ) -> Result<UploadState, ImageRegistryError> {
        let uploads = self.uploads.read().await;
        let Some(upload) = uploads.get(uuid) else {
            return Err(ImageRegistryError::UploadNotFound {
                uuid: uuid.to_string(),
            });
        };
        if upload.repository != repository
            || upload.operation_id != session.operation_id
            || upload.source_machine != session.source_machine
            || upload.token != session.token
            || upload.failed
        {
            return Err(ImageRegistryError::UploadSessionMismatch {
                uuid: uuid.to_string(),
            });
        }
        Ok(upload.clone())
    }
}

impl ImageRegistryError {
    fn io(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Io {
            operation,
            message: error.to_string(),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::MissingHeader { .. }
            | Self::InvalidHeader { .. }
            | Self::Unauthorized
            | Self::UploadSessionMismatch { .. } => StatusCode::UNAUTHORIZED,
            Self::UploadNotFound { .. }
            | Self::BlobUnknown { .. }
            | Self::ManifestUnknown { .. }
            | Self::UnsupportedPath { .. } => StatusCode::NOT_FOUND,
            Self::InvalidRepository { .. }
            | Self::InvalidReference { .. }
            | Self::InvalidDigest { .. }
            | Self::DigestMismatch { .. }
            | Self::Body { .. } => StatusCode::BAD_REQUEST,
            Self::UnsupportedMethod { .. } => StatusCode::METHOD_NOT_ALLOWED,
            Self::Io { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn registry_code(&self) -> &'static str {
        match self {
            Self::MissingHeader { .. }
            | Self::InvalidHeader { .. }
            | Self::Unauthorized
            | Self::UploadSessionMismatch { .. } => "UNAUTHORIZED",
            Self::UploadNotFound { .. } => "BLOB_UPLOAD_UNKNOWN",
            Self::BlobUnknown { .. } => "BLOB_UNKNOWN",
            Self::ManifestUnknown { .. } => "MANIFEST_UNKNOWN",
            Self::InvalidDigest { .. } | Self::DigestMismatch { .. } => "DIGEST_INVALID",
            Self::InvalidRepository { .. } => "NAME_INVALID",
            Self::InvalidReference { .. } => "TAG_INVALID",
            Self::UnsupportedPath { .. } => "NAME_UNKNOWN",
            Self::UnsupportedMethod { .. } => "UNSUPPORTED",
            Self::Body { .. } => "BLOB_UPLOAD_INVALID",
            Self::Io { .. } => "UNKNOWN",
        }
    }
}

impl IntoResponse for ImageRegistryError {
    fn into_response(self) -> Response {
        let mut headers = base_headers();
        insert_header(
            &mut headers,
            CONTENT_TYPE.as_str(),
            "application/json; charset=utf-8",
        )
        .ok();
        let body = RegistryErrorBody {
            errors: vec![RegistryErrorEntry {
                code: self.registry_code(),
                message: self.to_string(),
            }],
        };
        let body = serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"errors\":[]}".to_vec());
        (self.status(), headers, Body::from(body)).into_response()
    }
}

#[derive(Serialize)]
struct RegistryErrorBody {
    errors: Vec<RegistryErrorEntry>,
}

#[derive(Serialize)]
struct RegistryErrorEntry {
    code: &'static str,
    message: String,
}

async fn registry_root(method: Method) -> Response {
    if matches!(method, Method::GET | Method::HEAD) {
        return (StatusCode::OK, base_headers()).into_response();
    }
    ImageRegistryError::UnsupportedMethod {
        method,
        path: "/v2/".into(),
    }
    .into_response()
}

async fn registry_entry(
    State(registry): State<ImageRegistry>,
    AxumPath(path): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response {
    match route_registry_request(&registry, &path, query, &headers, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn route_registry_request(
    registry: &ImageRegistry,
    path: &str,
    query: BTreeMap<String, String>,
    headers: &HeaderMap,
    request: Request<Body>,
) -> Result<Response, ImageRegistryError> {
    let method = request.method().clone();
    let body = request.into_body();
    match method {
        Method::POST => {
            let Some(repository) = strip_blob_upload_start(path) else {
                return Err(ImageRegistryError::UnsupportedPath { path: path.into() });
            };
            if let Some(digest) = query.get("digest") {
                let stored = registry
                    .put_monolithic_blob(repository, digest, headers, body)
                    .await?;
                return Ok(blob_committed_response(repository, stored));
            }
            let upload = registry.start_upload(repository, headers).await?;
            Ok(upload_started_response(upload))
        }
        Method::PATCH => {
            let Some((repository, uuid)) = split_upload_path(path) else {
                return Err(ImageRegistryError::UnsupportedPath { path: path.into() });
            };
            let bytes = registry
                .append_upload(repository, uuid, headers, body)
                .await?;
            Ok(upload_progress_response(repository, uuid, bytes))
        }
        Method::PUT => {
            if let Some((repository, uuid)) = split_upload_path(path) {
                let digest =
                    query
                        .get("digest")
                        .ok_or_else(|| ImageRegistryError::InvalidDigest {
                            value: "missing digest query parameter".into(),
                        })?;
                let stored = registry
                    .finish_upload(repository, uuid, digest, headers, body)
                    .await?;
                return Ok(blob_committed_response(repository, stored));
            }
            let Some((repository, reference)) = split_manifest_path(path) else {
                return Err(ImageRegistryError::UnsupportedPath { path: path.into() });
            };
            let manifest = registry
                .put_manifest(repository, reference, headers, body)
                .await?;
            Ok(manifest_stored_response(repository, reference, manifest))
        }
        Method::GET => {
            if let Some((repository, digest)) = split_blob_path(path) {
                let _session = registry.validate_session(headers, repository).await?;
                return registry.get_blob(digest).await?.ok_or_else(|| {
                    ImageRegistryError::BlobUnknown {
                        digest: digest.to_string(),
                    }
                });
            }
            let Some((repository, reference)) = split_manifest_path(path) else {
                return Err(ImageRegistryError::UnsupportedPath { path: path.into() });
            };
            let _session = registry.validate_session(headers, repository).await?;
            registry
                .get_manifest(repository, reference)
                .await?
                .ok_or_else(|| ImageRegistryError::ManifestUnknown {
                    repository: repository.to_string(),
                    reference: reference.to_string(),
                })
        }
        Method::HEAD => {
            if let Some((repository, digest)) = split_blob_path(path) {
                let _session = registry.validate_session(headers, repository).await?;
                let Some(blob) = registry.has_blob(digest).await? else {
                    return Err(ImageRegistryError::BlobUnknown {
                        digest: digest.to_string(),
                    });
                };
                let mut headers = base_headers();
                insert_header(
                    &mut headers,
                    CONTENT_LENGTH.as_str(),
                    &blob.bytes.to_string(),
                )?;
                insert_header(&mut headers, CONTENT_DIGEST_HEADER, &blob.digest)?;
                return Ok((StatusCode::OK, headers).into_response());
            }
            let Some((repository, reference)) = split_manifest_path(path) else {
                return Err(ImageRegistryError::UnsupportedPath { path: path.into() });
            };
            let _session = registry.validate_session(headers, repository).await?;
            let Some(response) = registry.get_manifest(repository, reference).await? else {
                return Err(ImageRegistryError::ManifestUnknown {
                    repository: repository.to_string(),
                    reference: reference.to_string(),
                });
            };
            let (parts, _) = response.into_parts();
            Ok(Response::from_parts(parts, Body::empty()))
        }
        _ => Err(ImageRegistryError::UnsupportedMethod {
            method,
            path: path.into(),
        }),
    }
}

async fn append_body_to_file(path: &Path, body: Body) -> Result<u64, ImageRegistryError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|error| ImageRegistryError::io("open registry upload file", error))?;
    let mut stream = body.into_data_stream();
    let mut written = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ImageRegistryError::Body {
            message: error.to_string(),
        })?;
        file.write_all(&chunk)
            .await
            .map_err(|error| ImageRegistryError::io("write registry upload chunk", error))?;
        written = written.saturating_add(chunk.len() as u64);
    }
    file.flush()
        .await
        .map_err(|error| ImageRegistryError::io("flush registry upload", error))?;
    Ok(written)
}

async fn file_sha256_digest(path: &Path) -> Result<String, ImageRegistryError> {
    let mut file = File::open(path)
        .await
        .map_err(|error| ImageRegistryError::io("open registry upload for digest", error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| ImageRegistryError::io("read registry upload for digest", error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format_digest(hasher.finalize()))
}

async fn read_limited_body(body: Body) -> Result<Bytes, ImageRegistryError> {
    axum::body::to_bytes(body, MAX_MANIFEST_BYTES)
        .await
        .map_err(|error| ImageRegistryError::Body {
            message: error.to_string(),
        })
}

async fn ensure_parent(path: &Path) -> Result<(), ImageRegistryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| ImageRegistryError::io("create registry directory", error))?;
    }
    Ok(())
}

async fn remove_file_if_exists(path: &Path) -> Result<(), ImageRegistryError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ImageRegistryError::io("remove registry upload", error)),
    }
}

fn strip_blob_upload_start(path: &str) -> Option<&str> {
    path.strip_suffix("/blobs/uploads/")
        .or_else(|| path.strip_suffix("/blobs/uploads"))
        .filter(|repository| !repository.is_empty())
}

fn split_upload_path(path: &str) -> Option<(&str, &str)> {
    let (repository, uuid) = path.rsplit_once("/blobs/uploads/")?;
    if repository.is_empty() || uuid.is_empty() || uuid.contains('/') {
        return None;
    }
    Some((repository, uuid))
}

fn split_blob_path(path: &str) -> Option<(&str, &str)> {
    let (repository, digest) = path.rsplit_once("/blobs/")?;
    if repository.is_empty() || digest.is_empty() || digest.contains('/') {
        return None;
    }
    Some((repository, digest))
}

fn split_manifest_path(path: &str) -> Option<(&str, &str)> {
    let (repository, reference) = path.rsplit_once("/manifests/")?;
    if repository.is_empty() || reference.is_empty() || reference.contains('/') {
        return None;
    }
    Some((repository, reference))
}

pub(crate) fn validate_repository(value: &str) -> Result<(), ImageRegistryError> {
    if value.is_empty()
        || value
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-'))
    {
        return Err(ImageRegistryError::InvalidRepository {
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_reference(value: &str) -> Result<(), ImageRegistryError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | ':'))
    {
        return Err(ImageRegistryError::InvalidReference {
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_sha256_digest(value: &str) -> Result<String, ImageRegistryError> {
    let Some(encoded) = value.strip_prefix("sha256:") else {
        return Err(ImageRegistryError::InvalidDigest {
            value: value.to_string(),
        });
    };
    if encoded.len() != 64 || !encoded.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(ImageRegistryError::InvalidDigest {
            value: value.to_string(),
        });
    }
    Ok(format!("sha256:{}", encoded.to_ascii_lowercase()))
}

fn header_value<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<&'a str, ImageRegistryError> {
    let value = headers
        .get(name)
        .ok_or(ImageRegistryError::MissingHeader { name })?;
    value
        .to_str()
        .map_err(|_| ImageRegistryError::InvalidHeader { name })
}

fn manifest_media_type(headers: &HeaderMap) -> Result<String, ImageRegistryError> {
    match headers.get(CONTENT_TYPE) {
        Some(value) => value
            .to_str()
            .map(|value| value.to_string())
            .map_err(|error| ImageRegistryError::Body {
                message: format!("invalid manifest content-type header: {error}"),
            }),
        None => Ok(DEFAULT_MANIFEST_CONTENT_TYPE.into()),
    }
}

fn upload_location(repository: &str, uuid: &str) -> String {
    format!("/v2/{repository}/blobs/uploads/{uuid}")
}

fn upload_started_response(upload: UploadStarted) -> Response {
    let mut headers = base_headers();
    insert_header(&mut headers, LOCATION.as_str(), &upload.location).ok();
    insert_header(&mut headers, UPLOAD_UUID_HEADER, &upload.uuid).ok();
    insert_header(&mut headers, RANGE_HEADER, "0-0").ok();
    (StatusCode::ACCEPTED, headers).into_response()
}

fn upload_progress_response(repository: &str, uuid: &str, bytes: u64) -> Response {
    let mut headers = base_headers();
    insert_header(
        &mut headers,
        LOCATION.as_str(),
        &upload_location(repository, uuid),
    )
    .ok();
    insert_header(&mut headers, UPLOAD_UUID_HEADER, uuid).ok();
    insert_header(&mut headers, RANGE_HEADER, &range_header(bytes)).ok();
    (StatusCode::ACCEPTED, headers).into_response()
}

fn blob_committed_response(repository: &str, stored: BlobStored) -> Response {
    let mut headers = base_headers();
    insert_header(
        &mut headers,
        LOCATION.as_str(),
        &format!("/v2/{repository}/blobs/{}", stored.digest),
    )
    .ok();
    insert_header(&mut headers, CONTENT_DIGEST_HEADER, &stored.digest).ok();
    (StatusCode::CREATED, headers).into_response()
}

fn manifest_stored_response(
    repository: &str,
    reference: &str,
    manifest: ManifestStored,
) -> Response {
    let mut headers = base_headers();
    insert_header(
        &mut headers,
        LOCATION.as_str(),
        &format!("/v2/{repository}/manifests/{reference}"),
    )
    .ok();
    insert_header(&mut headers, CONTENT_DIGEST_HEADER, &manifest.digest).ok();
    insert_header(
        &mut headers,
        CONTENT_LENGTH.as_str(),
        &manifest.bytes.to_string(),
    )
    .ok();
    insert_header(&mut headers, CONTENT_TYPE.as_str(), &manifest.media_type).ok();
    (StatusCode::CREATED, headers).into_response()
}

fn base_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    insert_header(&mut headers, API_VERSION_HEADER, API_VERSION).ok();
    headers
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), ImageRegistryError> {
    let value = HeaderValue::from_str(value).map_err(|error| ImageRegistryError::Body {
        message: format!("invalid response header '{name}': {error}"),
    })?;
    headers.insert(name, value);
    Ok(())
}

fn range_header(bytes: u64) -> String {
    if bytes == 0 {
        "0-0".into()
    } else {
        format!("0-{}", bytes - 1)
    }
}

fn sha256_digest(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format_digest(hasher.finalize())
}

fn format_digest(bytes: impl AsRef<[u8]>) -> String {
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in bytes.as_ref() {
        write!(&mut encoded, "{byte:02x}").expect("write digest into string");
    }
    encoded
}

fn current_unix_secs() -> u64 {
    ployz_types::time::now_unix_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn blob_upload_streams_to_content_addressed_path() {
        let root = unique_temp_dir("ployz-image-registry");
        let registry = ImageRegistry::new(root);
        let headers = session_headers(
            registry
                .register_session("image-push-1", MachineId("machine-a".into()), "apps/web")
                .await,
        );
        let digest = sha256_digest(b"hello world");

        let upload = registry
            .start_upload("apps/web", &headers)
            .await
            .expect("start upload");
        let bytes = registry
            .append_upload("apps/web", &upload.uuid, &headers, Body::from("hello "))
            .await
            .expect("append upload");
        assert_eq!(bytes, 6);
        let stored = registry
            .finish_upload(
                "apps/web",
                &upload.uuid,
                &digest,
                &headers,
                Body::from("world"),
            )
            .await
            .expect("finish upload");

        assert_eq!(stored.digest, digest);
        assert_eq!(stored.bytes, 11);
        assert_eq!(
            fs::read(stored.path).await.expect("read blob"),
            b"hello world"
        );
        assert!(!registry.upload_path(&upload.uuid).exists());
    }

    #[tokio::test]
    async fn blob_upload_rejects_digest_mismatch_without_promoting_blob() {
        let root = unique_temp_dir("ployz-image-registry-mismatch");
        let registry = ImageRegistry::new(root);
        let headers = session_headers(
            registry
                .register_session("image-push-2", MachineId("machine-a".into()), "apps/web")
                .await,
        );
        let upload = registry
            .start_upload("apps/web", &headers)
            .await
            .expect("start upload");
        let error = registry
            .finish_upload(
                "apps/web",
                &upload.uuid,
                &sha256_digest(b"other"),
                &headers,
                Body::from("actual"),
            )
            .await
            .expect_err("digest mismatch");

        assert!(matches!(error, ImageRegistryError::DigestMismatch { .. }));
        assert!(!registry.upload_path(&upload.uuid).exists());
        assert!(registry.failed_upload_path(&upload.uuid).exists());
        assert!(
            !registry
                .blob_path(&sha256_digest(b"other"))
                .expect("blob path")
                .exists()
        );

        let retry = registry
            .start_upload("apps/web", &headers)
            .await
            .expect("restart upload");
        let stored = registry
            .finish_upload(
                "apps/web",
                &retry.uuid,
                &sha256_digest(b"actual"),
                &headers,
                Body::from("actual"),
            )
            .await
            .expect("finish restarted upload");
        assert_eq!(
            fs::read(stored.path).await.expect("read retried blob"),
            b"actual"
        );
    }

    #[tokio::test]
    async fn manifest_round_trips_from_filesystem() {
        let root = unique_temp_dir("ployz-image-registry-manifest");
        let registry = ImageRegistry::new(root);
        let headers = session_headers(
            registry
                .register_session("image-push-3", MachineId("machine-a".into()), "apps/web")
                .await,
        );

        let stored = registry
            .put_manifest(
                "apps/web",
                "latest",
                &headers,
                Body::from(r#"{"schemaVersion":2}"#),
            )
            .await
            .expect("put manifest");
        let response = registry
            .get_manifest("apps/web", "latest")
            .await
            .expect("get manifest")
            .expect("manifest exists");

        assert_eq!(stored.bytes, 19);
        assert_eq!(stored.media_type, DEFAULT_MANIFEST_CONTENT_TYPE);
        let (parts, body) = response.into_parts();
        assert_eq!(parts.status, StatusCode::OK);
        assert_eq!(
            parts.headers[CONTENT_DIGEST_HEADER],
            HeaderValue::from_str(&stored.digest).expect("digest header")
        );
        let body = axum::body::to_bytes(body, MAX_MANIFEST_BYTES)
            .await
            .expect("manifest body");
        assert_eq!(&body[..], br#"{"schemaVersion":2}"#);
    }

    #[tokio::test]
    async fn router_handles_blob_upload_and_head() {
        let root = unique_temp_dir("ployz-image-registry-router-blob");
        let registry = ImageRegistry::new(root);
        let headers = session_headers(
            registry
                .register_session("image-push-4", MachineId("machine-a".into()), "apps/web")
                .await,
        );
        let digest = sha256_digest(b"hello world");
        let router = registry.clone().router();

        let response = router
            .clone()
            .oneshot(registry_request(
                Method::POST,
                "/v2/apps/web/blobs/uploads/",
                &headers,
                Body::empty(),
            ))
            .await
            .expect("post upload");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let location = response.headers()[LOCATION]
            .to_str()
            .expect("location")
            .to_string();

        let response = router
            .clone()
            .oneshot(registry_request(
                Method::PATCH,
                &location,
                &headers,
                Body::from("hello "),
            ))
            .await
            .expect("patch upload");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            response.headers()[RANGE_HEADER],
            HeaderValue::from_static("0-5")
        );

        let response = router
            .clone()
            .oneshot(registry_request(
                Method::PUT,
                &format!("{location}?digest={digest}"),
                &headers,
                Body::from("world"),
            ))
            .await
            .expect("put upload");
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers()[CONTENT_DIGEST_HEADER],
            HeaderValue::from_str(&digest).expect("digest header")
        );

        let response = router
            .oneshot(registry_request(
                Method::HEAD,
                &format!("/v2/apps/web/blobs/{digest}"),
                &headers,
                Body::empty(),
            ))
            .await
            .expect("head blob");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CONTENT_LENGTH],
            HeaderValue::from_static("11")
        );
    }

    #[tokio::test]
    async fn router_preserves_manifest_media_type_and_head_length() {
        let root = unique_temp_dir("ployz-image-registry-router-manifest");
        let registry = ImageRegistry::new(root);
        let mut headers = session_headers(
            registry
                .register_session("image-push-5", MachineId("machine-a".into()), "apps/web")
                .await,
        );
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.docker.distribution.manifest.v2+json"),
        );
        let router = registry.clone().router();

        let response = router
            .clone()
            .oneshot(registry_request(
                Method::PUT,
                "/v2/apps/web/manifests/latest",
                &headers,
                Body::from(r#"{"schemaVersion":2}"#),
            ))
            .await
            .expect("put manifest");
        assert_eq!(response.status(), StatusCode::CREATED);

        let response = router
            .clone()
            .oneshot(registry_request(
                Method::GET,
                "/v2/apps/web/manifests/latest",
                &headers,
                Body::empty(),
            ))
            .await
            .expect("get manifest");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            HeaderValue::from_static("application/vnd.docker.distribution.manifest.v2+json")
        );
        assert_eq!(
            response.headers()[CONTENT_LENGTH],
            HeaderValue::from_static("19")
        );

        let response = router
            .oneshot(registry_request(
                Method::HEAD,
                "/v2/apps/web/manifests/latest",
                &headers,
                Body::empty(),
            ))
            .await
            .expect("head manifest");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            HeaderValue::from_static("application/vnd.docker.distribution.manifest.v2+json")
        );
        assert_eq!(
            response.headers()[CONTENT_LENGTH],
            HeaderValue::from_static("19")
        );
    }

    #[tokio::test]
    async fn router_rejects_unauthenticated_reads() {
        let root = unique_temp_dir("ployz-image-registry-read-auth");
        let registry = ImageRegistry::new(root);
        let headers = session_headers(
            registry
                .register_session("image-push-6", MachineId("machine-a".into()), "apps/web")
                .await,
        );
        let digest = sha256_digest(b"hello");
        registry
            .put_monolithic_blob("apps/web", &digest, &headers, Body::from("hello"))
            .await
            .expect("put blob");
        let response = registry
            .router()
            .oneshot(registry_request(
                Method::GET,
                &format!("/v2/apps/web/blobs/{digest}"),
                &HeaderMap::new(),
                Body::empty(),
            ))
            .await
            .expect("get blob");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn router_reports_blob_and_manifest_misses_with_registry_codes() {
        let root = unique_temp_dir("ployz-image-registry-misses");
        let registry = ImageRegistry::new(root);
        let headers = session_headers(
            registry
                .register_session("image-push-7", MachineId("machine-a".into()), "apps/web")
                .await,
        );
        let router = registry.router();
        let digest = sha256_digest(b"missing");

        let response = router
            .clone()
            .oneshot(registry_request(
                Method::GET,
                &format!("/v2/apps/web/blobs/{digest}"),
                &headers,
                Body::empty(),
            ))
            .await
            .expect("get missing blob");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = error_body(response).await;
        assert!(body.contains("BLOB_UNKNOWN"));

        let response = router
            .oneshot(registry_request(
                Method::GET,
                "/v2/apps/web/manifests/latest",
                &headers,
                Body::empty(),
            ))
            .await
            .expect("get missing manifest");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = error_body(response).await;
        assert!(body.contains("MANIFEST_UNKNOWN"));
    }

    #[tokio::test]
    async fn revoke_session_removes_active_uploads_and_failed_uploads() {
        let root = unique_temp_dir("ployz-image-registry-revoke");
        let registry = ImageRegistry::new(root);
        let session = registry
            .register_session("image-push-8", MachineId("machine-a".into()), "apps/web")
            .await;
        let headers = session_headers(session.clone());
        let upload = registry
            .start_upload("apps/web", &headers)
            .await
            .expect("start upload");
        let _error = registry
            .finish_upload(
                "apps/web",
                &upload.uuid,
                &sha256_digest(b"other"),
                &headers,
                Body::from("actual"),
            )
            .await
            .expect_err("digest mismatch");
        assert!(registry.failed_upload_path(&upload.uuid).exists());

        registry
            .revoke_session(&session.token)
            .await
            .expect("revoke session");

        assert!(!registry.failed_upload_path(&upload.uuid).exists());
        let error = registry
            .start_upload("apps/web", &headers)
            .await
            .expect_err("session revoked");
        assert!(matches!(error, ImageRegistryError::Unauthorized));
    }

    #[tokio::test]
    async fn session_headers_are_required() {
        let root = unique_temp_dir("ployz-image-registry-auth");
        let registry = ImageRegistry::new(root);
        let error = registry
            .start_upload("apps/web", &HeaderMap::new())
            .await
            .expect_err("missing auth");

        assert!(matches!(
            error,
            ImageRegistryError::MissingHeader {
                name: REGISTRY_SESSION_HEADER
            }
        ));
    }

    #[tokio::test]
    async fn listener_serves_registry_root() {
        let root = unique_temp_dir("ployz-image-registry-listener");
        let registry = ImageRegistry::new(root);
        let handle = serve("127.0.0.1:0".parse().expect("bind addr"), registry)
            .await
            .expect("start listener");
        let mut stream = tokio::net::TcpStream::connect(handle.bind_addr())
            .await
            .expect("connect listener");
        stream
            .write_all(b"GET /v2/ HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");

        handle.shutdown().await;

        let response = String::from_utf8(response).expect("utf8 response");
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("docker-distribution-api-version"));
    }

    fn session_headers(session: ImageRegistrySession) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            REGISTRY_SESSION_HEADER,
            HeaderValue::from_str(&session.token).expect("session token"),
        );
        headers.insert(
            REGISTRY_OPERATION_HEADER,
            HeaderValue::from_str(&session.operation_id).expect("operation id"),
        );
        headers.insert(
            REGISTRY_SOURCE_MACHINE_HEADER,
            HeaderValue::from_str(&session.source_machine.0).expect("source machine"),
        );
        headers
    }

    fn registry_request(
        method: Method,
        uri: &str,
        headers: &HeaderMap,
        body: Body,
    ) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        builder
            .headers_mut()
            .expect("headers")
            .extend(headers.clone());
        builder.body(body).expect("request")
    }

    async fn error_body(response: Response) -> String {
        let (_parts, body) = response.into_parts();
        let body = axum::body::to_bytes(body, MAX_MANIFEST_BYTES)
            .await
            .expect("error body");
        String::from_utf8(body.to_vec()).expect("utf8 body")
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let id = Uuid::new_v4();
        let path = std::env::temp_dir().join(format!("{label}-{id}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}

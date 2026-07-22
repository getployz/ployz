//! Machine-local image push and ensure handlers.

use super::image_ensure::ImageEnsureRuntime;
use super::response::{failure_message, machine_domain_error, machine_success};
use super::runner::MachineImageRemovalRunner;
use crate::roles::machine::execution::containerd_content::{
    ContainerdContentStore, ContentIngest, ContentWriteOutcome,
};
use ployz_core::ids::MachineId;
use ployz_core::image::{
    IMAGE_BLOB_CHUNK_MAX_BYTES, IMAGE_BLOB_PUSH_ACTION_CHUNK, IMAGE_BLOB_PUSH_ACTION_HEADER,
    IMAGE_BLOB_PUSH_OFFSET_HEADER, IMAGE_BLOB_PUSH_UPLOAD_ID_HEADER, ImageBlobCheckOk,
    ImageBlobCheckRequest, ImageBlobCheckResponse, ImageBlobPushOk, ImageBlobPushOutcome,
    ImageBlobPushRequest, ImageBlobPushResponse, ImageEnsureOk, ImageEnsureRequest,
    ImageEnsureResponse, ImageEnsureSource, ImageManifestPushOk, ImageManifestPushRequest,
    ImageManifestPushResponse, ImageRemoveDomainError, ImageRemoveOk, ImageRemoveRequest,
    ImageRemoveResponse, ImageRpcDomainError, ImageUploadId, OCI_IMAGE_CONFIG_MEDIA_TYPE,
    OCI_IMAGE_MANIFEST_MEDIA_TYPE, OciDigest, OciPlatform,
};
use ployz_core::machine::rpc::MachineRpcResponse;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const UPLOAD_SESSION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_UPLOAD_SESSIONS: usize = 16;

#[derive(Clone)]
pub(crate) struct AvailableImageService {
    content: ContainerdContentStore,
    seed_host: Ipv4Addr,
    uploads: Arc<Mutex<BTreeMap<ImageUploadId, Arc<Mutex<UploadSession>>>>>,
}

#[derive(Clone)]
pub(crate) struct MachineImageEnsureService {
    pub(crate) runtime: ImageEnsureRuntime,
    pub(crate) available: Option<AvailableImageService>,
}

impl AvailableImageService {
    #[must_use]
    pub(crate) fn new(content: ContainerdContentStore, seed_host: Ipv4Addr) -> Self {
        Self {
            content,
            seed_host,
            uploads: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub(crate) async fn ingest_build_layout(
        &self,
        layout: &ployz_build_executor::ValidatedOciLayout,
    ) -> Result<ployz_core::image::ImageContentLeaseExpiresAt, String> {
        let lease = self
            .content
            .acquire_lease()
            .await
            .map_err(|error| error.to_string())?;
        for blob in layout.blobs() {
            if let Err(error) = self
                .content
                .ingest_file(
                    blob.path(),
                    blob.digest().clone(),
                    blob.size(),
                    lease.clone(),
                )
                .await
            {
                let _ = self.content.release_lease(lease).await;
                return Err(error.to_string());
            }
        }
        Ok(lease.expires_at())
    }
}

struct UploadSession {
    ingest: ContentIngest,
    progress: UploadProgress,
    deadline: Instant,
}

enum UploadProgress {
    Writing {
        offset: u64,
        pending: BTreeMap<u64, Vec<u8>>,
    },
    Retained,
}

enum UploadChunkAction {
    Write(Vec<u8>),
    Buffered,
    NoOp,
}

impl UploadProgress {
    fn accept_chunk(
        &mut self,
        total_size: u64,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<UploadChunkAction, ImageRpcDomainError> {
        validate_chunk_bounds(total_size, offset, bytes.len())?;
        match self {
            Self::Retained => Ok(UploadChunkAction::NoOp),
            Self::Writing {
                offset: next_offset,
                pending,
            } => {
                if offset < *next_offset || pending.contains_key(&offset) {
                    return Err(ImageRpcDomainError::OffsetMismatch {
                        expected: *next_offset,
                        actual: offset,
                    });
                }
                if offset > *next_offset {
                    pending.insert(offset, bytes);
                    Ok(UploadChunkAction::Buffered)
                } else {
                    Ok(UploadChunkAction::Write(bytes))
                }
            }
        }
    }

    fn record_write(&mut self, outcome: ContentWriteOutcome) -> Option<Vec<u8>> {
        match outcome {
            ContentWriteOutcome::ExpectedContentRetained => {
                *self = Self::Retained;
                None
            }
            ContentWriteOutcome::AdvancedTo(next_offset) => {
                let Self::Writing { offset, pending } = self else {
                    return None;
                };
                *offset = next_offset;
                pending.remove(offset)
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OciManifest {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    #[serde(rename = "mediaType")]
    media_type: String,
    config: OciDescriptor,
    layers: Vec<OciDescriptor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OciDescriptor {
    #[serde(rename = "mediaType")]
    media_type: String,
    size: u64,
    digest: OciDigest,
}

#[derive(Deserialize)]
struct OciImageConfig {
    architecture: String,
    os: String,
}

pub(crate) async fn handle_image_blob_check(
    machine_id: MachineId,
    state: Option<AvailableImageService>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let Some(state) = state else {
        return unavailable(machine_id);
    };
    let request = match serde_json::from_slice::<ImageBlobCheckRequest>(&request.payload) {
        Ok(request) => request,
        Err(error) => return invalid_request(machine_id, error),
    };
    let mut present = Vec::new();
    for digest in request.digests {
        match state.content.blob_info(&digest).await {
            Ok(Some(_)) => present.push(digest),
            Ok(None) => {}
            Err(error) => {
                return image_error(
                    machine_id,
                    ImageRpcDomainError::StorageFailed {
                        message: failure_message(error.to_string()),
                    },
                );
            }
        }
    }
    machine_success(ImageBlobCheckResponse::Ok(ImageBlobCheckOk {
        machine_id,
        present,
    }))
}

pub(crate) async fn handle_image_blob_push(
    machine_id: MachineId,
    state: Option<AvailableImageService>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let Some(state) = state else {
        return unavailable(machine_id);
    };
    if header(&request, IMAGE_BLOB_PUSH_ACTION_HEADER)
        .is_some_and(|value| value == IMAGE_BLOB_PUSH_ACTION_CHUNK)
    {
        return push_chunk(machine_id, &state, request).await;
    }
    let action = match serde_json::from_slice::<ImageBlobPushRequest>(&request.payload) {
        Ok(action) => action,
        Err(error) => return invalid_request(machine_id, error),
    };
    match action {
        ImageBlobPushRequest::Begin { digest, total_size } => {
            begin_upload(machine_id, &state, digest, total_size).await
        }
        ImageBlobPushRequest::Commit { upload_id } => {
            commit_upload(machine_id, &state, upload_id).await
        }
    }
}

async fn begin_upload(
    machine_id: MachineId,
    state: &AvailableImageService,
    digest: OciDigest,
    total_size: u64,
) -> NatsServiceResponse {
    sweep_expired_uploads(state).await;
    let lease = match state.content.acquire_lease().await {
        Ok(lease) => lease,
        Err(error) => {
            return storage_error(machine_id, error.to_string());
        }
    };
    match state.content.blob_info(&digest).await {
        Ok(Some(info)) if info.size == total_size => {
            if let Err(error) = state.content.retain_content(&lease, &digest).await {
                let _ = state.content.release_lease(lease).await;
                return storage_error(machine_id, error.to_string());
            }
            return machine_success(ImageBlobPushResponse::Ok(ImageBlobPushOk {
                machine_id,
                outcome: ImageBlobPushOutcome::Retained {
                    digest,
                    size: total_size,
                    lease_expires_at: lease.expires_at(),
                },
            }));
        }
        Ok(Some(info)) => {
            let _ = state.content.release_lease(lease).await;
            return invalid_message(
                machine_id,
                &format!(
                    "content {digest} has size {}, expected {total_size}",
                    info.size
                ),
            );
        }
        Ok(None) => {}
        Err(error) => {
            let _ = state.content.release_lease(lease).await;
            return storage_error(machine_id, error.to_string());
        }
    }
    let upload_id = match ImageUploadId::try_new(crate::identity::format_nuid_identity(
        "upload_",
        &nuid::next(),
    )) {
        Ok(upload_id) => upload_id,
        Err(error) => {
            return storage_error(machine_id, error.to_string());
        }
    };
    let ingest = ContentIngest::new(digest, total_size, lease);
    let mut uploads = state.uploads.lock().await;
    if uploads.len() >= MAX_UPLOAD_SESSIONS {
        drop(uploads);
        let _ = state.content.release_lease(ingest.lease()).await;
        return image_error(
            machine_id,
            ImageRpcDomainError::UploadBusy {
                maximum: u16::try_from(MAX_UPLOAD_SESSIONS).unwrap_or(u16::MAX),
            },
        );
    }
    uploads.insert(
        upload_id.clone(),
        Arc::new(Mutex::new(UploadSession {
            ingest,
            progress: UploadProgress::Writing {
                offset: 0,
                pending: BTreeMap::new(),
            },
            deadline: Instant::now() + UPLOAD_SESSION_TIMEOUT,
        })),
    );
    machine_success(ImageBlobPushResponse::Ok(ImageBlobPushOk {
        machine_id,
        outcome: ImageBlobPushOutcome::Begun { upload_id },
    }))
}

async fn sweep_expired_uploads(state: &AvailableImageService) {
    let sessions = state
        .uploads
        .lock()
        .await
        .iter()
        .map(|(upload_id, session)| (upload_id.clone(), Arc::clone(session)))
        .collect::<Vec<_>>();
    for (upload_id, session) in sessions {
        let session = session.lock().await;
        if Instant::now() < session.deadline {
            continue;
        }
        let lease = session.ingest.lease();
        drop(session);
        state.uploads.lock().await.remove(&upload_id);
        let _ = state.content.release_lease(lease).await;
    }
}

async fn push_chunk(
    machine_id: MachineId,
    state: &AvailableImageService,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    if request.payload.len() > IMAGE_BLOB_CHUNK_MAX_BYTES {
        return image_error(
            machine_id,
            ImageRpcDomainError::ChunkTooLarge {
                size: u64::try_from(request.payload.len()).unwrap_or(u64::MAX),
                maximum: u64::try_from(IMAGE_BLOB_CHUNK_MAX_BYTES).unwrap_or(u64::MAX),
            },
        );
    }
    let upload_id = match header(&request, IMAGE_BLOB_PUSH_UPLOAD_ID_HEADER)
        .map(str::to_owned)
        .ok_or("missing upload id")
        .and_then(|value| ImageUploadId::try_new(value).map_err(|_| "invalid upload id"))
    {
        Ok(upload_id) => upload_id,
        Err(message) => return invalid_message(machine_id, message),
    };
    let offset = match header(&request, IMAGE_BLOB_PUSH_OFFSET_HEADER)
        .ok_or("missing chunk offset")
        .and_then(|value| value.parse::<u64>().map_err(|_| "invalid chunk offset"))
    {
        Ok(offset) => offset,
        Err(message) => return invalid_message(machine_id, message),
    };
    let session = {
        let uploads = state.uploads.lock().await;
        uploads.get(&upload_id).cloned()
    };
    let Some(session) = session else {
        return image_error(
            machine_id,
            ImageRpcDomainError::UploadNotFound { upload_id },
        );
    };
    let mut session = session.lock().await;
    if Instant::now() >= session.deadline {
        let lease = session.ingest.lease();
        drop(session);
        state.uploads.lock().await.remove(&upload_id);
        let _ = state.content.release_lease(lease).await;
        return image_error(
            machine_id,
            ImageRpcDomainError::UploadNotFound { upload_id },
        );
    }
    let total_size = session.ingest.total_size();
    let mut bytes = match session
        .progress
        .accept_chunk(total_size, offset, request.payload)
    {
        Ok(UploadChunkAction::Write(bytes)) => Some(bytes),
        Ok(UploadChunkAction::Buffered | UploadChunkAction::NoOp) => None,
        Err(error) => return image_error(machine_id, error),
    };
    while let Some(chunk) = bytes {
        let write_offset = match &session.progress {
            UploadProgress::Writing { offset, .. } => *offset,
            UploadProgress::Retained => break,
        };
        let outcome = match state
            .content
            .write_ingest_chunk(&session.ingest, write_offset, chunk)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => return storage_error(machine_id, error.to_string()),
        };
        bytes = session.progress.record_write(outcome);
    }
    machine_success(ImageBlobPushResponse::Ok(ImageBlobPushOk {
        machine_id,
        outcome: ImageBlobPushOutcome::ChunkAccepted { upload_id },
    }))
}

fn validate_chunk_bounds(
    total_size: u64,
    offset: u64,
    chunk_size: usize,
) -> Result<(), ImageRpcDomainError> {
    let size = u64::try_from(chunk_size).unwrap_or(u64::MAX);
    if offset
        .checked_add(size)
        .is_some_and(|end| end <= total_size)
    {
        return Ok(());
    }
    Err(ImageRpcDomainError::ChunkOutOfBounds {
        total_size,
        offset,
        size,
    })
}

async fn commit_upload(
    machine_id: MachineId,
    state: &AvailableImageService,
    upload_id: ImageUploadId,
) -> NatsServiceResponse {
    let Some(session) = state.uploads.lock().await.remove(&upload_id) else {
        return image_error(
            machine_id,
            ImageRpcDomainError::UploadNotFound { upload_id },
        );
    };
    let session = session.lock().await;
    if Instant::now() >= session.deadline {
        let lease = session.ingest.lease();
        drop(session);
        let _ = state.content.release_lease(lease).await;
        return image_error(
            machine_id,
            ImageRpcDomainError::UploadNotFound { upload_id },
        );
    }
    match &session.progress {
        UploadProgress::Writing { offset, .. } => {
            if let Err(error) = state.content.commit_ingest(&session.ingest, *offset).await {
                return storage_error(machine_id, error.to_string());
            }
        }
        UploadProgress::Retained => {}
    }
    let digest = session.ingest.digest().clone();
    let size = session.ingest.total_size();
    let lease_expires_at = session.ingest.lease().expires_at();
    machine_success(ImageBlobPushResponse::Ok(ImageBlobPushOk {
        machine_id,
        outcome: committed_upload_outcome(digest, size, lease_expires_at),
    }))
}

pub(crate) async fn handle_image_manifest_push(
    machine_id: MachineId,
    state: Option<AvailableImageService>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let Some(state) = state else {
        return unavailable(machine_id);
    };
    let request = match serde_json::from_slice::<ImageManifestPushRequest>(&request.payload) {
        Ok(request) => request,
        Err(error) => return invalid_request(machine_id, error),
    };
    let manifest = match parse_manifest(&request.manifest_bytes) {
        Ok(manifest) => manifest,
        Err(message) => return invalid_message(machine_id, message),
    };
    if let Some(response) = verify_manifest_content(&machine_id, &state, &manifest).await {
        return response;
    }
    let platform = match read_platform(&state, &manifest.config.digest).await {
        Ok(platform) => platform,
        Err(error) => return image_error(machine_id, error),
    };
    let manifest_digest = OciDigest::sha256(&request.manifest_bytes);
    let lease = match state.content.acquire_lease().await {
        Ok(lease) => lease,
        Err(error) => {
            return storage_error(machine_id, error.to_string());
        }
    };
    let ingest = ContentIngest::new(
        manifest_digest.clone(),
        u64::try_from(request.manifest_bytes.len()).unwrap_or(u64::MAX),
        lease,
    );
    let write = match state
        .content
        .write_ingest_chunk(&ingest, 0, request.manifest_bytes)
        .await
    {
        Ok(write) => write,
        Err(error) => {
            return storage_error(machine_id, error.to_string());
        }
    };
    match write {
        ContentWriteOutcome::AdvancedTo(offset) => {
            if let Err(error) = state.content.commit_ingest(&ingest, offset).await {
                return storage_error(machine_id, error.to_string());
            }
        }
        ContentWriteOutcome::ExpectedContentRetained => {}
    }
    let lease_expires_at = ingest.lease().expires_at();
    let image_id = manifest.config.digest.clone();
    machine_success(ImageManifestPushResponse::Ok(ImageManifestPushOk {
        machine_id,
        manifest_digest,
        image_id,
        platform,
        lease_expires_at,
    }))
}

fn committed_upload_outcome(
    digest: OciDigest,
    size: u64,
    lease_expires_at: ployz_core::image::ImageContentLeaseExpiresAt,
) -> ImageBlobPushOutcome {
    ImageBlobPushOutcome::Committed {
        digest,
        size,
        lease_expires_at,
    }
}

pub(crate) async fn handle_image_ensure(
    machine_id: MachineId,
    state: Option<MachineImageEnsureService>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let Some(state) = state else {
        return unavailable(machine_id);
    };
    let request = match serde_json::from_slice::<ImageEnsureRequest>(&request.payload) {
        Ok(request) => request,
        Err(error) => return invalid_request(machine_id, error),
    };
    let status = match request {
        ImageEnsureRequest::Start { owner, mut source } => {
            if let ImageEnsureSource::LocalSeed {
                repository,
                manifest_digest,
                image_id,
                platform,
            } = &source
            {
                let Some(available) = &state.available else {
                    return unavailable(machine_id);
                };
                let inspected = match inspect_content(available, manifest_digest, image_id).await {
                    Ok(inspected) => inspected,
                    Err(error) => return image_error(machine_id, error),
                };
                if let Err(error) = ensure_platform(platform, &inspected.platform) {
                    return image_error(machine_id, error);
                }
                source = ImageEnsureSource::MeshSeed {
                    seed_host: available.seed_host,
                    repository: repository.clone(),
                    manifest_digest: manifest_digest.clone(),
                    image_id: image_id.clone(),
                    platform: platform.clone(),
                };
            }
            match state.runtime.start(owner, source).await {
                Ok(status) => status,
                Err(error) => return image_error(machine_id, error),
            }
        }
        ImageEnsureRequest::Status { owner } => match state.runtime.status(&owner).await {
            Ok(status) => status,
            Err(error) => return image_error(machine_id, error),
        },
        ImageEnsureRequest::Cancel { owner } => match state.runtime.cancel(&owner).await {
            Ok(status) => status,
            Err(error) => return image_error(machine_id, error),
        },
    };
    machine_success(ImageEnsureResponse::Ok(ImageEnsureOk {
        machine_id,
        ensure_status: status,
    }))
}

pub(crate) async fn handle_image_remove<R>(
    machine_id: MachineId,
    docker: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineImageRemovalRunner,
{
    let request = match serde_json::from_slice::<ImageRemoveRequest>(&request.payload) {
        Ok(request) => request,
        Err(error) => {
            return image_remove_error(
                machine_id,
                ImageRemoveDomainError::InvalidRequest {
                    message: failure_message(format!("invalid request: {error}")),
                },
            );
        }
    };
    let ImageRemoveRequest {
        operation_id,
        image_identity,
    } = request;
    match docker.remove_image(&image_identity).await {
        Ok(outcome) => machine_success(ImageRemoveResponse::Ok(ImageRemoveOk {
            machine_id,
            outcome,
        })),
        Err(error) => image_remove_error(
            machine_id,
            ImageRemoveDomainError::RemoveFailed {
                message: failure_message(format!(
                    "operation {} image removal failed: {}",
                    operation_id.as_str(),
                    error
                )),
            },
        ),
    }
}

fn image_remove_error(machine_id: MachineId, error: ImageRemoveDomainError) -> NatsServiceResponse {
    machine_domain_error(ImageRemoveResponse::DomainError { machine_id, error })
}

struct InspectedImage {
    platform: OciPlatform,
}

fn ensure_platform(
    expected: &OciPlatform,
    actual: &OciPlatform,
) -> Result<(), ImageRpcDomainError> {
    if expected == actual {
        return Ok(());
    }
    Err(ImageRpcDomainError::PlatformMismatch {
        expected: expected.clone(),
        actual: actual.clone(),
    })
}

async fn inspect_content(
    state: &AvailableImageService,
    manifest_digest: &OciDigest,
    image_id: &OciDigest,
) -> Result<InspectedImage, ImageRpcDomainError> {
    let bytes = state
        .content
        .read_blob(manifest_digest)
        .await
        .map_err(|error| ImageRpcDomainError::StorageFailed {
            message: failure_message(error.to_string()),
        })?;
    let bytes = require_manifest_blob(bytes, manifest_digest)?;
    inspect_manifest_bytes(&bytes, manifest_digest, image_id)?;
    let platform = read_platform(state, image_id).await?;
    Ok(InspectedImage { platform })
}

fn require_manifest_blob(
    bytes: Option<Vec<u8>>,
    digest: &OciDigest,
) -> Result<Vec<u8>, ImageRpcDomainError> {
    bytes.ok_or_else(|| ImageRpcDomainError::ImageMissing {
        digest: digest.clone(),
    })
}

fn inspect_manifest_bytes(
    bytes: &[u8],
    manifest_digest: &OciDigest,
    image_id: &OciDigest,
) -> Result<OciManifest, ImageRpcDomainError> {
    let actual_manifest_digest = OciDigest::sha256(bytes);
    if actual_manifest_digest != *manifest_digest {
        return Err(ImageRpcDomainError::DigestMismatch {
            expected: manifest_digest.clone(),
            actual: actual_manifest_digest,
        });
    }
    let manifest =
        parse_manifest(bytes).map_err(|message| ImageRpcDomainError::InvalidRequest {
            message: failure_message(message),
        })?;
    if manifest.config.digest != *image_id {
        return Err(ImageRpcDomainError::ConfigMismatch {
            expected: image_id.clone(),
            actual: manifest.config.digest,
        });
    }
    Ok(manifest)
}

fn parse_manifest(bytes: &[u8]) -> Result<OciManifest, &'static str> {
    let manifest = serde_json::from_slice::<OciManifest>(bytes).map_err(|_| "invalid manifest")?;
    if manifest.schema_version != 2 {
        return Err("manifest schema version must be 2");
    }
    if manifest.media_type != OCI_IMAGE_MANIFEST_MEDIA_TYPE {
        return Err("unsupported image manifest media type");
    }
    if manifest.config.media_type != OCI_IMAGE_CONFIG_MEDIA_TYPE {
        return Err("unsupported image config media type");
    }
    Ok(manifest)
}

async fn verify_manifest_content(
    machine_id: &MachineId,
    state: &AvailableImageService,
    manifest: &OciManifest,
) -> Option<NatsServiceResponse> {
    for descriptor in std::iter::once(&manifest.config).chain(manifest.layers.iter()) {
        match state.content.blob_info(&descriptor.digest).await {
            Ok(Some(info)) if info.size == descriptor.size => {}
            Ok(Some(info)) => {
                return Some(image_error(
                    machine_id.clone(),
                    ImageRpcDomainError::InvalidRequest {
                        message: failure_message(format!(
                            "blob {} size mismatch: manifest {}, content {}",
                            descriptor.digest, descriptor.size, info.size
                        )),
                    },
                ));
            }
            Ok(None) => {
                return Some(image_error(
                    machine_id.clone(),
                    ImageRpcDomainError::ImageMissing {
                        digest: descriptor.digest.clone(),
                    },
                ));
            }
            Err(error) => {
                return Some(storage_error(machine_id.clone(), error.to_string()));
            }
        }
    }
    None
}

async fn read_platform(
    state: &AvailableImageService,
    config_digest: &OciDigest,
) -> Result<OciPlatform, ImageRpcDomainError> {
    let bytes = state
        .content
        .read_blob(config_digest)
        .await
        .map_err(|error| ImageRpcDomainError::StorageFailed {
            message: failure_message(error.to_string()),
        })?
        .ok_or_else(|| ImageRpcDomainError::ImageMissing {
            digest: config_digest.clone(),
        })?;
    let config = serde_json::from_slice::<OciImageConfig>(&bytes).map_err(|error| {
        ImageRpcDomainError::InvalidRequest {
            message: failure_message(format!("invalid image config: {error}")),
        }
    })?;
    let OciImageConfig { architecture, os } = config;
    OciPlatform::try_new(os, architecture).map_err(|error| ImageRpcDomainError::InvalidRequest {
        message: failure_message(format!("invalid image platform: {error}")),
    })
}

fn header<'a>(request: &'a NatsServiceRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .as_ref()
        .and_then(|headers| headers.get(name))
        .map(|value| value.as_str())
}

fn unavailable(machine_id: MachineId) -> NatsServiceResponse {
    image_error(
        machine_id,
        ImageRpcDomainError::ServiceUnavailable {
            message: failure_message("image storage is unavailable"),
        },
    )
}

fn invalid_request(machine_id: MachineId, error: serde_json::Error) -> NatsServiceResponse {
    invalid_message(machine_id, &format!("invalid request: {error}"))
}

fn invalid_message(machine_id: MachineId, message: &str) -> NatsServiceResponse {
    image_error(
        machine_id,
        ImageRpcDomainError::InvalidRequest {
            message: failure_message(message),
        },
    )
}

fn storage_error(machine_id: MachineId, message: String) -> NatsServiceResponse {
    image_error(
        machine_id,
        ImageRpcDomainError::StorageFailed {
            message: failure_message(message),
        },
    )
}

fn image_error(machine_id: MachineId, error: ImageRpcDomainError) -> NatsServiceResponse {
    machine_domain_error(MachineRpcResponse::<serde_json::Value, _>::DomainError {
        machine_id,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        UploadChunkAction, UploadProgress, committed_upload_outcome, ensure_platform,
        inspect_manifest_bytes, parse_manifest, require_manifest_blob, validate_chunk_bounds,
    };
    use crate::roles::machine::execution::containerd_content::ContentWriteOutcome;
    use ployz_core::image::{
        ImageBlobPushOutcome, ImageContentLeaseExpiresAt, ImageRpcDomainError, OciDigest,
        OciPlatform,
    };
    use std::collections::BTreeMap;

    #[test]
    fn manifest_parser_rejects_manifest_lists() {
        let manifest_list = br#"{"schemaVersion":2,"manifests":[]}"#;

        assert!(parse_manifest(manifest_list).is_err());
    }

    #[test]
    fn chunk_bounds_reject_data_past_the_declared_upload_size() {
        assert!(validate_chunk_bounds(10, 8, 2).is_ok());
        assert!(validate_chunk_bounds(10, 8, 3).is_err());
        assert!(validate_chunk_bounds(10, u64::MAX, 1).is_err());
    }

    #[test]
    fn image_ensure_accepts_the_requested_platform() {
        let platform = platform("amd64");

        assert_eq!(ensure_platform(&platform, &platform), Ok(()));
    }

    #[test]
    fn image_ensure_reports_a_typed_platform_mismatch() {
        let expected = platform("arm64");
        let actual = platform("amd64");

        assert_eq!(
            ensure_platform(&expected, &actual),
            Err(ImageRpcDomainError::PlatformMismatch { expected, actual })
        );
    }

    #[test]
    fn image_ensure_reports_typed_manifest_digest_and_config_mismatches() {
        let config = OciDigest::try_new(format!("sha256:{}", "b".repeat(64))).expect("config");
        let bytes = format!(r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","size":2,"digest":"{}"}},"layers":[]}}"#, config.as_str()).into_bytes();
        let actual = OciDigest::sha256(&bytes);
        let wrong_manifest =
            OciDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
        assert!(matches!(
            inspect_manifest_bytes(&bytes, &wrong_manifest, &config),
            Err(ImageRpcDomainError::DigestMismatch { .. })
        ));
        let wrong_config =
            OciDigest::try_new(format!("sha256:{}", "c".repeat(64))).expect("config");
        assert!(matches!(
            inspect_manifest_bytes(&bytes, &actual, &wrong_config),
            Err(ImageRpcDomainError::ConfigMismatch { .. })
        ));
    }

    #[test]
    fn absent_seed_manifest_maps_to_typed_image_missing() {
        let digest = OciDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
        let result = require_manifest_blob(None, &digest);
        assert_eq!(result, Err(ImageRpcDomainError::ImageMissing { digest }));
    }

    #[test]
    fn retained_upload_discards_buffered_chunks_and_accepts_remaining_chunks_as_no_ops() {
        let mut progress = UploadProgress::Writing {
            offset: 0,
            pending: BTreeMap::from([(4, vec![5, 6])]),
        };

        assert!(
            progress
                .record_write(ContentWriteOutcome::ExpectedContentRetained)
                .is_none()
        );
        assert!(matches!(progress, UploadProgress::Retained));
        assert!(matches!(
            progress.accept_chunk(8, 2, vec![3, 4]),
            Ok(UploadChunkAction::NoOp)
        ));
        assert!(matches!(
            progress.accept_chunk(8, 7, vec![8, 9]),
            Err(ImageRpcDomainError::ChunkOutOfBounds {
                total_size: 8,
                offset: 7,
                size: 2,
            })
        ));
    }

    #[test]
    fn retained_upload_commit_keeps_the_existing_committed_outcome() {
        let digest = OciDigest::sha256(b"blob");
        let lease_expires_at = ImageContentLeaseExpiresAt::try_new(123).expect("lease expiry");

        assert_eq!(
            committed_upload_outcome(digest.clone(), 4, lease_expires_at),
            ImageBlobPushOutcome::Committed {
                digest,
                size: 4,
                lease_expires_at,
            }
        );
    }

    fn platform(architecture: &str) -> OciPlatform {
        OciPlatform::try_new("linux", architecture).expect("platform")
    }
}

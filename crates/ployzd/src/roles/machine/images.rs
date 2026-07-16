//! Machine-local image push and ensure handlers.

use super::protocol::MachineImagePull;
use super::response::{failure_message, machine_domain_error, machine_success};
use super::runner::MachineImageRemovalRunner;
use crate::roles::machine::execution::containerd_content::{
    ContainerdContentStore, ContentIngest, ContentLease,
};
use crate::roles::machine::execution::docker::runner::DockerManagedContainerRunner;
use ployz_core::ids::MachineId;
use ployz_core::image::{
    IMAGE_BLOB_CHUNK_MAX_BYTES, IMAGE_BLOB_PUSH_ACTION_CHUNK, IMAGE_BLOB_PUSH_ACTION_HEADER,
    IMAGE_BLOB_PUSH_OFFSET_HEADER, IMAGE_BLOB_PUSH_UPLOAD_ID_HEADER, ImageBlobCheckOk,
    ImageBlobCheckRequest, ImageBlobCheckResponse, ImageBlobPushOk, ImageBlobPushOutcome,
    ImageBlobPushRequest, ImageBlobPushResponse, ImageEnsureOk, ImageEnsureRequest,
    ImageEnsureResponse, ImageManifestPushOk, ImageManifestPushRequest, ImageManifestPushResponse,
    ImageRemoveDomainError, ImageRemoveOk, ImageRemoveRequest, ImageRemoveResponse,
    ImageRpcDomainError, ImageUploadId, OCI_IMAGE_CONFIG_MEDIA_TYPE, OCI_IMAGE_MANIFEST_MEDIA_TYPE,
    OciDigest, OciPlatform,
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
/// In-memory retention for committed-but-unensured leases, mirroring the
/// containerd-side `gc.expire` label so the map never outlives the leases it
/// guards. An ensure releases entries early; abandoned pushes expire here.
const COMMITTED_LEASE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const MAX_UPLOAD_SESSIONS: usize = 16;
const SELF_PULL_ATTEMPTS: u8 = 10;
const SELF_PULL_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(crate) struct AvailableImageService {
    content: ContainerdContentStore,
    docker: DockerManagedContainerRunner,
    seed_host: Ipv4Addr,
    uploads: Arc<Mutex<BTreeMap<ImageUploadId, Arc<Mutex<UploadSession>>>>>,
    committed_leases: Arc<Mutex<BTreeMap<OciDigest, CommittedLease>>>,
}

/// A committed blob or manifest lease awaiting release by an ensure, with the
/// deadline after which the sweeper drops it (the containerd lease itself
/// expires via its `gc.expire` label on the same schedule).
struct CommittedLease {
    lease: ContentLease,
    expires_at: Instant,
}

impl AvailableImageService {
    #[must_use]
    pub(crate) fn new(
        content: ContainerdContentStore,
        docker: DockerManagedContainerRunner,
        seed_host: Ipv4Addr,
    ) -> Self {
        Self {
            content,
            docker,
            seed_host,
            uploads: Arc::new(Mutex::new(BTreeMap::new())),
            committed_leases: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub(crate) async fn ingest_build_layout(
        &self,
        layout: &crate::roles::machine::execution::build::ValidatedOciLayout,
    ) -> Result<(), String> {
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
        store_committed_lease(self, layout.manifest_digest().clone(), lease).await;
        Ok(())
    }
}

struct UploadSession {
    ingest: ContentIngest,
    offset: u64,
    pending: BTreeMap<u64, Vec<u8>>,
    deadline: Instant,
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
    sweep_expired_committed_leases(state).await;
    let lease = match state.content.acquire_lease().await {
        Ok(lease) => lease,
        Err(error) => {
            return storage_error(machine_id, error.to_string());
        }
    };
    let upload_id =
        match ImageUploadId::try_new(format!("upload_{}", nuid::next().to_ascii_lowercase())) {
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
            offset: 0,
            pending: BTreeMap::new(),
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

/// Stores a freshly committed lease for later release by an ensure; a lease
/// replaced by a re-push of the same digest is released immediately instead
/// of waiting out its containerd expiry.
async fn store_committed_lease(
    state: &AvailableImageService,
    digest: OciDigest,
    lease: ContentLease,
) {
    let replaced = state.committed_leases.lock().await.insert(
        digest,
        CommittedLease {
            lease,
            expires_at: Instant::now() + COMMITTED_LEASE_TIMEOUT,
        },
    );
    if let Some(replaced) = replaced {
        let _ = state.content.release_lease(replaced.lease).await;
    }
}

/// Drops committed-lease entries whose deadline passed: their containerd
/// leases expire on the same schedule via `gc.expire`, so an abandoned push
/// stops costing memory here and content there at the same time.
async fn sweep_expired_committed_leases(state: &AvailableImageService) {
    let now = Instant::now();
    let expired = {
        let mut committed = state.committed_leases.lock().await;
        let expired_digests = committed
            .iter()
            .filter(|(_, entry)| now >= entry.expires_at)
            .map(|(digest, _)| digest.clone())
            .collect::<Vec<_>>();
        expired_digests
            .into_iter()
            .filter_map(|digest| committed.remove(&digest))
            .collect::<Vec<_>>()
    };
    for entry in expired {
        let _ = state.content.release_lease(entry.lease).await;
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
    if let Err(error) =
        validate_chunk_bounds(session.ingest.total_size(), offset, request.payload.len())
    {
        return image_error(machine_id, error);
    }
    if offset < session.offset || session.pending.contains_key(&offset) {
        return image_error(
            machine_id,
            ImageRpcDomainError::OffsetMismatch {
                expected: session.offset,
                actual: offset,
            },
        );
    }
    if offset > session.offset {
        session.pending.insert(offset, request.payload);
    } else {
        let mut bytes = request.payload;
        loop {
            let next_offset = match state
                .content
                .write_ingest_chunk(&session.ingest, session.offset, bytes)
                .await
            {
                Ok(next_offset) => next_offset,
                Err(error) => {
                    return storage_error(machine_id, error.to_string());
                }
            };
            session.offset = next_offset;
            let pending_offset = session.offset;
            let Some(pending) = session.pending.remove(&pending_offset) else {
                break;
            };
            bytes = pending;
        }
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
    if let Err(error) = state
        .content
        .commit_ingest(&session.ingest, session.offset)
        .await
    {
        return storage_error(machine_id, error.to_string());
    }
    let digest = session.ingest.digest().clone();
    let size = session.ingest.total_size();
    store_committed_lease(state, digest.clone(), session.ingest.lease()).await;
    machine_success(ImageBlobPushResponse::Ok(ImageBlobPushOk {
        machine_id,
        outcome: ImageBlobPushOutcome::Committed { digest, size },
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
    let offset = match state
        .content
        .write_ingest_chunk(&ingest, 0, request.manifest_bytes)
        .await
    {
        Ok(offset) => offset,
        Err(error) => {
            return storage_error(machine_id, error.to_string());
        }
    };
    if let Err(error) = state.content.commit_ingest(&ingest, offset).await {
        return storage_error(machine_id, error.to_string());
    }
    store_committed_lease(&state, manifest_digest.clone(), ingest.lease()).await;
    let image_id = manifest.config.digest.clone();
    machine_success(ImageManifestPushResponse::Ok(ImageManifestPushOk {
        machine_id,
        manifest_digest,
        image_id,
        platform,
    }))
}

pub(crate) async fn handle_image_ensure(
    machine_id: MachineId,
    state: Option<AvailableImageService>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let Some(state) = state else {
        return unavailable(machine_id);
    };
    let request = match serde_json::from_slice::<ImageEnsureRequest>(&request.payload) {
        Ok(request) => request,
        Err(error) => return invalid_request(machine_id, error),
    };
    let inspected = match inspect_content(&state, &request.manifest_digest, &request.image_id).await
    {
        Ok(inspected) => inspected,
        Err(error) => return image_error(machine_id, error),
    };
    if let Err(error) = ensure_platform(&request.platform, &inspected.platform) {
        return image_error(machine_id, error);
    }
    let reference = MachineImagePull::MeshSeed {
        seed_host: state.seed_host,
        repository: request.repository,
        manifest_digest: request.manifest_digest.clone(),
    }
    .reference();
    for attempt in 1..=SELF_PULL_ATTEMPTS {
        match state.docker.pull_image(&reference, None).await {
            Ok(()) => break,
            Err(error) if attempt == SELF_PULL_ATTEMPTS => {
                return image_error(
                    machine_id,
                    ImageRpcDomainError::SelfPullFailed {
                        message: failure_message(runner_error_message(error)),
                    },
                );
            }
            Err(_) => tokio::time::sleep(SELF_PULL_RETRY_DELAY).await,
        }
    }
    if let Err(error) =
        release_manifest_leases(&state, &inspected.manifest, &request.manifest_digest).await
    {
        return storage_error(machine_id, error);
    }
    machine_success(ImageEnsureResponse::Ok(ImageEnsureOk {
        machine_id,
        platform: inspected.platform,
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
    manifest: OciManifest,
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
        })?
        .ok_or_else(|| ImageRpcDomainError::ImageMissing {
            digest: manifest_digest.clone(),
        })?;
    let actual_manifest_digest = OciDigest::sha256(&bytes);
    if actual_manifest_digest != *manifest_digest {
        return Err(ImageRpcDomainError::DigestMismatch {
            expected: manifest_digest.clone(),
            actual: actual_manifest_digest,
        });
    }
    let manifest =
        parse_manifest(&bytes).map_err(|message| ImageRpcDomainError::InvalidRequest {
            message: failure_message(message),
        })?;
    if manifest.config.digest != *image_id {
        return Err(ImageRpcDomainError::ConfigMismatch {
            expected: image_id.clone(),
            actual: manifest.config.digest,
        });
    }
    let platform = read_platform(state, image_id).await?;
    Ok(InspectedImage { manifest, platform })
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

async fn release_manifest_leases(
    state: &AvailableImageService,
    manifest: &OciManifest,
    manifest_digest: &OciDigest,
) -> Result<(), String> {
    let digests = std::iter::once(manifest_digest)
        .chain(std::iter::once(&manifest.config.digest))
        .chain(manifest.layers.iter().map(|layer| &layer.digest));
    for digest in digests {
        let committed = state.committed_leases.lock().await.remove(digest);
        if let Some(committed) = committed {
            state
                .content
                .release_lease(committed.lease)
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn header<'a>(request: &'a NatsServiceRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .as_ref()
        .and_then(|headers| headers.get(name))
        .map(|value| value.as_str())
}

fn runner_error_message(
    error: crate::roles::machine::runner::MachineContainerRunnerError,
) -> String {
    use crate::roles::machine::runner::MachineContainerRunnerError;

    match error {
        MachineContainerRunnerError::ListExisting { message }
        | MachineContainerRunnerError::EnsureEndpointNetwork { message }
        | MachineContainerRunnerError::Create { message }
        | MachineContainerRunnerError::ImagePull { message }
        | MachineContainerRunnerError::Start { message, .. }
        | MachineContainerRunnerError::Wait { message, .. }
        | MachineContainerRunnerError::Stop { message, .. }
        | MachineContainerRunnerError::Restart { message, .. }
        | MachineContainerRunnerError::Remove { message, .. }
        | MachineContainerRunnerError::RemoveVolume { message, .. } => message,
        MachineContainerRunnerError::EndpointNetworkSubnetMismatch { expected, observed } => {
            format!("endpoint network subnet is {observed:?}, expected {expected:?}")
        }
    }
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
    use super::{ensure_platform, parse_manifest, validate_chunk_bounds};
    use ployz_core::image::{ImageRpcDomainError, OciPlatform};

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

    fn platform(architecture: &str) -> OciPlatform {
        OciPlatform::try_new("linux", architecture).expect("platform")
    }
}

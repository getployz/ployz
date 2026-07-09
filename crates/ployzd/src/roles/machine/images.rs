//! Machine-local image push and inspection handlers.

use super::response::{failure_message, machine_domain_error, machine_success};
use crate::adapters::containerd_content::{ContainerdContentStore, ContentIngest, ContentLease};
use crate::adapters::docker::runner::DockerManagedContainerRunner;
use ployz_core::ids::MachineId;
use ployz_core::image::{
    IMAGE_BLOB_CHUNK_MAX_BYTES, IMAGE_BLOB_PUSH_ACTION_CHUNK, IMAGE_BLOB_PUSH_ACTION_HEADER,
    IMAGE_BLOB_PUSH_OFFSET_HEADER, IMAGE_BLOB_PUSH_UPLOAD_ID_HEADER, IMAGE_MESH_REGISTRY_PORT,
    ImageBlobCheckOk, ImageBlobCheckRequest, ImageBlobCheckResponse, ImageBlobPushOk,
    ImageBlobPushOutcome, ImageBlobPushRequest, ImageBlobPushResponse, ImageInspectOk,
    ImageInspectRequest, ImageInspectResponse, ImageManifestPushOk, ImageManifestPushRequest,
    ImageManifestPushResponse, ImageRpcDomainError, ImageUploadId, OciDigest, OciPlatform,
};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const UPLOAD_SESSION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone)]
pub(crate) enum ImageServiceState {
    Available(Box<AvailableImageService>),
    Unavailable,
}

#[derive(Clone)]
pub(crate) struct AvailableImageService {
    content: ContainerdContentStore,
    docker: DockerManagedContainerRunner,
    seed_host: Ipv4Addr,
    uploads: Arc<Mutex<BTreeMap<ImageUploadId, Arc<Mutex<UploadSession>>>>>,
    committed_leases: Arc<Mutex<BTreeMap<OciDigest, ContentLease>>>,
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
    state: ImageServiceState,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let Some(state) = available_state(machine_id.clone(), state) else {
        return unavailable(machine_id, ImageEndpoint::BlobCheck);
    };
    let request = match serde_json::from_slice::<ImageBlobCheckRequest>(&request.payload) {
        Ok(request) => request,
        Err(error) => return invalid_request(machine_id, ImageEndpoint::BlobCheck, error),
    };
    let mut present = Vec::new();
    for digest in request.digests {
        match state.content.blob_info(&digest).await {
            Ok(Some(_)) => present.push(digest),
            Ok(None) => {}
            Err(error) => {
                return image_error(
                    machine_id,
                    ImageEndpoint::BlobCheck,
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
    state: ImageServiceState,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let Some(state) = available_state(machine_id.clone(), state) else {
        return unavailable(machine_id, ImageEndpoint::BlobPush);
    };
    if header(&request, IMAGE_BLOB_PUSH_ACTION_HEADER)
        .is_some_and(|value| value == IMAGE_BLOB_PUSH_ACTION_CHUNK)
    {
        return push_chunk(machine_id, &state, request).await;
    }
    let action = match serde_json::from_slice::<ImageBlobPushRequest>(&request.payload) {
        Ok(action) => action,
        Err(error) => return invalid_request(machine_id, ImageEndpoint::BlobPush, error),
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
    let lease = match state.content.acquire_lease().await {
        Ok(lease) => lease,
        Err(error) => {
            return storage_error(machine_id, ImageEndpoint::BlobPush, error.to_string());
        }
    };
    let upload_id =
        match ImageUploadId::try_new(format!("upload_{}", nuid::next().to_ascii_lowercase())) {
            Ok(upload_id) => upload_id,
            Err(error) => {
                return storage_error(machine_id, ImageEndpoint::BlobPush, error.to_string());
            }
        };
    let ingest = ContentIngest::new(digest, total_size, lease);
    state.uploads.lock().await.insert(
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
        outcome: ImageBlobPushOutcome::Begun {
            upload_id,
            offset: 0,
        },
    }))
}

async fn push_chunk(
    machine_id: MachineId,
    state: &AvailableImageService,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    if request.payload.len() > IMAGE_BLOB_CHUNK_MAX_BYTES {
        return image_error(
            machine_id,
            ImageEndpoint::BlobPush,
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
        Err(message) => return invalid_message(machine_id, ImageEndpoint::BlobPush, message),
    };
    let offset = match header(&request, IMAGE_BLOB_PUSH_OFFSET_HEADER)
        .ok_or("missing chunk offset")
        .and_then(|value| value.parse::<u64>().map_err(|_| "invalid chunk offset"))
    {
        Ok(offset) => offset,
        Err(message) => return invalid_message(machine_id, ImageEndpoint::BlobPush, message),
    };
    let session = {
        let uploads = state.uploads.lock().await;
        uploads.get(&upload_id).cloned()
    };
    let Some(session) = session else {
        return image_error(
            machine_id,
            ImageEndpoint::BlobPush,
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
            ImageEndpoint::BlobPush,
            ImageRpcDomainError::UploadNotFound { upload_id },
        );
    }
    if offset < session.offset || session.pending.contains_key(&offset) {
        return image_error(
            machine_id,
            ImageEndpoint::BlobPush,
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
                    return storage_error(machine_id, ImageEndpoint::BlobPush, error.to_string());
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
    let next_offset = session.offset;
    machine_success(ImageBlobPushResponse::Ok(ImageBlobPushOk {
        machine_id,
        outcome: ImageBlobPushOutcome::ChunkAccepted {
            upload_id,
            next_offset,
        },
    }))
}

async fn commit_upload(
    machine_id: MachineId,
    state: &AvailableImageService,
    upload_id: ImageUploadId,
) -> NatsServiceResponse {
    let Some(session) = state.uploads.lock().await.remove(&upload_id) else {
        return image_error(
            machine_id,
            ImageEndpoint::BlobPush,
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
            ImageEndpoint::BlobPush,
            ImageRpcDomainError::UploadNotFound { upload_id },
        );
    }
    if let Err(error) = state
        .content
        .commit_ingest(&session.ingest, session.offset)
        .await
    {
        return storage_error(machine_id, ImageEndpoint::BlobPush, error.to_string());
    }
    let digest = session.ingest.digest().clone();
    let size = session.ingest.total_size();
    state
        .committed_leases
        .lock()
        .await
        .insert(digest.clone(), session.ingest.lease());
    machine_success(ImageBlobPushResponse::Ok(ImageBlobPushOk {
        machine_id,
        outcome: ImageBlobPushOutcome::Committed { digest, size },
    }))
}

pub(crate) async fn handle_image_manifest_push(
    machine_id: MachineId,
    state: ImageServiceState,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let Some(state) = available_state(machine_id.clone(), state) else {
        return unavailable(machine_id, ImageEndpoint::ManifestPush);
    };
    let request = match serde_json::from_slice::<ImageManifestPushRequest>(&request.payload) {
        Ok(request) => request,
        Err(error) => return invalid_request(machine_id, ImageEndpoint::ManifestPush, error),
    };
    let manifest = match parse_manifest(&request.manifest_bytes) {
        Ok(manifest) => manifest,
        Err(message) => return invalid_message(machine_id, ImageEndpoint::ManifestPush, message),
    };
    if let Some(response) = verify_manifest_content(&machine_id, &state, &manifest).await {
        return response;
    }
    let platform = match read_platform(&state, &manifest.config.digest).await {
        Ok(platform) => platform,
        Err(error) => return image_error(machine_id, ImageEndpoint::ManifestPush, error),
    };
    let manifest_digest = OciDigest::sha256(&request.manifest_bytes);
    let lease = match state.content.acquire_lease().await {
        Ok(lease) => lease,
        Err(error) => {
            return storage_error(machine_id, ImageEndpoint::ManifestPush, error.to_string());
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
            return storage_error(machine_id, ImageEndpoint::ManifestPush, error.to_string());
        }
    };
    if let Err(error) = state.content.commit_ingest(&ingest, offset).await {
        return storage_error(machine_id, ImageEndpoint::ManifestPush, error.to_string());
    }
    state
        .committed_leases
        .lock()
        .await
        .insert(manifest_digest.clone(), ingest.lease());
    let image_id = manifest.config.digest.clone();
    machine_success(ImageManifestPushResponse::Ok(ImageManifestPushOk {
        machine_id,
        manifest_digest,
        image_id,
        platform,
    }))
}

pub(crate) async fn handle_image_inspect(
    machine_id: MachineId,
    state: ImageServiceState,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let Some(state) = available_state(machine_id.clone(), state) else {
        return unavailable(machine_id, ImageEndpoint::Inspect);
    };
    let request = match serde_json::from_slice::<ImageInspectRequest>(&request.payload) {
        Ok(request) => request,
        Err(error) => return invalid_request(machine_id, ImageEndpoint::Inspect, error),
    };
    let bytes = match state.content.read_blob(&request.manifest_digest).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return image_error(
                machine_id,
                ImageEndpoint::Inspect,
                ImageRpcDomainError::ImageMissing {
                    digest: request.manifest_digest,
                },
            );
        }
        Err(error) => return storage_error(machine_id, ImageEndpoint::Inspect, error.to_string()),
    };
    let actual_manifest_digest = OciDigest::sha256(&bytes);
    if actual_manifest_digest != request.manifest_digest {
        return image_error(
            machine_id,
            ImageEndpoint::Inspect,
            ImageRpcDomainError::DigestMismatch {
                expected: request.manifest_digest,
                actual: actual_manifest_digest,
            },
        );
    }
    let manifest = match parse_manifest(&bytes) {
        Ok(manifest) => manifest,
        Err(message) => return invalid_message(machine_id, ImageEndpoint::Inspect, message),
    };
    if manifest.config.digest != request.image_id {
        return image_error(
            machine_id,
            ImageEndpoint::Inspect,
            ImageRpcDomainError::ConfigMismatch {
                expected: request.image_id,
                actual: manifest.config.digest,
            },
        );
    }
    let platform = match read_platform(&state, &request.image_id).await {
        Ok(platform) => platform,
        Err(error) => return image_error(machine_id, ImageEndpoint::Inspect, error),
    };
    let reference = format!(
        "{}:{}/{repository}@{manifest_digest}",
        state.seed_host,
        IMAGE_MESH_REGISTRY_PORT,
        repository = request.repository,
        manifest_digest = request.manifest_digest,
    );
    if let Err(error) = state.docker.pull_image(&reference).await {
        return image_error(
            machine_id,
            ImageEndpoint::Inspect,
            ImageRpcDomainError::SelfPullFailed {
                message: failure_message(runner_error_message(error)),
            },
        );
    }
    if let Err(error) = release_manifest_leases(&state, &manifest, &request.manifest_digest).await {
        return storage_error(machine_id, ImageEndpoint::Inspect, error);
    }
    machine_success(ImageInspectResponse::Ok(ImageInspectOk {
        machine_id,
        manifest_digest: request.manifest_digest,
        image_id: request.image_id,
        platform,
    }))
}

fn parse_manifest(bytes: &[u8]) -> Result<OciManifest, &'static str> {
    let manifest = serde_json::from_slice::<OciManifest>(bytes).map_err(|_| "invalid manifest")?;
    if manifest.schema_version != 2 {
        return Err("manifest schema version must be 2");
    }
    if manifest.media_type != "application/vnd.oci.image.manifest.v1+json"
        && manifest.media_type != "application/vnd.docker.distribution.manifest.v2+json"
    {
        return Err("unsupported image manifest media type");
    }
    if manifest.config.media_type != "application/vnd.oci.image.config.v1+json"
        && manifest.config.media_type != "application/vnd.docker.container.image.v1+json"
    {
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
                    ImageEndpoint::ManifestPush,
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
                    ImageEndpoint::ManifestPush,
                    ImageRpcDomainError::ImageMissing {
                        digest: descriptor.digest.clone(),
                    },
                ));
            }
            Err(error) => {
                return Some(storage_error(
                    machine_id.clone(),
                    ImageEndpoint::ManifestPush,
                    error.to_string(),
                ));
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
    Ok(OciPlatform { os, architecture })
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
        let lease = state.committed_leases.lock().await.remove(digest);
        if let Some(lease) = lease {
            state
                .content
                .release_lease(lease)
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
        | MachineContainerRunnerError::Stop { message, .. }
        | MachineContainerRunnerError::Restart { message, .. }
        | MachineContainerRunnerError::Remove { message, .. } => message,
    }
}

fn available_state(
    _machine_id: MachineId,
    state: ImageServiceState,
) -> Option<AvailableImageService> {
    match state {
        ImageServiceState::Available(state) => Some(*state),
        ImageServiceState::Unavailable => None,
    }
}

#[derive(Clone, Copy)]
enum ImageEndpoint {
    BlobCheck,
    BlobPush,
    ManifestPush,
    Inspect,
}

fn unavailable(machine_id: MachineId, endpoint: ImageEndpoint) -> NatsServiceResponse {
    image_error(
        machine_id,
        endpoint,
        ImageRpcDomainError::StorageFailed {
            message: failure_message("image storage is unavailable"),
        },
    )
}

fn invalid_request(
    machine_id: MachineId,
    endpoint: ImageEndpoint,
    error: serde_json::Error,
) -> NatsServiceResponse {
    invalid_message(machine_id, endpoint, &format!("invalid request: {error}"))
}

fn invalid_message(
    machine_id: MachineId,
    endpoint: ImageEndpoint,
    message: &str,
) -> NatsServiceResponse {
    image_error(
        machine_id,
        endpoint,
        ImageRpcDomainError::InvalidRequest {
            message: failure_message(message),
        },
    )
}

fn storage_error(
    machine_id: MachineId,
    endpoint: ImageEndpoint,
    message: String,
) -> NatsServiceResponse {
    image_error(
        machine_id,
        endpoint,
        ImageRpcDomainError::StorageFailed {
            message: failure_message(message),
        },
    )
}

fn image_error(
    machine_id: MachineId,
    endpoint: ImageEndpoint,
    error: ImageRpcDomainError,
) -> NatsServiceResponse {
    match endpoint {
        ImageEndpoint::BlobCheck => {
            machine_domain_error(ImageBlobCheckResponse::DomainError { machine_id, error })
        }
        ImageEndpoint::BlobPush => {
            machine_domain_error(ImageBlobPushResponse::DomainError { machine_id, error })
        }
        ImageEndpoint::ManifestPush => {
            machine_domain_error(ImageManifestPushResponse::DomainError { machine_id, error })
        }
        ImageEndpoint::Inspect => {
            machine_domain_error(ImageInspectResponse::DomainError { machine_id, error })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_manifest;

    #[test]
    fn manifest_parser_rejects_manifest_lists() {
        let manifest_list = br#"{"schemaVersion":2,"manifests":[]}"#;

        assert!(parse_manifest(manifest_list).is_err());
    }
}

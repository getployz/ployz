//! Operator-side Docker image export and machine RPC push.

use async_nats::HeaderMap;
use bollard::Docker;
use bollard::errors::Error as DockerError;
use flate2::{Compression, GzBuilder};
use futures_util::{StreamExt, TryStreamExt, stream};
use ployz_core::deploy::DeployServiceSpec;
use ployz_core::deploy::{ImageReference, ImageSource};
use ployz_core::ids::MachineId;
use ployz_core::image::{
    IMAGE_BLOB_CHUNK_MAX_BYTES, IMAGE_BLOB_PUSH_ACTION_CHUNK, IMAGE_BLOB_PUSH_ACTION_HEADER,
    IMAGE_BLOB_PUSH_OFFSET_HEADER, IMAGE_BLOB_PUSH_UPLOAD_ID_HEADER, ImageBlobCheckRequest,
    ImageBlobCheckResponse, ImageBlobPushOk, ImageBlobPushOutcome, ImageBlobPushRequest,
    ImageBlobPushResponse, ImageManifestPushOk, ImageManifestPushRequest,
    ImageManifestPushResponse, ImageRpcDomainError, ImageUploadId, OCI_IMAGE_CONFIG_MEDIA_TYPE,
    OCI_IMAGE_LAYER_GZIP_MEDIA_TYPE, OCI_IMAGE_MANIFEST_MEDIA_TYPE, OciDigest, OciPlatform,
};
use ployz_core::machine_rpc::{MachineRpcResponder, MachineRpcResponse};
use ployz_core::state::MachineLifecycle;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_nats::service_protocol::decode_nats_service_error;
use ployz_nats::service_runtime::request_json;
use ployz_sdk_types::MachineListRequest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

const PUSH_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNKS_IN_FLIGHT: usize = 8;
const LAYER_GZIP_LEVEL: u32 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePushReceipt {
    pub seed: MachineId,
    pub manifest_digest: OciDigest,
    pub config_digest: OciDigest,
    pub uploaded: BlobTransferReceipt,
    pub reused: BlobTransferReceipt,
}

impl ImagePushReceipt {
    #[must_use]
    pub fn image_source(&self) -> ImageSource {
        ImageSource::PushedToSeed {
            seed: self.seed.clone(),
            manifest_digest: self.manifest_digest.clone(),
            image_id: self.config_digest.clone(),
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "pushed {} layers, {} bytes; {} layers already on {}\n",
            self.uploaded.count(),
            self.uploaded.bytes,
            self.reused.count(),
            self.seed.as_str(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobTransferReceipt {
    pub digests: Vec<OciDigest>,
    pub bytes: u64,
}

impl BlobTransferReceipt {
    #[must_use]
    pub fn count(&self) -> usize {
        self.digests.len()
    }
}

#[derive(Debug)]
struct PreparedImage {
    manifest_bytes: Vec<u8>,
    manifest_digest: OciDigest,
    config_digest: OciDigest,
    platform: OciPlatform,
    blobs: Vec<PreparedBlob>,
}

#[derive(Debug)]
struct PreparedBlob {
    digest: OciDigest,
    bytes: Vec<u8>,
    kind: PreparedBlobKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreparedBlobKind {
    Config,
    Layer,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerSaveManifestEntry {
    config: PathBuf,
    layers: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct ImageConfigPlatform {
    architecture: String,
    os: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OciManifest<'a> {
    schema_version: u8,
    media_type: &'static str,
    config: OciDescriptor<'a>,
    layers: Vec<OciDescriptor<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OciDescriptor<'a> {
    media_type: &'static str,
    size: u64,
    digest: &'a OciDigest,
}

pub fn connect_operator_docker() -> Result<Docker, ImagePushError> {
    Docker::connect_with_defaults().map_err(|error| ImagePushError::DockerConnect {
        message: error.to_string(),
    })
}

pub async fn image_exists_locally(
    docker: &Docker,
    reference: &ImageReference,
) -> Result<bool, ImagePushError> {
    match docker.inspect_image(reference.as_str()).await {
        Ok(_) => Ok(true),
        Err(DockerError::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(false),
        Err(error) => Err(ImagePushError::DockerInspect {
            reference: reference.clone(),
            message: error.to_string(),
        }),
    }
}

pub async fn push_local_image(
    client: &async_nats::Client,
    docker: &Docker,
    seed: &MachineId,
    requested_reference: ImageReference,
) -> Result<ImagePushReceipt, ImagePushError> {
    let chunks = docker
        .export_image(requested_reference.as_str())
        .map_err(|error| ImagePushError::DockerExport {
            reference: requested_reference.clone(),
            message: error.to_string(),
        })
        .try_collect::<Vec<_>>()
        .await?;
    // ponytail: v1 buffers the Docker export in memory; stream the tar conversion
    // and RPC upload when whole-image memory becomes the limiting ceiling.
    let export_size = chunks.iter().try_fold(0_usize, |total, chunk| {
        total
            .checked_add(chunk.len())
            .ok_or(ImagePushError::ImageTooLarge)
    })?;
    let mut export = Vec::with_capacity(export_size);
    for chunk in chunks {
        export.extend_from_slice(&chunk);
    }
    let prepared = prepare_docker_save(&export)?;
    let requested_digests = prepared
        .blobs
        .iter()
        .map(|blob| blob.digest.clone())
        .collect::<Vec<_>>();
    let present = blob_check(client, seed, requested_digests).await?;
    let present = present.into_iter().collect::<BTreeSet<_>>();
    let mut uploaded = Vec::new();
    let mut reused = Vec::new();
    for blob in &prepared.blobs {
        if present.contains(&blob.digest) {
            if matches!(blob.kind, PreparedBlobKind::Layer) {
                reused.push((blob.digest.clone(), blob.bytes.len()));
            }
        } else {
            push_blob(client, seed, blob).await?;
            if matches!(blob.kind, PreparedBlobKind::Layer) {
                uploaded.push((blob.digest.clone(), blob.bytes.len()));
            }
        }
    }
    let pushed = manifest_push(
        client,
        seed,
        ImageManifestPushRequest {
            manifest_bytes: prepared.manifest_bytes,
        },
    )
    .await?;
    if pushed.manifest_digest != prepared.manifest_digest {
        return Err(ImagePushError::ManifestDigestMismatch {
            expected: prepared.manifest_digest,
            actual: pushed.manifest_digest,
        });
    }
    if pushed.image_id != prepared.config_digest {
        return Err(ImagePushError::ConfigDigestMismatch {
            expected: prepared.config_digest,
            actual: pushed.image_id,
        });
    }
    if pushed.platform != prepared.platform {
        return Err(ImagePushError::UnexpectedResponse {
            message: "seed reported a different image platform".to_owned(),
        });
    }
    Ok(ImagePushReceipt {
        seed: seed.clone(),
        manifest_digest: pushed.manifest_digest,
        config_digest: pushed.image_id,
        uploaded: transfer_receipt(uploaded)?,
        reused: transfer_receipt(reused)?,
    })
}

pub async fn prepare_deploy_images(
    api: &OperationApiClient,
    services: &mut [DeployServiceSpec],
    from_registry: bool,
) -> Result<Vec<ImagePushReceipt>, ImagePushError> {
    if from_registry {
        for service in services {
            service.image_source = ImageSource::Registry;
        }
        return Ok(Vec::new());
    }
    let docker = connect_operator_docker()?;
    let mut seed = None;
    let mut receipts = Vec::new();
    for service in services {
        if !image_exists_locally(&docker, &service.image).await? {
            service.image_source = ImageSource::Registry;
            continue;
        }
        let seed = match &mut seed {
            Some(seed) => &*seed,
            slot @ None => &*slot.insert(select_seed(api).await?),
        };
        let receipt =
            push_local_image(&api.nats_client(), &docker, seed, service.image.clone()).await?;
        service.image_source = receipt.image_source();
        receipts.push(receipt);
    }
    Ok(receipts)
}

async fn select_seed(api: &OperationApiClient) -> Result<MachineId, ImagePushError> {
    api.machine_list(&MachineListRequest {})
        .await
        .map_err(|error| ImagePushError::MachineList {
            message: error.to_string(),
        })?
        .machines
        .into_iter()
        .filter(|machine| machine.active.lifecycle == MachineLifecycle::Active)
        .map(|machine| machine.active.machine_id)
        .min()
        .ok_or(ImagePushError::NoActiveMachines)
}

fn prepare_docker_save(bytes: &[u8]) -> Result<PreparedImage, ImagePushError> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut files = BTreeMap::new();
    for entry in archive
        .entries()
        .map_err(|error| ImagePushError::InvalidDockerExport {
            message: error.to_string(),
        })?
    {
        let mut entry = entry.map_err(|error| ImagePushError::InvalidDockerExport {
            message: error.to_string(),
        })?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| ImagePushError::InvalidDockerExport {
                message: error.to_string(),
            })?
            .into_owned();
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|error| ImagePushError::InvalidDockerExport {
                message: error.to_string(),
            })?;
        files.insert(path, content);
    }
    let manifest_bytes = files
        .remove(&PathBuf::from("manifest.json"))
        .ok_or_else(|| ImagePushError::InvalidDockerExport {
            message: "manifest.json is missing".to_owned(),
        })?;
    let entries = serde_json::from_slice::<Vec<DockerSaveManifestEntry>>(&manifest_bytes).map_err(
        |error| ImagePushError::InvalidDockerExport {
            message: format!("invalid manifest.json: {error}"),
        },
    )?;
    let [entry] = entries.as_slice() else {
        return Err(ImagePushError::InvalidDockerExport {
            message: "single-image export must contain exactly one manifest entry".to_owned(),
        });
    };
    let config_bytes =
        files
            .remove(&entry.config)
            .ok_or_else(|| ImagePushError::InvalidDockerExport {
                message: format!("config {} is missing", entry.config.display()),
            })?;
    let platform =
        serde_json::from_slice::<ImageConfigPlatform>(&config_bytes).map_err(|error| {
            ImagePushError::InvalidDockerExport {
                message: format!("invalid image config: {error}"),
            }
        })?;
    let config_digest = OciDigest::sha256(&config_bytes);
    let mut blobs = vec![PreparedBlob {
        digest: config_digest.clone(),
        bytes: config_bytes,
        kind: PreparedBlobKind::Config,
    }];
    let mut layer_descriptors = Vec::new();
    for path in &entry.layers {
        let layer = files
            .remove(path)
            .ok_or_else(|| ImagePushError::InvalidDockerExport {
                message: format!("layer {} is missing", path.display()),
            })?;
        let compressed = deterministic_gzip(&layer)?;
        let digest = OciDigest::sha256(&compressed);
        layer_descriptors.push((digest.clone(), compressed.len()));
        blobs.push(PreparedBlob {
            digest,
            bytes: compressed,
            kind: PreparedBlobKind::Layer,
        });
    }
    let config_size = blobs.first().map(|blob| blob.bytes.len()).ok_or_else(|| {
        ImagePushError::InvalidDockerExport {
            message: "prepared image config is missing".to_owned(),
        }
    })?;
    let manifest = OciManifest {
        schema_version: 2,
        media_type: OCI_IMAGE_MANIFEST_MEDIA_TYPE,
        config: OciDescriptor {
            media_type: OCI_IMAGE_CONFIG_MEDIA_TYPE,
            size: u64::try_from(config_size).map_err(|_| ImagePushError::ImageTooLarge)?,
            digest: &config_digest,
        },
        layers: layer_descriptors
            .iter()
            .map(|(digest, size)| {
                Ok(OciDescriptor {
                    media_type: OCI_IMAGE_LAYER_GZIP_MEDIA_TYPE,
                    size: u64::try_from(*size).map_err(|_| ImagePushError::ImageTooLarge)?,
                    digest,
                })
            })
            .collect::<Result<Vec<_>, ImagePushError>>()?,
    };
    let manifest_bytes =
        serde_json::to_vec(&manifest).map_err(|error| ImagePushError::InvalidDockerExport {
            message: format!("encode OCI manifest: {error}"),
        })?;
    Ok(PreparedImage {
        manifest_digest: OciDigest::sha256(&manifest_bytes),
        manifest_bytes,
        config_digest,
        platform: OciPlatform {
            os: platform.os,
            architecture: platform.architecture,
        },
        blobs,
    })
}

fn deterministic_gzip(bytes: &[u8]) -> Result<Vec<u8>, ImagePushError> {
    let mut encoder = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::new(LAYER_GZIP_LEVEL));
    encoder
        .write_all(bytes)
        .map_err(|error| ImagePushError::Gzip {
            message: error.to_string(),
        })?;
    encoder.finish().map_err(|error| ImagePushError::Gzip {
        message: error.to_string(),
    })
}

async fn blob_check(
    client: &async_nats::Client,
    machine_id: &MachineId,
    digests: Vec<OciDigest>,
) -> Result<Vec<OciDigest>, ImagePushError> {
    let response = request_json::<_, ImageBlobCheckResponse>(
        client,
        machine_service(machine_id, MachineServiceEndpoint::ImageBlobCheck),
        &ImageBlobCheckRequest { digests },
        PUSH_RPC_TIMEOUT,
    )
    .await
    .map_err(|error| rpc_transport(machine_id, error.to_string()))?;
    image_response(machine_id, response).map(|ok| ok.present)
}

async fn push_blob(
    client: &async_nats::Client,
    machine_id: &MachineId,
    blob: &PreparedBlob,
) -> Result<(), ImagePushError> {
    let begun = blob_push_json(
        client,
        machine_id,
        &ImageBlobPushRequest::Begin {
            digest: blob.digest.clone(),
            total_size: u64::try_from(blob.bytes.len())
                .map_err(|_| ImagePushError::ImageTooLarge)?,
        },
    )
    .await?;
    let ImageBlobPushOutcome::Begun { upload_id } = begun.outcome else {
        return Err(ImagePushError::UnexpectedResponse {
            message: "blob begin returned the wrong outcome".to_owned(),
        });
    };
    stream::iter(blob.bytes.chunks(IMAGE_BLOB_CHUNK_MAX_BYTES).enumerate())
        .map(|(index, chunk)| {
            let upload_id = upload_id.clone();
            async move {
                let offset = index
                    .checked_mul(IMAGE_BLOB_CHUNK_MAX_BYTES)
                    .and_then(|value| u64::try_from(value).ok())
                    .ok_or(ImagePushError::ImageTooLarge)?;
                push_chunk(client, machine_id, &upload_id, offset, chunk).await
            }
        })
        .buffer_unordered(CHUNKS_IN_FLIGHT)
        .try_collect::<Vec<_>>()
        .await?;
    let committed = blob_push_json(
        client,
        machine_id,
        &ImageBlobPushRequest::Commit {
            upload_id: upload_id.clone(),
        },
    )
    .await?;
    match committed.outcome {
        ImageBlobPushOutcome::Committed { digest, size }
            if digest == blob.digest
                && size == u64::try_from(blob.bytes.len()).unwrap_or(u64::MAX) =>
        {
            Ok(())
        }
        ImageBlobPushOutcome::Begun { .. }
        | ImageBlobPushOutcome::ChunkAccepted { .. }
        | ImageBlobPushOutcome::Committed { .. } => Err(ImagePushError::UnexpectedResponse {
            message: format!("blob commit returned an unexpected outcome for {upload_id:?}"),
        }),
    }
}

async fn push_chunk(
    client: &async_nats::Client,
    machine_id: &MachineId,
    upload_id: &ImageUploadId,
    offset: u64,
    bytes: &[u8],
) -> Result<(), ImagePushError> {
    let mut headers = HeaderMap::new();
    headers.insert(IMAGE_BLOB_PUSH_ACTION_HEADER, IMAGE_BLOB_PUSH_ACTION_CHUNK);
    headers.insert(IMAGE_BLOB_PUSH_UPLOAD_ID_HEADER, upload_id.as_str());
    headers.insert(IMAGE_BLOB_PUSH_OFFSET_HEADER, offset.to_string());
    let request = async_nats::Request::new()
        .payload(bytes.to_vec().into())
        .headers(headers)
        .timeout(Some(PUSH_RPC_TIMEOUT));
    let response = client
        .send_request(
            machine_service(machine_id, MachineServiceEndpoint::ImageBlobPush),
            request,
        )
        .await
        .map_err(|error| rpc_transport(machine_id, error.to_string()))?;
    if let Some(error) = decode_nats_service_error(response.headers.as_ref())
        .map_err(|error| rpc_transport(machine_id, error.to_string()))?
    {
        return Err(rpc_transport(machine_id, error.message));
    }
    let response = serde_json::from_slice::<ImageBlobPushResponse>(&response.payload)
        .map_err(|error| rpc_transport(machine_id, error.to_string()))?;
    let ok = image_response(machine_id, response)?;
    match ok.outcome {
        ImageBlobPushOutcome::ChunkAccepted {
            upload_id: actual, ..
        } if actual == *upload_id => Ok(()),
        ImageBlobPushOutcome::Begun { .. }
        | ImageBlobPushOutcome::ChunkAccepted { .. }
        | ImageBlobPushOutcome::Committed { .. } => Err(ImagePushError::UnexpectedResponse {
            message: "blob chunk returned the wrong outcome".to_owned(),
        }),
    }
}

async fn blob_push_json(
    client: &async_nats::Client,
    machine_id: &MachineId,
    request: &ImageBlobPushRequest,
) -> Result<ImageBlobPushOk, ImagePushError> {
    let response = request_json::<_, ImageBlobPushResponse>(
        client,
        machine_service(machine_id, MachineServiceEndpoint::ImageBlobPush),
        request,
        PUSH_RPC_TIMEOUT,
    )
    .await
    .map_err(|error| rpc_transport(machine_id, error.to_string()))?;
    image_response(machine_id, response)
}

async fn manifest_push(
    client: &async_nats::Client,
    machine_id: &MachineId,
    request: ImageManifestPushRequest,
) -> Result<ImageManifestPushOk, ImagePushError> {
    let response = request_json::<_, ImageManifestPushResponse>(
        client,
        machine_service(machine_id, MachineServiceEndpoint::ImageManifestPush),
        &request,
        PUSH_RPC_TIMEOUT,
    )
    .await
    .map_err(|error| rpc_transport(machine_id, error.to_string()))?;
    image_response(machine_id, response)
}

fn image_response<T>(
    machine_id: &MachineId,
    response: MachineRpcResponse<T, ImageRpcDomainError>,
) -> Result<T, ImagePushError>
where
    T: MachineRpcResponder,
{
    match response {
        MachineRpcResponse::Ok(value) if value.responder_machine_id() == machine_id => Ok(value),
        MachineRpcResponse::Ok(value) => Err(ImagePushError::WrongResponder {
            expected: machine_id.clone(),
            actual: value.responder_machine_id().clone(),
        }),
        MachineRpcResponse::DomainError {
            machine_id: actual,
            error,
        } if actual == *machine_id => Err(ImagePushError::Domain {
            machine_id: actual,
            error,
        }),
        MachineRpcResponse::DomainError {
            machine_id: actual, ..
        } => Err(ImagePushError::WrongResponder {
            expected: machine_id.clone(),
            actual,
        }),
    }
}

fn transfer_receipt(blobs: Vec<(OciDigest, usize)>) -> Result<BlobTransferReceipt, ImagePushError> {
    let bytes = blobs.iter().try_fold(0_u64, |total, (_, size)| {
        let size = u64::try_from(*size).map_err(|_| ImagePushError::ImageTooLarge)?;
        total.checked_add(size).ok_or(ImagePushError::ImageTooLarge)
    })?;
    Ok(BlobTransferReceipt {
        digests: blobs.into_iter().map(|(digest, _)| digest).collect(),
        bytes,
    })
}

fn rpc_transport(machine_id: &MachineId, message: String) -> ImagePushError {
    ImagePushError::Rpc {
        machine_id: machine_id.clone(),
        message,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImagePushError {
    #[error("failed to connect to Docker: {message}")]
    DockerConnect { message: String },
    #[error("failed to inspect local image {}: {message}", reference.as_str())]
    DockerInspect {
        reference: ImageReference,
        message: String,
    },
    #[error("failed to export local image {}: {message}", reference.as_str())]
    DockerExport {
        reference: ImageReference,
        message: String,
    },
    #[error("failed to list seed machines: {message}")]
    MachineList { message: String },
    #[error("no active machine is available as an image seed")]
    NoActiveMachines,
    #[error("invalid Docker image export: {message}")]
    InvalidDockerExport { message: String },
    #[error("failed to gzip image layer: {message}")]
    Gzip { message: String },
    #[error("image is too large")]
    ImageTooLarge,
    #[error("image RPC to {} failed: {message}", machine_id.as_str())]
    Rpc {
        machine_id: MachineId,
        message: String,
    },
    #[error("image RPC from {} failed: {error:?}", machine_id.as_str())]
    Domain {
        machine_id: MachineId,
        error: ImageRpcDomainError,
    },
    #[error(
        "image RPC expected {}, but {} answered",
        expected.as_str(),
        actual.as_str()
    )]
    WrongResponder {
        expected: MachineId,
        actual: MachineId,
    },
    #[error("image push returned an invalid response: {message}")]
    UnexpectedResponse { message: String },
    #[error("manifest digest mismatch: expected {expected}, got {actual}")]
    ManifestDigestMismatch {
        expected: OciDigest,
        actual: OciDigest,
    },
    #[error("config digest mismatch: expected {expected}, got {actual}")]
    ConfigDigestMismatch {
        expected: OciDigest,
        actual: OciDigest,
    },
}

#[cfg(test)]
mod tests {
    use super::deterministic_gzip;
    use ployz_core::image::OciDigest;

    #[test]
    fn deterministic_gzip_has_a_stable_digest() {
        let compressed = deterministic_gzip(b"ployz direct image push\n").expect("gzip");

        assert_eq!(
            OciDigest::sha256(&compressed).as_str(),
            "sha256:63402c3de330966758f6d9c95461b607e966b827cfd73783a5c538eb5e2c235c"
        );
    }
}

//! Operator-side Docker image export and machine RPC push.

use async_nats::HeaderMap;
use bollard::Docker;
use bollard::errors::Error as DockerError;
use flate2::{Compression, GzBuilder, read::GzDecoder};
use futures_util::{StreamExt, TryStreamExt, stream};
use ployz_build_executor::ValidatedOciLayout;
use ployz_core::deploy::DeployServiceSpec;
use ployz_core::deploy::{
    ImageAvailabilityExpiresAt, ImageReference, ImageSource, PlatformImage, PushedImageReceipt,
};
use ployz_core::ids::MachineId;
use ployz_core::image::{
    IMAGE_BLOB_CHUNK_MAX_BYTES, IMAGE_BLOB_PUSH_ACTION_CHUNK, IMAGE_BLOB_PUSH_ACTION_HEADER,
    IMAGE_BLOB_PUSH_OFFSET_HEADER, IMAGE_BLOB_PUSH_UPLOAD_ID_HEADER, ImageBlobCheckOk,
    ImageBlobCheckRequest, ImageBlobCheckResponse, ImageBlobPushOk, ImageBlobPushOutcome,
    ImageBlobPushRequest, ImageBlobPushResponse, ImageContentLeaseExpiresAt, ImageManifestPushOk,
    ImageManifestPushRequest, ImageManifestPushResponse, ImageRpcDomainError, ImageUploadId,
    OCI_IMAGE_CONFIG_MEDIA_TYPE, OCI_IMAGE_LAYER_GZIP_MEDIA_TYPE, OCI_IMAGE_MANIFEST_MEDIA_TYPE,
    OciDigest, OciPlatform,
};
use ployz_core::machine::MachineLifecycle;
use ployz_core::machine::rpc::{MachineRpcResponder, MachineRpcResponse};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_nats::service_protocol::decode_nats_service_error;
use ployz_nats::service_runtime::request_json;
use ployz_nats::subjects::{MachineServiceEndpoint, machine_service};
use ployz_sdk_types::MachineListRequest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

const PUSH_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const EXECUTOR_ADMISSION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const CHUNKS_IN_FLIGHT: usize = 8;
const LAYER_GZIP_LEVEL: u32 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePushReceipt {
    receipt: PushedImageReceipt,
    pub uploaded: BlobTransferReceipt,
    pub reused: BlobTransferReceipt,
}

impl ImagePushReceipt {
    #[must_use]
    pub const fn receipt(&self) -> &PushedImageReceipt {
        &self.receipt
    }

    #[must_use]
    pub fn image_source(&self) -> ImageSource {
        ImageSource::PushedToSeed(self.receipt.clone())
    }

    #[must_use]
    pub fn render(&self) -> String {
        let Some((_, image)) = self.receipt.platforms().next() else {
            unreachable!("validated pushed image receipts contain a platform");
        };
        format!(
            "pushed {} layers, {} bytes; {} layers already on {}\n",
            self.uploaded.count(),
            self.uploaded.bytes,
            self.reused.count(),
            image.seed.as_str(),
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
    let mut uploaded = Vec::new();
    let mut reused = Vec::new();
    let mut lease_expiries = Vec::new();
    for blob in &prepared.blobs {
        let (lease_expires_at, was_reused) = push_blob(client, seed, blob).await?;
        lease_expiries.push(lease_expires_at);
        if matches!(blob.kind, PreparedBlobKind::Layer) {
            if was_reused {
                reused.push((blob.digest.clone(), blob.bytes.len()));
            } else {
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
    lease_expiries.push(pushed.lease_expires_at);
    let availability_expires_at = receipt_availability(lease_expiries)?;
    let receipt = PushedImageReceipt::try_new([(
        pushed.platform.clone(),
        PlatformImage {
            seed: seed.clone(),
            manifest_digest: pushed.manifest_digest.clone(),
            image_id: pushed.image_id.clone(),
            availability_expires_at,
        },
    )])
    .expect("a successful image push contains one platform");
    Ok(ImagePushReceipt {
        receipt,
        uploaded: transfer_receipt(uploaded)?,
        reused: transfer_receipt(reused)?,
    })
}

/// Proves that the assigned Machine image RPC is reachable and answers as
/// that exact machine before an external executor accepts build work.
pub(crate) async fn probe_image_seed(
    client: &async_nats::Client,
    seed: &MachineId,
    admission_budget: Duration,
) -> Result<(), ImagePushError> {
    let request_timeout = admission_budget.min(EXECUTOR_ADMISSION_PROBE_TIMEOUT);
    let response = request_json::<_, ImageBlobCheckResponse>(
        client,
        machine_service(seed, MachineServiceEndpoint::ImageBlobCheck),
        &ImageBlobCheckRequest {
            digests: Vec::new(),
        },
        request_timeout,
    )
    .await
    .map_err(|error| rpc_transport(seed, error.to_string()))?;
    let ImageBlobCheckOk {
        machine_id: _,
        present,
    } = image_response(seed, response)?;
    if !present.is_empty() {
        return Err(ImagePushError::UnexpectedResponse {
            message: "empty image blob check returned present digests".to_owned(),
        });
    }
    Ok(())
}

pub(crate) async fn push_validated_oci_layout(
    client: &async_nats::Client,
    layout: &ValidatedOciLayout,
    platform: &OciPlatform,
    seed: &MachineId,
) -> Result<ImagePushReceipt, ImagePushError> {
    let blobs = classify_layout_blobs(layout)?;
    let manifest_bytes = read_layout_blob(blobs.manifest).await?;
    let mut uploaded = Vec::new();
    let mut reused = Vec::new();
    let mut lease_expiries = Vec::new();

    for blob in std::iter::once(blobs.config).chain(blobs.layers.iter().copied()) {
        let (lease_expires_at, was_reused) = push_layout_blob(client, seed, blob).await?;
        lease_expiries.push(lease_expires_at);
        if blob.digest != layout.image_id() {
            let receipt = (blob.digest.clone(), blob.size);
            if was_reused {
                reused.push(receipt);
            } else {
                uploaded.push(receipt);
            }
        }
    }

    let pushed = manifest_push(client, seed, ImageManifestPushRequest { manifest_bytes }).await?;
    if pushed.manifest_digest != *layout.manifest_digest() {
        return Err(ImagePushError::ManifestDigestMismatch {
            expected: layout.manifest_digest().clone(),
            actual: pushed.manifest_digest,
        });
    }
    if pushed.image_id != *layout.image_id() {
        return Err(ImagePushError::ConfigDigestMismatch {
            expected: layout.image_id().clone(),
            actual: pushed.image_id,
        });
    }
    if pushed.platform != *platform {
        return Err(ImagePushError::UnexpectedResponse {
            message: "seed reported a different image platform".to_owned(),
        });
    }
    lease_expiries.push(pushed.lease_expires_at);
    layout_push_receipt(
        platform,
        seed,
        layout.manifest_digest(),
        layout.image_id(),
        lease_expiries,
        uploaded,
        reused,
    )
}

fn layout_push_receipt(
    platform: &OciPlatform,
    seed: &MachineId,
    manifest_digest: &OciDigest,
    image_id: &OciDigest,
    lease_expiries: impl IntoIterator<Item = ImageContentLeaseExpiresAt>,
    uploaded: Vec<(OciDigest, u64)>,
    reused: Vec<(OciDigest, u64)>,
) -> Result<ImagePushReceipt, ImagePushError> {
    let availability_expires_at = receipt_availability(lease_expiries)?;
    let receipt = PushedImageReceipt::try_new([(
        platform.clone(),
        PlatformImage {
            seed: seed.clone(),
            manifest_digest: manifest_digest.clone(),
            image_id: image_id.clone(),
            availability_expires_at,
        },
    )])
    .map_err(|error| ImagePushError::UnexpectedResponse {
        message: error.to_string(),
    })?;

    Ok(ImagePushReceipt {
        receipt,
        uploaded: transfer_receipt_sizes(uploaded)?,
        reused: transfer_receipt_sizes(reused)?,
    })
}

struct LayoutBlobs<'a> {
    manifest: LayoutBlob<'a>,
    config: LayoutBlob<'a>,
    layers: Vec<LayoutBlob<'a>>,
}

fn classify_layout_blobs(layout: &ValidatedOciLayout) -> Result<LayoutBlobs<'_>, ImagePushError> {
    let blobs = layout
        .blobs()
        .iter()
        .map(|blob| LayoutBlob {
            path: blob.path(),
            digest: blob.digest(),
            size: blob.size(),
        })
        .collect::<Vec<_>>();
    classify_blob_metadata(layout.manifest_digest(), layout.image_id(), &blobs)
}

fn classify_blob_metadata<'a>(
    manifest_digest: &OciDigest,
    image_id: &OciDigest,
    blobs: &[LayoutBlob<'a>],
) -> Result<LayoutBlobs<'a>, ImagePushError> {
    if manifest_digest == image_id {
        return Err(ImagePushError::InvalidOciLayout {
            message: "manifest and config must have different digests".to_owned(),
        });
    }
    let manifest = blobs
        .iter()
        .find(|blob| blob.digest == manifest_digest)
        .copied()
        .ok_or_else(|| ImagePushError::InvalidOciLayout {
            message: format!("manifest blob {manifest_digest} is missing"),
        })?;
    let config = blobs
        .iter()
        .find(|blob| blob.digest == image_id)
        .copied()
        .ok_or_else(|| ImagePushError::InvalidOciLayout {
            message: format!("config blob {image_id} is missing"),
        })?;
    let layers = blobs
        .iter()
        .filter(|blob| blob.digest != manifest_digest && blob.digest != image_id)
        .copied()
        .collect();
    Ok(LayoutBlobs {
        manifest,
        config,
        layers,
    })
}

#[derive(Clone, Copy)]
struct LayoutBlob<'a> {
    path: &'a Path,
    digest: &'a OciDigest,
    size: u64,
}

struct ValidatedBlobReader<'a> {
    blob: LayoutBlob<'a>,
    file: File,
    offset: u64,
    hasher: Sha256,
}

impl<'a> ValidatedBlobReader<'a> {
    async fn open(blob: LayoutBlob<'a>) -> Result<Self, ImagePushError> {
        let file = File::open(blob.path)
            .await
            .map_err(|error| blob_read_error(blob.path, blob.digest, error))?;
        let actual = file
            .metadata()
            .await
            .map_err(|error| blob_read_error(blob.path, blob.digest, error))?
            .len();
        if actual != blob.size {
            return Err(ImagePushError::OciBlobSizeDrift {
                digest: blob.digest.clone(),
                expected: blob.size,
                actual,
            });
        }
        Ok(Self {
            blob,
            file,
            offset: 0,
            hasher: Sha256::new(),
        })
    }

    async fn next_chunk(&mut self) -> Result<Option<(u64, Vec<u8>)>, ImagePushError> {
        if self.offset == self.blob.size {
            let mut extra = [0_u8; 1];
            let read = self
                .file
                .read(&mut extra)
                .await
                .map_err(|error| blob_read_error(self.blob.path, self.blob.digest, error))?;
            if read != 0 {
                return Err(ImagePushError::OciBlobSizeDrift {
                    digest: self.blob.digest.clone(),
                    expected: self.blob.size,
                    actual: self.blob.size.saturating_add(1),
                });
            }
            let actual = OciDigest::try_new(format!("sha256:{:x}", self.hasher.clone().finalize()))
                .map_err(|error| ImagePushError::InvalidOciLayout {
                    message: error.to_string(),
                })?;
            if actual != *self.blob.digest {
                return Err(ImagePushError::OciBlobDigestMismatch {
                    expected: self.blob.digest.clone(),
                    actual,
                });
            }
            return Ok(None);
        }

        let remaining = self.blob.size - self.offset;
        let chunk_len = usize::try_from(remaining.min(IMAGE_BLOB_CHUNK_MAX_BYTES as u64))
            .map_err(|_| ImagePushError::ImageTooLarge)?;
        let mut bytes = vec![0_u8; chunk_len];
        let read = self
            .file
            .read(&mut bytes)
            .await
            .map_err(|error| blob_read_error(self.blob.path, self.blob.digest, error))?;
        if read == 0 {
            return Err(ImagePushError::OciBlobSizeDrift {
                digest: self.blob.digest.clone(),
                expected: self.blob.size,
                actual: self.offset,
            });
        }
        bytes.truncate(read);
        let offset = self.offset;
        self.offset = self
            .offset
            .checked_add(u64::try_from(read).map_err(|_| ImagePushError::ImageTooLarge)?)
            .ok_or(ImagePushError::ImageTooLarge)?;
        self.hasher.update(&bytes);
        Ok(Some((offset, bytes)))
    }
}

async fn read_layout_blob(blob: LayoutBlob<'_>) -> Result<Vec<u8>, ImagePushError> {
    let mut reader = ValidatedBlobReader::open(blob).await?;
    let mut bytes =
        Vec::with_capacity(usize::try_from(blob.size).map_err(|_| ImagePushError::ImageTooLarge)?);
    while let Some((_, chunk)) = reader.next_chunk().await? {
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn push_layout_blob(
    client: &async_nats::Client,
    machine_id: &MachineId,
    blob: LayoutBlob<'_>,
) -> Result<(ImageContentLeaseExpiresAt, bool), ImagePushError> {
    let mut reader = ValidatedBlobReader::open(blob).await?;
    let begun = blob_push_json(
        client,
        machine_id,
        &ImageBlobPushRequest::Begin {
            digest: blob.digest.clone(),
            total_size: blob.size,
        },
    )
    .await?;
    let upload_id = match begun.outcome {
        ImageBlobPushOutcome::Retained {
            digest,
            size,
            lease_expires_at,
        } if digest == *blob.digest && size == blob.size => {
            while reader.next_chunk().await?.is_some() {}
            return Ok((lease_expires_at, true));
        }
        ImageBlobPushOutcome::Begun { upload_id } => upload_id,
        ImageBlobPushOutcome::Retained { .. }
        | ImageBlobPushOutcome::ChunkAccepted { .. }
        | ImageBlobPushOutcome::Committed { .. } => {
            return Err(ImagePushError::UnexpectedResponse {
                message: "blob begin returned the wrong outcome".to_owned(),
            });
        }
    };
    while let Some((offset, chunk)) = reader.next_chunk().await? {
        push_chunk(client, machine_id, &upload_id, offset, &chunk).await?;
    }
    let committed = blob_push_json(
        client,
        machine_id,
        &ImageBlobPushRequest::Commit {
            upload_id: upload_id.clone(),
        },
    )
    .await?;
    match committed.outcome {
        ImageBlobPushOutcome::Committed {
            digest,
            size,
            lease_expires_at,
        } if digest == *blob.digest && size == blob.size => Ok((lease_expires_at, false)),
        ImageBlobPushOutcome::Begun { .. }
        | ImageBlobPushOutcome::Retained { .. }
        | ImageBlobPushOutcome::ChunkAccepted { .. }
        | ImageBlobPushOutcome::Committed { .. } => Err(ImagePushError::UnexpectedResponse {
            message: format!("blob commit returned an unexpected outcome for {upload_id:?}"),
        }),
    }
}

fn receipt_availability(
    lease_expiries: impl IntoIterator<Item = ImageContentLeaseExpiresAt>,
) -> Result<ImageAvailabilityExpiresAt, ImagePushError> {
    let Some(lease_expires_at) = lease_expiries.into_iter().min() else {
        return Err(ImagePushError::UnexpectedResponse {
            message: "image push reported no content lease deadlines".to_owned(),
        });
    };
    ImageAvailabilityExpiresAt::from_content_lease_expiry(lease_expires_at).map_err(|error| {
        ImagePushError::UnexpectedResponse {
            message: format!("invalid image availability deadline: {error}"),
        }
    })
}

pub async fn prepare_deploy_images(
    api: &OperationApiClient,
    services: &mut [DeployServiceSpec],
    from_registry: bool,
) -> Result<Vec<ImagePushReceipt>, ImagePushError> {
    if from_registry {
        return Ok(Vec::new());
    }
    let docker = connect_operator_docker()?;
    let mut seed = None;
    let mut receipts = Vec::new();
    for service in services {
        if matches!(service.image_source, ImageSource::PushedToSeed(_)) {
            continue;
        }
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
        service.image = service
            .image
            .with_digest(receipt.receipt.index_digest())
            .map_err(|error| ImagePushError::UnexpectedResponse {
                message: error.to_string(),
            })?;
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
        let compressed = prepare_layer(layer)?;
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
        platform: OciPlatform::try_new(platform.os, platform.architecture).map_err(|error| {
            ImagePushError::InvalidDockerExport {
                message: format!("invalid image platform: {error}"),
            }
        })?,
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

fn prepare_layer(layer: Vec<u8>) -> Result<Vec<u8>, ImagePushError> {
    if !layer.starts_with(&[0x1f, 0x8b]) {
        return deterministic_gzip(&layer);
    }
    std::io::copy(&mut GzDecoder::new(layer.as_slice()), &mut std::io::sink()).map_err(
        |error| ImagePushError::InvalidDockerExport {
            message: format!("invalid gzip layer: {error}"),
        },
    )?;
    Ok(layer)
}

async fn push_blob(
    client: &async_nats::Client,
    machine_id: &MachineId,
    blob: &PreparedBlob,
) -> Result<(ImageContentLeaseExpiresAt, bool), ImagePushError> {
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
    let upload_id = match begun.outcome {
        ImageBlobPushOutcome::Retained {
            digest,
            size,
            lease_expires_at,
        } if digest == blob.digest
            && size == u64::try_from(blob.bytes.len()).unwrap_or(u64::MAX) =>
        {
            return Ok((lease_expires_at, true));
        }
        ImageBlobPushOutcome::Begun { upload_id } => upload_id,
        ImageBlobPushOutcome::Retained { .. }
        | ImageBlobPushOutcome::ChunkAccepted { .. }
        | ImageBlobPushOutcome::Committed { .. } => {
            return Err(ImagePushError::UnexpectedResponse {
                message: "blob begin returned the wrong outcome".to_owned(),
            });
        }
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
        ImageBlobPushOutcome::Committed {
            digest,
            size,
            lease_expires_at,
        } if digest == blob.digest
            && size == u64::try_from(blob.bytes.len()).unwrap_or(u64::MAX) =>
        {
            Ok((lease_expires_at, false))
        }
        ImageBlobPushOutcome::Begun { .. }
        | ImageBlobPushOutcome::Retained { .. }
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
        | ImageBlobPushOutcome::Retained { .. }
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
            error: Box::new(error),
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

fn transfer_receipt_sizes(
    blobs: Vec<(OciDigest, u64)>,
) -> Result<BlobTransferReceipt, ImagePushError> {
    let bytes = blobs.iter().try_fold(0_u64, |total, (_, size)| {
        total
            .checked_add(*size)
            .ok_or(ImagePushError::ImageTooLarge)
    })?;
    Ok(BlobTransferReceipt {
        digests: blobs.into_iter().map(|(digest, _)| digest).collect(),
        bytes,
    })
}

fn blob_read_error(path: &Path, digest: &OciDigest, error: std::io::Error) -> ImagePushError {
    ImagePushError::OciBlobRead {
        path: path.to_path_buf(),
        digest: digest.clone(),
        message: error.to_string(),
    }
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
    #[error("invalid validated OCI layout: {message}")]
    InvalidOciLayout { message: String },
    #[error("failed to read OCI blob {digest} at {}: {message}", path.display())]
    OciBlobRead {
        path: PathBuf,
        digest: OciDigest,
        message: String,
    },
    #[error("OCI blob {digest} size changed: expected {expected}, got {actual}")]
    OciBlobSizeDrift {
        digest: OciDigest,
        expected: u64,
        actual: u64,
    },
    #[error("OCI blob digest changed: expected {expected}, got {actual}")]
    OciBlobDigestMismatch {
        expected: OciDigest,
        actual: OciDigest,
    },
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
        error: Box<ImageRpcDomainError>,
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
    use super::{
        LayoutBlob, PreparedBlobKind, ValidatedBlobReader, classify_blob_metadata,
        deterministic_gzip, layout_push_receipt, prepare_docker_save, probe_image_seed,
        receipt_availability,
    };
    use ployz_core::deploy::{PlatformImage, PushedImageReceipt};
    use ployz_core::ids::MachineId;
    use ployz_core::image::{
        IMAGE_BLOB_CHUNK_MAX_BYTES, ImageBlobCheckOk, ImageBlobCheckRequest,
        ImageBlobCheckResponse, ImageContentLeaseExpiresAt, OciDigest, OciPlatform,
    };
    use ployz_core::machine::rpc::MachineRpcResponse;
    use ployz_nats::subjects::{MachineServiceEndpoint, machine_service};
    use std::io::Cursor;
    use std::time::Duration;

    fn digest(byte: char) -> OciDigest {
        OciDigest::try_new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    #[tokio::test]
    async fn executor_admission_seed_probe_rejects_a_wrong_responder() {
        let expected = MachineId::try_new("machine_expected").expect("machine");
        let actual = MachineId::try_new("machine_other").expect("machine");
        let nats = ployz_test_support::nats::TestNats::start_with_machines(std::slice::from_ref(
            &expected,
        ))
        .await;
        let machine = nats.machine_client(&expected).await;
        let subject = machine_service(&expected, MachineServiceEndpoint::ImageBlobCheck);
        let mut requests = machine.subscribe(subject).await.expect("subscribe");
        machine.flush().await.expect("flush");
        let responder = machine.clone();
        let task = tokio::spawn(async move {
            use futures_util::StreamExt;
            let request = requests.next().await.expect("request");
            let decoded: ImageBlobCheckRequest =
                serde_json::from_slice(&request.payload).expect("decode");
            assert!(decoded.digests.is_empty());
            let reply = request.reply.expect("request has reply");
            let response: ImageBlobCheckResponse = MachineRpcResponse::Ok(ImageBlobCheckOk {
                machine_id: actual,
                present: Vec::new(),
            });
            responder
                .publish(reply, serde_json::to_vec(&response).expect("encode").into())
                .await
                .expect("respond");
        });

        let error = probe_image_seed(&nats.user, &expected, Duration::from_secs(1))
            .await
            .expect_err("wrong responder rejected");
        assert!(matches!(
            error,
            super::ImagePushError::WrongResponder { .. }
        ));
        task.await.expect("responder");
    }

    #[test]
    fn deterministic_gzip_has_a_stable_digest() {
        let compressed = deterministic_gzip(b"ployz direct image push\n").expect("gzip");

        assert_eq!(
            OciDigest::sha256(&compressed).as_str(),
            "sha256:63402c3de330966758f6d9c95461b607e966b827cfd73783a5c538eb5e2c235c"
        );
    }

    #[test]
    fn receipt_availability_uses_the_earliest_machine_lease_deadline() {
        let availability = receipt_availability([
            ImageContentLeaseExpiresAt::try_new(4_200).expect("lease"),
            ImageContentLeaseExpiresAt::try_new(4_000).expect("lease"),
            ImageContentLeaseExpiresAt::try_new(4_100).expect("lease"),
        ])
        .expect("availability");

        assert_eq!(availability.unix_seconds(), 3_700);
    }

    #[test]
    fn single_platform_receipt_index_has_a_stable_digest() {
        let manifest =
            OciDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("manifest digest");
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");

        assert_eq!(
            PushedImageReceipt::try_new([(
                platform,
                PlatformImage {
                    seed: MachineId::try_new("machine_seed").expect("machine id"),
                    manifest_digest: manifest.clone(),
                    image_id: manifest,
                    availability_expires_at:
                        ployz_core::deploy::ImageAvailabilityExpiresAt::try_new(4_102_444_800)
                            .expect("expiry"),
                },
            )])
            .expect("receipt")
            .index_digest()
            .as_str(),
            "sha256:d5a3f65cf1cb1fc648fc9a13993161ff70a76ad60198da84fba7c17c3a8f6a1f"
        );
    }

    #[test]
    fn validated_layout_classifies_manifest_config_and_layers() {
        let manifest = digest('a');
        let config = digest('b');
        let layer = digest('c');
        let manifest_path = std::path::PathBuf::from("manifest");
        let config_path = std::path::PathBuf::from("config");
        let layer_path = std::path::PathBuf::from("layer");
        let blobs = [
            LayoutBlob {
                path: &manifest_path,
                digest: &manifest,
                size: 10,
            },
            LayoutBlob {
                path: &config_path,
                digest: &config,
                size: 20,
            },
            LayoutBlob {
                path: &layer_path,
                digest: &layer,
                size: 30,
            },
        ];

        let classified =
            classify_blob_metadata(&manifest, &config, &blobs).expect("classify layout");

        assert_eq!(classified.manifest.digest, &manifest);
        assert_eq!(classified.config.digest, &config);
        let [classified_layer] = classified.layers.as_slice() else {
            panic!("fixture has one layer");
        };
        assert_eq!(classified_layer.digest, &layer);
        assert!(classify_blob_metadata(&digest('d'), &config, &blobs).is_err());
        assert!(classify_blob_metadata(&manifest, &digest('d'), &blobs).is_err());
    }

    #[tokio::test]
    async fn validated_blob_reader_streams_bounded_offsets_and_checks_digest() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("blob");
        let bytes = vec![7_u8; IMAGE_BLOB_CHUNK_MAX_BYTES + 17];
        tokio::fs::write(&path, &bytes).await.expect("write blob");
        let digest = OciDigest::sha256(&bytes);
        let blob = LayoutBlob {
            path: &path,
            digest: &digest,
            size: u64::try_from(bytes.len()).expect("size"),
        };
        let mut reader = ValidatedBlobReader::open(blob).await.expect("open blob");
        let mut chunks = Vec::new();
        while let Some((offset, chunk)) = reader.next_chunk().await.expect("read chunk") {
            chunks.push((offset, chunk.len()));
        }

        assert_eq!(
            chunks,
            vec![
                (0, IMAGE_BLOB_CHUNK_MAX_BYTES),
                (IMAGE_BLOB_CHUNK_MAX_BYTES as u64, 17)
            ]
        );
    }

    #[tokio::test]
    async fn validated_blob_reader_rejects_size_drift() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("blob");
        tokio::fs::write(&path, b"short").await.expect("write blob");
        let digest = OciDigest::sha256(b"short");
        let result = ValidatedBlobReader::open(LayoutBlob {
            path: &path,
            digest: &digest,
            size: 6,
        })
        .await;
        let Err(error) = result else {
            panic!("size drift must fail");
        };

        assert!(matches!(
            error,
            super::ImagePushError::OciBlobSizeDrift { .. }
        ));
    }

    #[tokio::test]
    async fn validated_blob_reader_rejects_growth_after_open() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("blob");
        tokio::fs::write(&path, b"start").await.expect("write blob");
        let digest = OciDigest::sha256(b"start");
        let mut reader = ValidatedBlobReader::open(LayoutBlob {
            path: &path,
            digest: &digest,
            size: 5,
        })
        .await
        .expect("open blob");
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open writer");
        tokio::io::AsyncWriteExt::write_all(&mut file, b"!")
            .await
            .expect("grow blob");

        assert!(reader.next_chunk().await.expect("original chunk").is_some());
        let error = reader
            .next_chunk()
            .await
            .expect_err("growth after open must fail");
        assert!(matches!(
            error,
            super::ImagePushError::OciBlobSizeDrift { .. }
        ));
    }

    #[tokio::test]
    async fn validated_blob_reader_rejects_digest_drift() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("blob");
        tokio::fs::write(&path, b"changed")
            .await
            .expect("write blob");
        let expected = OciDigest::sha256(b"initial");
        let mut reader = ValidatedBlobReader::open(LayoutBlob {
            path: &path,
            digest: &expected,
            size: 7,
        })
        .await
        .expect("open same-sized blob");
        let error = loop {
            match reader.next_chunk().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("digest drift must fail"),
                Err(error) => break error,
            }
        };

        assert!(matches!(
            error,
            super::ImagePushError::OciBlobDigestMismatch { .. }
        ));
    }

    #[test]
    fn layout_receipt_keeps_layer_accounting_and_earliest_lease() {
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        let seed = MachineId::try_new("machine_seed").expect("machine id");
        let manifest = digest('a');
        let config = digest('b');
        let uploaded = digest('c');
        let reused = digest('d');

        let pushed = layout_push_receipt(
            &platform,
            &seed,
            &manifest,
            &config,
            [
                ImageContentLeaseExpiresAt::try_new(4_200).expect("lease"),
                ImageContentLeaseExpiresAt::try_new(4_000).expect("lease"),
                ImageContentLeaseExpiresAt::try_new(4_100).expect("lease"),
            ],
            vec![(uploaded.clone(), 11)],
            vec![(reused.clone(), 13)],
        )
        .expect("receipt");

        let (_, image) = pushed
            .receipt()
            .platforms()
            .next()
            .expect("platform receipt");
        assert_eq!(image.seed, seed);
        assert_eq!(image.manifest_digest, manifest);
        assert_eq!(image.image_id, config);
        assert_eq!(image.availability_expires_at.unix_seconds(), 3_700);
        assert_eq!(pushed.uploaded.digests, vec![uploaded]);
        assert_eq!(pushed.uploaded.bytes, 11);
        assert_eq!(pushed.reused.digests, vec![reused]);
        assert_eq!(pushed.reused.bytes, 13);
    }

    #[test]
    fn docker_save_gzip_layer_is_not_compressed_twice() {
        let layer = deterministic_gzip(b"already compressed layer").expect("gzip layer");
        let docker_save = docker_save_with_layer(&layer);

        let prepared = prepare_docker_save(&docker_save).expect("prepare Docker save");
        let prepared_layers = prepared
            .blobs
            .iter()
            .filter(|blob| matches!(blob.kind, PreparedBlobKind::Layer))
            .collect::<Vec<_>>();
        let [prepared_layer] = prepared_layers.as_slice() else {
            panic!("fixture produces one prepared layer");
        };

        assert_eq!(prepared_layer.bytes, layer);
    }

    #[test]
    fn docker_save_truncated_gzip_layer_is_rejected() {
        let mut layer = deterministic_gzip(b"truncated layer").expect("gzip layer");
        layer.pop();

        let error = prepare_docker_save(&docker_save_with_layer(&layer))
            .expect_err("truncated gzip layer must fail");

        assert!(
            matches!(error, super::ImagePushError::InvalidDockerExport { message } if message.starts_with("invalid gzip layer:"))
        );
    }

    fn docker_save_with_layer(layer: &[u8]) -> Vec<u8> {
        let config = br#"{"architecture":"amd64","os":"linux"}"#;
        let manifest =
            br#"[{"Config":"config.json","RepoTags":["app:test"],"Layers":["layer.tar"]}]"#;
        let mut docker_save = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut docker_save);
            for (path, bytes) in [
                ("manifest.json", manifest.as_slice()),
                ("config.json", config.as_slice()),
                ("layer.tar", layer),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(u64::try_from(bytes.len()).expect("fixture size"));
                header.set_mode(0o644);
                header.set_cksum();
                archive
                    .append_data(&mut header, path, Cursor::new(bytes))
                    .expect("append fixture file");
            }
            archive.finish().expect("finish fixture archive");
        }
        docker_save
    }
}

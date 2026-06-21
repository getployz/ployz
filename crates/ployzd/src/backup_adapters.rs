//! Backup target adapters owned by `ployzd`.

use aws_config::{BehaviorVersion, timeout::TimeoutConfig};
use aws_sdk_s3::{
    Client as S3Client,
    config::{Builder as S3ConfigBuilder, Region},
    primitives::ByteStream,
};
use futures_util::future::BoxFuture;
use ployz_core::backup::{
    BackupArtifact, BackupArtifactKind, BackupArtifactLocation, BackupManifest,
    BackupRestoreSource, BackupTarget, S3AddressingStyle,
};
use ployz_core::ids::OperationId;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CONTROL_PLANE_BUNDLE_FILE: &str = "control-plane-bundle.json";
const MANIFEST_FILE: &str = "manifest.json";
const S3_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const S3_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct BackupAdapterRegistry {
    adapter: Arc<dyn BackupTargetAdapter>,
}

impl BackupAdapterRegistry {
    #[must_use]
    pub fn s3_default() -> Self {
        Self {
            adapter: Arc::new(S3BackupAdapter::default()),
        }
    }

    #[must_use]
    pub fn new(adapter: impl BackupTargetAdapter + 'static) -> Self {
        Self {
            adapter: Arc::new(adapter),
        }
    }

    pub async fn write_control_plane_bundle(
        &self,
        operation_id: &OperationId,
        target: &BackupTarget,
        payload: &[u8],
    ) -> Result<BackupArtifact, BackupAdapterError> {
        self.adapter
            .write_control_plane_bundle(operation_id, target, payload)
            .await
    }

    pub async fn write_manifest(
        &self,
        operation_id: &OperationId,
        target: &BackupTarget,
        manifest: &BackupManifest,
    ) -> Result<(), BackupAdapterError> {
        self.adapter
            .write_manifest(operation_id, target, manifest)
            .await
    }

    pub async fn read_manifest(
        &self,
        source: &BackupRestoreSource,
    ) -> Result<BackupManifest, BackupAdapterError> {
        self.adapter.read_manifest(source).await
    }

    pub async fn read_artifact(
        &self,
        artifact: &BackupArtifact,
    ) -> Result<Vec<u8>, BackupAdapterError> {
        self.adapter.read_artifact(artifact).await
    }
}

pub trait BackupTargetAdapter: Send + Sync {
    fn write_control_plane_bundle<'a>(
        &'a self,
        operation_id: &'a OperationId,
        target: &'a BackupTarget,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<BackupArtifact, BackupAdapterError>>;

    fn write_manifest<'a>(
        &'a self,
        operation_id: &'a OperationId,
        target: &'a BackupTarget,
        manifest: &'a BackupManifest,
    ) -> BoxFuture<'a, Result<(), BackupAdapterError>>;

    fn read_manifest<'a>(
        &'a self,
        source: &'a BackupRestoreSource,
    ) -> BoxFuture<'a, Result<BackupManifest, BackupAdapterError>>;

    fn read_artifact<'a>(
        &'a self,
        artifact: &'a BackupArtifact,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackupAdapterError>>;
}

#[derive(Debug, Clone)]
pub struct S3BackupAdapter {
    operation_timeout: Duration,
    operation_attempt_timeout: Duration,
}

impl Default for S3BackupAdapter {
    fn default() -> Self {
        Self {
            operation_timeout: S3_OPERATION_TIMEOUT,
            operation_attempt_timeout: S3_ATTEMPT_TIMEOUT,
        }
    }
}

impl S3BackupAdapter {
    #[must_use]
    pub const fn new(operation_timeout: Duration, operation_attempt_timeout: Duration) -> Self {
        Self {
            operation_timeout,
            operation_attempt_timeout,
        }
    }

    async fn client(
        &self,
        region: &str,
        endpoint_url: Option<&str>,
        addressing_style: S3AddressingStyle,
    ) -> S3Client {
        let timeout_config = TimeoutConfig::builder()
            .operation_timeout(self.operation_timeout)
            .operation_attempt_timeout(self.operation_attempt_timeout)
            .build();
        let config = aws_config::defaults(BehaviorVersion::latest())
            .timeout_config(timeout_config)
            .load()
            .await;
        let mut builder = S3ConfigBuilder::from(&config).region(Region::new(region.to_owned()));
        if let Some(endpoint_url) = endpoint_url {
            builder = builder.endpoint_url(endpoint_url);
        }
        if addressing_style == S3AddressingStyle::Path {
            builder = builder.force_path_style(true);
        }
        S3Client::from_conf(builder.build())
    }
}

impl BackupTargetAdapter for S3BackupAdapter {
    fn write_control_plane_bundle<'a>(
        &'a self,
        operation_id: &'a OperationId,
        target: &'a BackupTarget,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<BackupArtifact, BackupAdapterError>> {
        Box::pin(async move {
            validate_backup_target(target)?;
            let S3TargetParts {
                bucket,
                key_prefix,
                region,
                endpoint_url,
                addressing_style,
            } = s3_target_parts(target)?;
            let key = backup_object_key(key_prefix, operation_id, CONTROL_PLANE_BUNDLE_FILE);
            let client = self
                .client(region, endpoint_url.as_deref(), *addressing_style)
                .await;
            put_object_non_destructive(&client, bucket, &key, payload).await?;

            Ok(BackupArtifact::new(
                BackupArtifactLocation::s3(
                    bucket.clone(),
                    key,
                    region.clone(),
                    endpoint_url.clone(),
                    *addressing_style,
                ),
                BackupArtifactKind::ControlPlaneBundle,
                payload.len() as u64,
                sha256_digest(payload),
            ))
        })
    }

    fn write_manifest<'a>(
        &'a self,
        operation_id: &'a OperationId,
        target: &'a BackupTarget,
        manifest: &'a BackupManifest,
    ) -> BoxFuture<'a, Result<(), BackupAdapterError>> {
        Box::pin(async move {
            validate_backup_target(target)?;
            let S3TargetParts {
                bucket,
                key_prefix,
                region,
                endpoint_url,
                addressing_style,
            } = s3_target_parts(target)?;
            let key = backup_object_key(key_prefix, operation_id, MANIFEST_FILE);
            let payload = serde_json::to_vec(manifest).map_err(|error| {
                BackupAdapterError::EncodeManifest {
                    message: error.to_string(),
                }
            })?;
            let client = self
                .client(region, endpoint_url.as_deref(), *addressing_style)
                .await;
            put_object_non_destructive(&client, bucket, &key, &payload).await
        })
    }

    fn read_manifest<'a>(
        &'a self,
        source: &'a BackupRestoreSource,
    ) -> BoxFuture<'a, Result<BackupManifest, BackupAdapterError>> {
        Box::pin(async move {
            let BackupRestoreSource::S3 {
                bucket,
                manifest_key,
                region,
                endpoint_url,
                addressing_style,
            } = source;
            validate_s3_restore_source(bucket, manifest_key, region)?;
            let client = self
                .client(region, endpoint_url.as_deref(), *addressing_style)
                .await;
            let payload = get_object_payload(&client, bucket, manifest_key).await?;
            serde_json::from_slice(&payload).map_err(|error| BackupAdapterError::DecodeManifest {
                message: error.to_string(),
            })
        })
    }

    fn read_artifact<'a>(
        &'a self,
        artifact: &'a BackupArtifact,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackupAdapterError>> {
        Box::pin(async move {
            let BackupArtifactLocation::S3 {
                bucket,
                key,
                region,
                endpoint_url,
                addressing_style,
            } = &artifact.location;
            if artifact.kind != BackupArtifactKind::ControlPlaneBundle {
                return Err(BackupAdapterError::UnsupportedArtifactKind {
                    kind: artifact.kind,
                });
            }
            validate_s3_restore_source(bucket, key, region)?;
            let client = self
                .client(region, endpoint_url.as_deref(), *addressing_style)
                .await;
            let payload = get_object_payload(&client, bucket, key).await?;
            verify_payload(artifact, &payload)?;
            Ok(payload)
        })
    }
}

#[derive(Clone, Default)]
pub struct InMemoryBackupAdapter {
    state: Arc<Mutex<InMemoryBackupState>>,
}

impl InMemoryBackupAdapter {
    #[must_use]
    pub fn registry(&self) -> BackupAdapterRegistry {
        BackupAdapterRegistry::new(self.clone())
    }

    #[must_use]
    pub fn object(&self, key: &str) -> Option<Vec<u8>> {
        let state = self.state.lock().expect("in-memory backup state lock");
        state.objects.get(key).cloned()
    }

    #[must_use]
    pub fn writes(&self) -> Vec<String> {
        let state = self.state.lock().expect("in-memory backup state lock");
        state.writes.clone()
    }

    fn put_object(&self, key: String, payload: Vec<u8>) -> Result<(), BackupAdapterError> {
        let mut state = self.state.lock().expect("in-memory backup state lock");
        if let Some(existing) = state.objects.get(&key) {
            if existing == &payload {
                return Ok(());
            }
            return Err(BackupAdapterError::ObjectConflict { key });
        }
        state.writes.push(key.clone());
        state.objects.insert(key, payload);
        Ok(())
    }

    fn get_object(&self, key: &str) -> Result<Vec<u8>, BackupAdapterError> {
        let state = self.state.lock().expect("in-memory backup state lock");
        state
            .objects
            .get(key)
            .cloned()
            .ok_or_else(|| BackupAdapterError::ObjectMissing {
                key: key.to_owned(),
            })
    }
}

#[derive(Default)]
struct InMemoryBackupState {
    objects: BTreeMap<String, Vec<u8>>,
    writes: Vec<String>,
}

impl BackupTargetAdapter for InMemoryBackupAdapter {
    fn write_control_plane_bundle<'a>(
        &'a self,
        operation_id: &'a OperationId,
        target: &'a BackupTarget,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<BackupArtifact, BackupAdapterError>> {
        Box::pin(async move {
            validate_backup_target(target)?;
            let S3TargetParts {
                bucket,
                key_prefix,
                region,
                endpoint_url,
                addressing_style,
            } = s3_target_parts(target)?;
            let key = backup_object_key(key_prefix, operation_id, CONTROL_PLANE_BUNDLE_FILE);
            self.put_object(key.clone(), payload.to_vec())?;
            Ok(BackupArtifact::new(
                BackupArtifactLocation::s3(
                    bucket.clone(),
                    key,
                    region.clone(),
                    endpoint_url.clone(),
                    *addressing_style,
                ),
                BackupArtifactKind::ControlPlaneBundle,
                payload.len() as u64,
                sha256_digest(payload),
            ))
        })
    }

    fn write_manifest<'a>(
        &'a self,
        operation_id: &'a OperationId,
        target: &'a BackupTarget,
        manifest: &'a BackupManifest,
    ) -> BoxFuture<'a, Result<(), BackupAdapterError>> {
        Box::pin(async move {
            validate_backup_target(target)?;
            let S3TargetParts { key_prefix, .. } = s3_target_parts(target)?;
            let key = backup_object_key(key_prefix, operation_id, MANIFEST_FILE);
            let payload = serde_json::to_vec(manifest).map_err(|error| {
                BackupAdapterError::EncodeManifest {
                    message: error.to_string(),
                }
            })?;
            self.put_object(key, payload)
        })
    }

    fn read_manifest<'a>(
        &'a self,
        source: &'a BackupRestoreSource,
    ) -> BoxFuture<'a, Result<BackupManifest, BackupAdapterError>> {
        Box::pin(async move {
            let BackupRestoreSource::S3 {
                bucket,
                manifest_key,
                region,
                ..
            } = source;
            validate_s3_restore_source(bucket, manifest_key, region)?;
            let payload = self.get_object(manifest_key)?;
            serde_json::from_slice(&payload).map_err(|error| BackupAdapterError::DecodeManifest {
                message: error.to_string(),
            })
        })
    }

    fn read_artifact<'a>(
        &'a self,
        artifact: &'a BackupArtifact,
    ) -> BoxFuture<'a, Result<Vec<u8>, BackupAdapterError>> {
        Box::pin(async move {
            let BackupArtifactLocation::S3 { key, .. } = &artifact.location;
            if artifact.kind != BackupArtifactKind::ControlPlaneBundle {
                return Err(BackupAdapterError::UnsupportedArtifactKind {
                    kind: artifact.kind,
                });
            }
            let payload = self.get_object(key)?;
            verify_payload(artifact, &payload)?;
            Ok(payload)
        })
    }
}

struct S3TargetParts<'a> {
    bucket: &'a String,
    key_prefix: &'a String,
    region: &'a String,
    endpoint_url: &'a Option<String>,
    addressing_style: &'a S3AddressingStyle,
}

fn s3_target_parts(target: &BackupTarget) -> Result<S3TargetParts<'_>, BackupAdapterError> {
    let BackupTarget::S3 {
        bucket,
        key_prefix,
        region,
        endpoint_url,
        addressing_style,
    } = target;
    Ok(S3TargetParts {
        bucket,
        key_prefix,
        region,
        endpoint_url,
        addressing_style,
    })
}

fn validate_backup_target(target: &BackupTarget) -> Result<(), BackupAdapterError> {
    target
        .validate_create()
        .map_err(|error| BackupAdapterError::InvalidTarget {
            field: error.field.wire_name(),
            message: error.failure.message().to_owned(),
        })
}

fn validate_s3_restore_source(
    bucket: &str,
    key: &str,
    region: &str,
) -> Result<(), BackupAdapterError> {
    validate_non_empty("bucket", bucket)?;
    validate_non_empty("key", key)?;
    validate_non_empty("region", region)
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), BackupAdapterError> {
    if value.trim().is_empty() {
        return Err(BackupAdapterError::InvalidTarget {
            field,
            message: "must not be empty".to_owned(),
        });
    }
    Ok(())
}

#[must_use]
pub fn backup_object_key(key_prefix: &str, operation_id: &OperationId, filename: &str) -> String {
    format!(
        "{}/{}/{}",
        key_prefix.trim_matches('/'),
        operation_id.as_str(),
        filename
    )
}

async fn put_object_non_destructive(
    client: &S3Client,
    bucket: &str,
    key: &str,
    payload: &[u8],
) -> Result<(), BackupAdapterError> {
    let result = client
        .put_object()
        .bucket(bucket)
        .key(key)
        .if_none_match("*")
        .body(ByteStream::from(payload.to_vec()))
        .send()
        .await;

    match result {
        Ok(_) => Ok(()),
        Err(error) => {
            if let Ok(existing) = get_object_payload(client, bucket, key).await
                && existing == payload
            {
                return Ok(());
            }
            Err(BackupAdapterError::WriteObject {
                key: key.to_owned(),
                message: error.to_string(),
            })
        }
    }
}

async fn get_object_payload(
    client: &S3Client,
    bucket: &str,
    key: &str,
) -> Result<Vec<u8>, BackupAdapterError> {
    let object = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|error| BackupAdapterError::ReadObject {
            key: key.to_owned(),
            message: error.to_string(),
        })?;
    object
        .body
        .collect()
        .await
        .map(|bytes| bytes.into_bytes().to_vec())
        .map_err(|error| BackupAdapterError::ReadObject {
            key: key.to_owned(),
            message: error.to_string(),
        })
}

fn verify_payload(artifact: &BackupArtifact, payload: &[u8]) -> Result<(), BackupAdapterError> {
    if payload.len() as u64 != artifact.byte_count {
        return Err(BackupAdapterError::ArtifactMismatch {
            message: format!(
                "expected {} bytes, read {} bytes",
                artifact.byte_count,
                payload.len()
            ),
        });
    }
    let digest = sha256_digest(payload);
    if digest != artifact.sha256_digest {
        return Err(BackupAdapterError::ArtifactMismatch {
            message: format!("expected digest {}, got {digest}", artifact.sha256_digest),
        });
    }
    Ok(())
}

fn sha256_digest(payload: &[u8]) -> String {
    format!("{:x}", Sha256::digest(payload))
}

#[derive(Debug)]
pub enum BackupAdapterError {
    InvalidTarget {
        field: &'static str,
        message: String,
    },
    EncodeManifest {
        message: String,
    },
    DecodeManifest {
        message: String,
    },
    WriteObject {
        key: String,
        message: String,
    },
    ReadObject {
        key: String,
        message: String,
    },
    ObjectConflict {
        key: String,
    },
    ObjectMissing {
        key: String,
    },
    UnsupportedArtifactKind {
        kind: BackupArtifactKind,
    },
    ArtifactMismatch {
        message: String,
    },
}

impl fmt::Display for BackupAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget { field, message } => {
                write!(formatter, "invalid backup target {field}: {message}")
            }
            Self::EncodeManifest { message } => {
                write!(formatter, "encode backup manifest: {message}")
            }
            Self::DecodeManifest { message } => {
                write!(formatter, "decode backup manifest: {message}")
            }
            Self::WriteObject { key, message } => write!(formatter, "write {key}: {message}"),
            Self::ReadObject { key, message } => write!(formatter, "read {key}: {message}"),
            Self::ObjectConflict { key } => {
                write!(
                    formatter,
                    "backup object {key} already exists with different content"
                )
            }
            Self::ObjectMissing { key } => write!(formatter, "backup object {key} is missing"),
            Self::UnsupportedArtifactKind { kind } => {
                write!(formatter, "unsupported backup artifact kind {kind:?}")
            }
            Self::ArtifactMismatch { message } => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for BackupAdapterError {}

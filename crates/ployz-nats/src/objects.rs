//! JetStream Object Store helpers.

use async_nats::jetstream;
use ployz_core::backup::{BackupArtifact, BackupArtifactKind};
use ployz_core::ids::OperationId;
use tokio::io::AsyncReadExt;

use crate::bootstrap::ResourceReplicas;
use crate::kv::{NatsIoTimeout, with_io_timeout};

pub const PLZ_BACKUPS_BUCKET: &str = "PLZ_BACKUPS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectBucketSpec {
    pub name: &'static str,
    pub(crate) replicas: ResourceReplicas,
}

impl ObjectBucketSpec {
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            replicas: ResourceReplicas::SINGLE_CORE,
        }
    }

    #[must_use]
    pub const fn replicas(&self) -> ResourceReplicas {
        self.replicas
    }

    #[must_use]
    pub(crate) const fn with_observed_replicas(mut self, replicas: ResourceReplicas) -> Self {
        self.replicas = replicas;
        self
    }
}

#[derive(Clone)]
pub struct AsyncNatsBackupObjectStore {
    bucket: jetstream::object_store::ObjectStore,
}

impl AsyncNatsBackupObjectStore {
    pub async fn from_jetstream(
        jetstream: &jetstream::Context,
    ) -> Result<Self, BackupObjectStoreError> {
        let bucket = with_io_timeout(
            "backup object store open",
            jetstream.get_object_store(PLZ_BACKUPS_BUCKET),
        )
        .await?
        .map_err(|error| BackupObjectStoreError::OpenBucket {
            bucket: PLZ_BACKUPS_BUCKET,
            message: error.to_string(),
        })?;

        Ok(Self { bucket })
    }

    pub async fn write_control_plane_bundle(
        &self,
        operation_id: &OperationId,
        payload: &[u8],
    ) -> Result<BackupArtifact, BackupObjectStoreError> {
        let object_name = control_plane_bundle_object_name(operation_id);
        let info = with_io_timeout("backup object write", async {
            let mut reader = payload;
            self.bucket.put(object_name.as_str(), &mut reader).await
        })
        .await?
        .map_err(|error| BackupObjectStoreError::PutObject {
            object_name: object_name.clone(),
            message: error.to_string(),
        })?;

        Ok(BackupArtifact::new(
            info.bucket,
            info.name,
            BackupArtifactKind::ControlPlaneBundle,
            info.size as u64,
            info.digest.unwrap_or_else(|| "unknown".to_owned()),
        ))
    }

    pub async fn read_control_plane_bundle(
        &self,
        artifact: &BackupArtifact,
    ) -> Result<Vec<u8>, BackupObjectStoreError> {
        if artifact.bucket != PLZ_BACKUPS_BUCKET
            || artifact.kind != BackupArtifactKind::ControlPlaneBundle
        {
            return Err(BackupObjectStoreError::WrongArtifact {
                bucket: artifact.bucket.clone(),
                object_name: artifact.object_name.clone(),
                kind: artifact.kind,
            });
        }

        let mut object = with_io_timeout(
            "backup object read",
            self.bucket.get(artifact.object_name.as_str()),
        )
        .await?
        .map_err(|error| BackupObjectStoreError::GetObject {
            object_name: artifact.object_name.clone(),
            message: error.to_string(),
        })?;
        verify_backup_artifact_info(artifact, object.info())?;

        let mut payload = Vec::new();
        with_io_timeout("backup object body read", object.read_to_end(&mut payload))
            .await?
            .map_err(|error| BackupObjectStoreError::GetObject {
                object_name: artifact.object_name.clone(),
                message: error.to_string(),
            })?;
        if payload.len() as u64 != artifact.byte_count {
            return Err(BackupObjectStoreError::ArtifactMismatch {
                object_name: artifact.object_name.clone(),
                message: format!(
                    "expected {} bytes, read {} bytes",
                    artifact.byte_count,
                    payload.len()
                ),
            });
        }

        Ok(payload)
    }
}

fn verify_backup_artifact_info(
    artifact: &BackupArtifact,
    info: &jetstream::object_store::ObjectInfo,
) -> Result<(), BackupObjectStoreError> {
    if info.size as u64 != artifact.byte_count {
        return Err(BackupObjectStoreError::ArtifactMismatch {
            object_name: artifact.object_name.clone(),
            message: format!(
                "expected {} bytes, object store reports {} bytes",
                artifact.byte_count, info.size
            ),
        });
    }

    let Some(digest) = &info.digest else {
        return Err(BackupObjectStoreError::ArtifactMismatch {
            object_name: artifact.object_name.clone(),
            message: "object store did not report a digest".to_owned(),
        });
    };
    if digest != &artifact.digest {
        return Err(BackupObjectStoreError::ArtifactMismatch {
            object_name: artifact.object_name.clone(),
            message: format!("expected digest {}, got {}", artifact.digest, digest),
        });
    }

    Ok(())
}

#[must_use]
pub fn control_plane_bundle_object_name(operation_id: &OperationId) -> String {
    format!(
        "backups/{}/control-plane-bundle.json",
        operation_id.as_str()
    )
}

#[derive(Debug)]
pub enum BackupObjectStoreError {
    OpenBucket {
        bucket: &'static str,
        message: String,
    },
    PutObject {
        object_name: String,
        message: String,
    },
    GetObject {
        object_name: String,
        message: String,
    },
    WrongArtifact {
        bucket: String,
        object_name: String,
        kind: BackupArtifactKind,
    },
    ArtifactMismatch {
        object_name: String,
        message: String,
    },
    Timeout {
        operation: &'static str,
    },
}

impl From<NatsIoTimeout> for BackupObjectStoreError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

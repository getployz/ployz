//! JetStream Object Store helpers.

use async_nats::jetstream;
use ployz_core::backup::{BackupArtifact, BackupArtifactKind};
use ployz_core::ids::OperationId;
use std::future::Future;
use std::time::Duration;

use crate::bootstrap::ResourceReplicas;

pub const PLZ_BACKUPS_BUCKET: &str = "PLZ_BACKUPS";
const NATS_OBJECT_TIMEOUT: Duration = Duration::from_secs(10);

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
        let bucket = with_object_timeout(
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
        let info = with_object_timeout("backup object write", async {
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
    Timeout {
        operation: &'static str,
    },
}

async fn with_object_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, BackupObjectStoreError> {
    tokio::time::timeout(NATS_OBJECT_TIMEOUT, future)
        .await
        .map_err(|_| BackupObjectStoreError::Timeout { operation })
}

//! Control-plane restore from backup bundles.

use crate::backup_adapters::{BackupAdapterError, BackupAdapterRegistry};
use crate::backup_runtime::CONTROL_PLANE_KV_BUCKETS;
use ployz_core::backup::{
    BackupArtifact, BackupBundle, BackupRestoreSource, ControlPlaneKvSnapshot, KvBucketSnapshot,
};
use std::collections::BTreeSet;
use std::fmt;

#[derive(Clone)]
pub struct BackupRestoreRuntime {
    jetstream: async_nats::jetstream::Context,
    adapters: BackupAdapterRegistry,
}

impl BackupRestoreRuntime {
    #[must_use]
    pub fn new(jetstream: async_nats::jetstream::Context, adapters: BackupAdapterRegistry) -> Self {
        Self {
            jetstream,
            adapters,
        }
    }

    pub async fn restore_source(
        &self,
        source: &BackupRestoreSource,
    ) -> Result<ControlPlaneRestoreReport, BackupRestoreError> {
        let manifest = self
            .adapters
            .read_manifest(source)
            .await
            .map_err(BackupRestoreError::ReadArtifact)?;
        let [artifact] = manifest.artifacts.as_slice() else {
            return Err(BackupRestoreError::InvalidManifest {
                message: format!(
                    "expected exactly one control-plane bundle artifact, got {}",
                    manifest.artifacts.len()
                ),
            });
        };
        self.restore_artifact(artifact).await
    }

    pub async fn restore_artifact(
        &self,
        artifact: &BackupArtifact,
    ) -> Result<ControlPlaneRestoreReport, BackupRestoreError> {
        let payload = self
            .adapters
            .read_artifact(artifact)
            .await
            .map_err(BackupRestoreError::ReadArtifact)?;
        let bundle = serde_json::from_slice::<BackupBundle>(&payload).map_err(|error| {
            BackupRestoreError::DecodeArtifact {
                message: error.to_string(),
            }
        })?;

        self.restore_bundle(&bundle).await
    }

    pub async fn restore_bundle(
        &self,
        bundle: &BackupBundle,
    ) -> Result<ControlPlaneRestoreReport, BackupRestoreError> {
        validate_control_plane_snapshot(&bundle.control_plane)?;
        self.preflight_empty_restore_target(&bundle.control_plane)
            .await?;

        let mut buckets = Vec::with_capacity(bundle.control_plane.buckets.len());
        for snapshot in &bundle.control_plane.buckets {
            buckets.push(self.restore_bucket(snapshot).await?);
        }
        Ok(ControlPlaneRestoreReport {
            buckets,
            observations: RestoreObservationState::RebuildableAfterMachineReconnect {
                restored_entries: 0,
            },
        })
    }

    async fn preflight_empty_restore_target(
        &self,
        snapshot: &ControlPlaneKvSnapshot,
    ) -> Result<(), BackupRestoreError> {
        for bucket in &snapshot.buckets {
            self.preflight_empty_bucket(bucket).await?;
        }

        Ok(())
    }

    async fn preflight_empty_bucket(
        &self,
        snapshot: &KvBucketSnapshot,
    ) -> Result<(), BackupRestoreError> {
        let bucket = self.open_bucket(snapshot.name.as_str()).await?;
        for entry in &snapshot.entries {
            let existing = bucket.get(entry.key.as_str()).await.map_err(|error| {
                BackupRestoreError::CheckDestination {
                    bucket: snapshot.name.clone(),
                    key: entry.key.clone(),
                    message: error.to_string(),
                }
            })?;
            if existing.is_some() {
                return Err(BackupRestoreError::DestinationNotEmpty {
                    bucket: snapshot.name.clone(),
                    key: entry.key.clone(),
                });
            }
        }

        Ok(())
    }

    async fn restore_bucket(
        &self,
        snapshot: &KvBucketSnapshot,
    ) -> Result<KvBucketRestoreReport, BackupRestoreError> {
        let bucket = self.open_bucket(snapshot.name.as_str()).await?;

        for entry in &snapshot.entries {
            bucket
                .create(entry.key.as_str(), entry.value.clone().into())
                .await
                .map_err(|error| BackupRestoreError::RestoreEntry {
                    bucket: snapshot.name.clone(),
                    key: entry.key.clone(),
                    message: error.to_string(),
                })?;
        }

        Ok(KvBucketRestoreReport {
            name: snapshot.name.clone(),
            entries_restored: snapshot.entries.len(),
        })
    }

    async fn open_bucket(
        &self,
        bucket: &str,
    ) -> Result<async_nats::jetstream::kv::Store, BackupRestoreError> {
        self.jetstream
            .get_key_value(bucket)
            .await
            .map_err(|error| BackupRestoreError::OpenBucket {
                bucket: bucket.to_owned(),
                message: error.to_string(),
            })
    }
}

fn validate_control_plane_snapshot(
    snapshot: &ControlPlaneKvSnapshot,
) -> Result<(), BackupRestoreError> {
    let mut buckets = BTreeSet::new();
    for bucket in &snapshot.buckets {
        if !CONTROL_PLANE_KV_BUCKETS.contains(&bucket.name.as_str()) {
            return Err(BackupRestoreError::UnknownBucket {
                bucket: bucket.name.clone(),
            });
        }
        if !buckets.insert(bucket.name.as_str()) {
            return Err(BackupRestoreError::DuplicateBucket {
                bucket: bucket.name.clone(),
            });
        }
        validate_bucket_entries(bucket)?;
    }

    for expected in CONTROL_PLANE_KV_BUCKETS {
        if !buckets.contains(expected) {
            return Err(BackupRestoreError::MissingBucket {
                bucket: expected.to_owned(),
            });
        }
    }

    Ok(())
}

fn validate_bucket_entries(bucket: &KvBucketSnapshot) -> Result<(), BackupRestoreError> {
    let mut keys = BTreeSet::new();
    for entry in &bucket.entries {
        if !keys.insert(entry.key.as_str()) {
            return Err(BackupRestoreError::DuplicateKey {
                bucket: bucket.name.clone(),
                key: entry.key.clone(),
            });
        }
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlaneRestoreReport {
    pub buckets: Vec<KvBucketRestoreReport>,
    pub observations: RestoreObservationState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvBucketRestoreReport {
    pub name: String,
    pub entries_restored: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreObservationState {
    RebuildableAfterMachineReconnect { restored_entries: usize },
}

#[derive(Debug)]
pub enum BackupRestoreError {
    ReadArtifact(BackupAdapterError),
    DecodeArtifact {
        message: String,
    },
    InvalidManifest {
        message: String,
    },
    MissingBucket {
        bucket: String,
    },
    UnknownBucket {
        bucket: String,
    },
    DuplicateBucket {
        bucket: String,
    },
    DuplicateKey {
        bucket: String,
        key: String,
    },
    CheckDestination {
        bucket: String,
        key: String,
        message: String,
    },
    DestinationNotEmpty {
        bucket: String,
        key: String,
    },
    OpenBucket {
        bucket: String,
        message: String,
    },
    RestoreEntry {
        bucket: String,
        key: String,
        message: String,
    },
}

impl fmt::Display for BackupRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadArtifact(error) => {
                write!(formatter, "backup artifact could not be read: {error}")
            }
            Self::DecodeArtifact { message } => {
                write!(formatter, "backup artifact could not be decoded: {message}")
            }
            Self::InvalidManifest { message } => {
                write!(formatter, "invalid backup manifest: {message}")
            }
            Self::MissingBucket { bucket } => {
                write!(formatter, "backup bundle is missing KV bucket {bucket}")
            }
            Self::UnknownBucket { bucket } => {
                write!(
                    formatter,
                    "backup bundle contains unknown KV bucket {bucket}"
                )
            }
            Self::DuplicateBucket { bucket } => {
                write!(
                    formatter,
                    "backup bundle contains duplicate KV bucket {bucket}"
                )
            }
            Self::DuplicateKey { bucket, key } => {
                write!(
                    formatter,
                    "backup bundle contains duplicate key {bucket}.{key}"
                )
            }
            Self::CheckDestination {
                bucket,
                key,
                message,
            } => write!(
                formatter,
                "restore check destination KV entry {bucket}.{key}: {message}"
            ),
            Self::DestinationNotEmpty { bucket, key } => {
                write!(
                    formatter,
                    "restore target KV entry {bucket}.{key} already exists"
                )
            }
            Self::OpenBucket { bucket, message } => {
                write!(formatter, "restore open KV bucket {bucket}: {message}")
            }
            Self::RestoreEntry {
                bucket,
                key,
                message,
            } => write!(formatter, "restore KV entry {bucket}.{key}: {message}"),
        }
    }
}

impl std::error::Error for BackupRestoreError {}

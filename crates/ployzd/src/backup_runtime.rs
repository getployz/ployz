//! Owned backup operation execution.

use crate::backup_adapters::{BackupAdapterError, BackupAdapterRegistry};
use crate::controllers::OperationControllers;
use crate::tasks::TaskRegistry;
use futures_util::TryStreamExt;
use ployz_core::backup::{
    BackupArtifact, BackupBundle, BackupManifest, ControlPlaneKvSnapshot, KvBucketSnapshot,
    KvEntrySnapshot,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    BackupOperationFailure, BackupRunningStage, BackupTransition, FailureMessage, OperationStatus,
    StatusProjectionError,
};
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::operations::{
    AcceptedBackupSubmission, OperationStatusWrite, RecordBackupEventError,
    RecordOperationEventError,
};
use std::fmt;

pub(crate) const CONTROL_PLANE_KV_BUCKETS: [&str; 1] = [KV_CORE_BUCKET];

#[derive(Clone)]
pub struct BackupOperationRuntime {
    controllers: OperationControllers,
    adapters: BackupAdapterRegistry,
    snapshot_source: ControlPlaneSnapshotSource,
    task_registry: TaskRegistry,
}

impl BackupOperationRuntime {
    #[must_use]
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        controllers: OperationControllers,
        adapters: BackupAdapterRegistry,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            controllers,
            adapters,
            snapshot_source: ControlPlaneSnapshotSource::new(jetstream),
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedBackupSubmission) {
        if !accepted.should_start_execution {
            return;
        }

        let runtime = self.clone();
        self.task_registry.spawn(async move {
            let operation_id = accepted.operation_id.clone();
            let failure_runtime = runtime.clone();
            if let Err(error) = runtime.run(accepted).await {
                failure_runtime
                    .record_execution_failure(&operation_id, &error)
                    .await;
            }
        });
    }

    pub async fn run(self, accepted: AcceptedBackupSubmission) -> Result<(), BackupExecutionError> {
        let operation_id = accepted.operation_id.clone();
        let target = accepted.target.clone();

        run_backup_create(
            &self.controllers,
            &self.adapters,
            &self.snapshot_source,
            &operation_id,
            &target,
        )
        .await
    }

    /// The single writer of `Failed` transitions for backup execution.
    async fn record_execution_failure(
        &self,
        operation_id: &OperationId,
        error: &BackupExecutionError,
    ) {
        if let Ok(Some(OperationStatus::Backup { state, .. })) = self
            .controllers
            .repository()
            .records()
            .get(operation_id)
            .await
            && state.is_terminal()
        {
            return;
        }

        let _ = self
            .controllers
            .repository()
            .record_backup_transition(
                operation_id,
                BackupTransition::Failed {
                    failure: backup_execution_failure(error),
                },
            )
            .await;
    }
}

async fn run_backup_create(
    controllers: &OperationControllers,
    adapters: &BackupAdapterRegistry,
    snapshot_source: &ControlPlaneSnapshotSource,
    operation_id: &OperationId,
    target: &ployz_core::backup::BackupTarget,
) -> Result<(), BackupExecutionError> {
    if !claim_backup_execution(controllers, operation_id).await? {
        return Ok(());
    }
    write_artifact_and_complete(controllers, adapters, snapshot_source, operation_id, target).await
}

async fn claim_backup_execution(
    controllers: &OperationControllers,
    operation_id: &OperationId,
) -> Result<bool, BackupExecutionError> {
    match controllers
        .repository()
        .record_backup_transition(
            operation_id,
            BackupTransition::Running {
                stage: BackupRunningStage::SnapshottingControlPlane,
            },
        )
        .await
    {
        Ok(OperationStatusWrite::Stored { .. }) => Ok(true),
        Ok(OperationStatusWrite::AlreadySatisfied { .. } | OperationStatusWrite::Stale { .. }) => {
            Ok(false)
        }
        Err(RecordOperationEventError::ProjectStatus(
            StatusProjectionError::InvalidTransition { .. }
            | StatusProjectionError::TerminalState { .. },
        )) => Ok(false),
        Err(error) => Err(BackupExecutionError::RecordTransition {
            stage: BackupFailureStage::Snapshot,
            error,
        }),
    }
}

async fn record_transition(
    controllers: &OperationControllers,
    operation_id: &OperationId,
    stage: BackupFailureStage,
    transition: BackupTransition,
) -> Result<OperationStatusWrite, BackupExecutionError> {
    controllers
        .repository()
        .record_backup_transition(operation_id, transition)
        .await
        .map_err(|error| BackupExecutionError::RecordTransition { stage, error })
}

async fn write_artifact_and_complete(
    controllers: &OperationControllers,
    adapters: &BackupAdapterRegistry,
    snapshot_source: &ControlPlaneSnapshotSource,
    operation_id: &OperationId,
    target: &ployz_core::backup::BackupTarget,
) -> Result<(), BackupExecutionError> {
    let snapshot = snapshot_source
        .snapshot()
        .await
        .map_err(BackupExecutionError::SnapshotControlPlane)?;
    let bundle = BackupBundle::new(snapshot);
    let payload =
        serde_json::to_vec(&bundle).map_err(|error| BackupExecutionError::EncodeSnapshot {
            message: error.to_string(),
        })?;
    let artifact = adapters
        .write_control_plane_bundle(operation_id, target, &payload)
        .await
        .map_err(BackupExecutionError::WriteArtifact)?;

    record_transition(
        controllers,
        operation_id,
        BackupFailureStage::Manifest,
        BackupTransition::Running {
            stage: BackupRunningStage::WritingManifest {
                artifact: artifact.clone(),
            },
        },
    )
    .await?;
    complete_from_artifact(controllers, adapters, operation_id, target, artifact).await
}

async fn complete_from_artifact(
    controllers: &OperationControllers,
    adapters: &BackupAdapterRegistry,
    operation_id: &OperationId,
    target: &ployz_core::backup::BackupTarget,
    artifact: BackupArtifact,
) -> Result<(), BackupExecutionError> {
    let manifest = BackupManifest::current_control_plane_kv_only().with_artifact(artifact);
    adapters
        .write_manifest(operation_id, target, &manifest)
        .await
        .map_err(BackupExecutionError::WriteArtifact)?;

    record_transition(
        controllers,
        operation_id,
        BackupFailureStage::Manifest,
        BackupTransition::Completed { manifest },
    )
    .await?;
    Ok(())
}

#[derive(Clone)]
struct ControlPlaneSnapshotSource {
    jetstream: async_nats::jetstream::Context,
}

impl ControlPlaneSnapshotSource {
    #[must_use]
    const fn new(jetstream: async_nats::jetstream::Context) -> Self {
        Self { jetstream }
    }

    async fn snapshot(&self) -> Result<ControlPlaneKvSnapshot, BackupSnapshotError> {
        let mut snapshots = Vec::with_capacity(CONTROL_PLANE_KV_BUCKETS.len());

        for bucket in CONTROL_PLANE_KV_BUCKETS {
            snapshots.push(self.snapshot_bucket(bucket).await?);
        }

        Ok(ControlPlaneKvSnapshot::new(snapshots))
    }

    async fn snapshot_bucket(
        &self,
        name: &'static str,
    ) -> Result<KvBucketSnapshot, BackupSnapshotError> {
        let bucket = self.jetstream.get_key_value(name).await.map_err(|error| {
            BackupSnapshotError::OpenBucket {
                bucket: name,
                message: error.to_string(),
            }
        })?;
        let keys = bucket
            .keys()
            .await
            .map_err(|error| BackupSnapshotError::ListKeys {
                bucket: name,
                message: error.to_string(),
            })?
            .try_collect::<Vec<String>>()
            .await
            .map_err(|error| BackupSnapshotError::ListKeys {
                bucket: name,
                message: error.to_string(),
            })?;
        let mut entries = Vec::with_capacity(keys.len());

        for key in keys {
            let Some(entry) =
                bucket
                    .entry(&key)
                    .await
                    .map_err(|error| BackupSnapshotError::ReadEntry {
                        bucket: name,
                        key: key.clone(),
                        message: error.to_string(),
                    })?
            else {
                continue;
            };
            entries.push(KvEntrySnapshot::new(
                key,
                entry.revision,
                entry.value.to_vec(),
            ));
        }

        entries.sort_by(|left, right| left.key.cmp(&right.key));

        Ok(KvBucketSnapshot::new(name, entries))
    }
}

#[derive(Debug)]
pub enum BackupSnapshotError {
    OpenBucket {
        bucket: &'static str,
        message: String,
    },
    ListKeys {
        bucket: &'static str,
        message: String,
    },
    ReadEntry {
        bucket: &'static str,
        key: String,
        message: String,
    },
}

impl fmt::Display for BackupSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenBucket { bucket, message } => {
                write!(formatter, "open KV bucket {bucket}: {message}")
            }
            Self::ListKeys { bucket, message } => {
                write!(formatter, "list keys in KV bucket {bucket}: {message}")
            }
            Self::ReadEntry {
                bucket,
                key,
                message,
            } => write!(
                formatter,
                "read key {key} from KV bucket {bucket}: {message}"
            ),
        }
    }
}

/// Which backup failure variant a recording failure maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupFailureStage {
    Snapshot,
    Manifest,
}

#[derive(Debug)]
pub enum BackupExecutionError {
    SnapshotControlPlane(BackupSnapshotError),
    EncodeSnapshot {
        message: String,
    },
    WriteArtifact(BackupAdapterError),
    RecordTransition {
        stage: BackupFailureStage,
        error: RecordBackupEventError,
    },
}

fn backup_execution_failure(error: &BackupExecutionError) -> BackupOperationFailure {
    match error {
        BackupExecutionError::SnapshotControlPlane(source) => {
            BackupOperationFailure::SnapshotFailed {
                message: backup_failure_message(format!("control-plane snapshot failed: {source}")),
            }
        }
        BackupExecutionError::EncodeSnapshot { message } => {
            BackupOperationFailure::ManifestWriteFailed {
                message: backup_failure_message(format!(
                    "backup artifact encode failed: {message}"
                )),
            }
        }
        BackupExecutionError::WriteArtifact(source) => {
            BackupOperationFailure::ManifestWriteFailed {
                message: backup_failure_message(format!("backup artifact write failed: {source}")),
            }
        }
        BackupExecutionError::RecordTransition { stage, error } => {
            let message =
                backup_failure_message(format!("backup operation update failed: {error:?}"));
            match stage {
                BackupFailureStage::Snapshot => BackupOperationFailure::SnapshotFailed { message },
                BackupFailureStage::Manifest => {
                    BackupOperationFailure::ManifestWriteFailed { message }
                }
            }
        }
    }
}

fn backup_failure_message(message: String) -> FailureMessage {
    FailureMessage::try_new(message).expect("generated backup failure message is non-empty")
}

impl fmt::Display for BackupExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SnapshotControlPlane(source) => {
                write!(formatter, "control-plane snapshot failed: {source}")
            }
            Self::EncodeSnapshot { message } => {
                write!(
                    formatter,
                    "control-plane snapshot could not be encoded: {message}"
                )
            }
            Self::WriteArtifact(source) => {
                write!(formatter, "backup artifact could not be written: {source}")
            }
            Self::RecordTransition { stage: _, error } => {
                write!(
                    formatter,
                    "backup operation transition could not be recorded: {error:?}"
                )
            }
        }
    }
}

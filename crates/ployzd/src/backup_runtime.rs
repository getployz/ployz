//! Owned backup operation execution.

use crate::controllers::{AcceptedBackupOperation, OperationControllers};
use crate::operation_lease::with_advisory_operation_lease;
use futures_util::TryStreamExt;
use ployz_core::backup::{
    BackupArtifact, BackupBundle, BackupManifest, ControlPlaneKvSnapshot, KvBucketSnapshot,
    KvEntrySnapshot,
};
use ployz_core::ids::{OperationId, OperationOwnerId};
use ployz_core::ops::{
    BackupOperationFailure, BackupOperationState, BackupRunningStage, BackupTransition,
    FailureMessage, OperationOwnerLease, OperationStatus,
};
use ployz_nats::kv::{KV_CORE_BUCKET, KV_LOCKS_BUCKET};
use ployz_nats::objects::{AsyncNatsBackupObjectStore, BackupObjectStoreError};
use ployz_nats::observations::KV_OBS_BUCKET;
use ployz_nats::operations::{
    KV_OPS_BUCKET, OperationStatusReadError, OperationStatusStoreError, RecordBackupEventError,
};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

const CONTROL_PLANE_KV_BUCKETS: [&str; 4] = [
    KV_CORE_BUCKET,
    KV_OPS_BUCKET,
    KV_OBS_BUCKET,
    KV_LOCKS_BUCKET,
];

#[derive(Clone)]
pub struct BackupOperationRuntime {
    controllers: OperationControllers,
    backups: AsyncNatsBackupObjectStore,
    snapshot_source: ControlPlaneSnapshotSource,
    task_registry: BackupTaskRegistry,
}

#[derive(Clone)]
pub struct BackupRestoreRuntime {
    jetstream: async_nats::jetstream::Context,
    backups: AsyncNatsBackupObjectStore,
}

impl BackupRestoreRuntime {
    #[must_use]
    pub const fn new(
        jetstream: async_nats::jetstream::Context,
        backups: AsyncNatsBackupObjectStore,
    ) -> Self {
        Self { jetstream, backups }
    }

    pub async fn restore_artifact(
        &self,
        artifact: &BackupArtifact,
    ) -> Result<ControlPlaneRestoreReport, BackupRestoreError> {
        let payload = self
            .backups
            .read_control_plane_bundle(artifact)
            .await
            .map_err(BackupRestoreError::ReadArtifact)?;
        let bundle = serde_json::from_slice::<BackupBundle>(&payload).map_err(|error| {
            BackupRestoreError::DecodeArtifact {
                object_name: artifact.object_name.clone(),
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
        let restored_observation_entries = buckets
            .iter()
            .find(|bucket| bucket.name == KV_OBS_BUCKET)
            .map(|bucket| bucket.entries_restored)
            .unwrap_or(0);

        Ok(ControlPlaneRestoreReport {
            buckets,
            observations: RestoreObservationState::RebuildableAfterNodeReconnect {
                restored_entries: restored_observation_entries,
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
    RebuildableAfterNodeReconnect { restored_entries: usize },
}

#[derive(Debug)]
pub enum BackupRestoreError {
    ReadArtifact(BackupObjectStoreError),
    DecodeArtifact {
        object_name: String,
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
                write!(formatter, "backup artifact could not be read: {error:?}")
            }
            Self::DecodeArtifact {
                object_name,
                message,
            } => write!(
                formatter,
                "backup artifact {object_name} could not be decoded: {message}"
            ),
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

impl BackupOperationRuntime {
    #[must_use]
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        controllers: OperationControllers,
        backups: AsyncNatsBackupObjectStore,
        task_registry: BackupTaskRegistry,
    ) -> Self {
        Self {
            controllers,
            backups,
            snapshot_source: ControlPlaneSnapshotSource::new(jetstream),
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedBackupOperation) {
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

    pub async fn run(self, accepted: AcceptedBackupOperation) -> Result<(), BackupExecutionError> {
        let lease = renew_backup_owner_lease(&self.controllers, &accepted).await?;
        verify_backup_lease_owner(&lease, &accepted.operation_id, self.controllers.owner_id())?;
        let lease_policy = self.controllers.lease_policy();
        let lease_renewer = self.controllers.clone();
        let operation_id = accepted.operation_id.clone();

        with_advisory_operation_lease(
            operation_id.clone(),
            lease_policy,
            lease_renewer,
            async move {
                run_backup_create(
                    &self.controllers,
                    &self.backups,
                    &self.snapshot_source,
                    &operation_id,
                )
                .await
            },
        )
        .await
    }

    async fn record_execution_failure(
        &self,
        operation_id: &OperationId,
        error: &BackupExecutionError,
    ) {
        if let Ok(Some(OperationStatus::Backup { state, .. })) =
            self.controllers.operation_status(operation_id).await
            && state.is_terminal()
        {
            return;
        }

        let _ = self
            .controllers
            .record_backup_transition(
                operation_id,
                BackupTransition::Failed {
                    failure: backup_execution_failure(error),
                },
            )
            .await;
    }
}

async fn renew_backup_owner_lease(
    controllers: &OperationControllers,
    accepted: &AcceptedBackupOperation,
) -> Result<OperationOwnerLease, BackupExecutionError> {
    verify_backup_lease_owner(
        &accepted.lease,
        &accepted.operation_id,
        controllers.owner_id(),
    )?;
    let Some(lease) = controllers
        .renew_owner_lease(&accepted.operation_id)
        .await
        .map_err(BackupExecutionError::RenewLease)?
    else {
        return Err(BackupExecutionError::NoCurrentLease {
            operation_id: accepted.operation_id.clone(),
            expected_owner: controllers.owner_id().clone(),
        });
    };
    verify_backup_lease_owner(&lease, &accepted.operation_id, controllers.owner_id())?;

    Ok(lease)
}

fn verify_backup_lease_owner(
    lease: &OperationOwnerLease,
    operation_id: &OperationId,
    expected_owner: &OperationOwnerId,
) -> Result<(), BackupExecutionError> {
    if &lease.operation_id != operation_id {
        return Err(BackupExecutionError::LeaseOperationMismatch {
            operation_id: operation_id.clone(),
            lease: lease.clone(),
        });
    }
    if lease.owner_id != *expected_owner {
        return Err(BackupExecutionError::LeaseNotHeld {
            operation_id: operation_id.clone(),
            lease: lease.clone(),
            expected_owner: expected_owner.clone(),
        });
    }

    Ok(())
}

async fn run_backup_create(
    controllers: &OperationControllers,
    backups: &AsyncNatsBackupObjectStore,
    snapshot_source: &ControlPlaneSnapshotSource,
    operation_id: &OperationId,
) -> Result<(), BackupExecutionError> {
    match backup_state(controllers, operation_id).await? {
        BackupOperationState::Accepted => {
            record_snapshotting(controllers, operation_id).await?;
            write_artifact_and_complete(controllers, backups, snapshot_source, operation_id).await
        }
        BackupOperationState::Running {
            stage: BackupRunningStage::SnapshottingControlPlane,
        } => write_artifact_and_complete(controllers, backups, snapshot_source, operation_id).await,
        BackupOperationState::Running {
            stage: BackupRunningStage::WritingManifest { artifact },
        } => complete_from_artifact(controllers, operation_id, artifact).await,
        BackupOperationState::Completed { .. }
        | BackupOperationState::Failed { .. }
        | BackupOperationState::Cancelled { .. } => Ok(()),
    }
}

async fn backup_state(
    controllers: &OperationControllers,
    operation_id: &OperationId,
) -> Result<BackupOperationState, BackupExecutionError> {
    let Some(status) = controllers
        .operation_status(operation_id)
        .await
        .map_err(BackupExecutionError::ReadStatus)?
    else {
        return Err(BackupExecutionError::MissingStatus {
            operation_id: operation_id.clone(),
        });
    };
    let OperationStatus::Backup { state, .. } = status else {
        return Err(BackupExecutionError::WrongOperationKind {
            operation_id: operation_id.clone(),
        });
    };

    Ok(state)
}

async fn record_snapshotting(
    controllers: &OperationControllers,
    operation_id: &OperationId,
) -> Result<(), BackupExecutionError> {
    match controllers
        .record_backup_transition(
            operation_id,
            BackupTransition::Running {
                stage: BackupRunningStage::SnapshottingControlPlane,
            },
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(error) => {
            record_backup_failure(
                controllers,
                operation_id,
                BackupFailureStage::Snapshot,
                error,
            )
            .await;
            Err(BackupExecutionError::RecordTransition)
        }
    }
}

async fn record_manifest_write(
    controllers: &OperationControllers,
    operation_id: &OperationId,
    artifact: BackupArtifact,
) -> Result<(), BackupExecutionError> {
    match controllers
        .record_backup_transition(
            operation_id,
            BackupTransition::Running {
                stage: BackupRunningStage::WritingManifest { artifact },
            },
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(error) => {
            record_backup_failure(
                controllers,
                operation_id,
                BackupFailureStage::Manifest,
                error,
            )
            .await;
            Err(BackupExecutionError::RecordTransition)
        }
    }
}

async fn record_completed(
    controllers: &OperationControllers,
    operation_id: &OperationId,
    manifest: BackupManifest,
) -> Result<(), BackupExecutionError> {
    match controllers
        .record_backup_transition(operation_id, BackupTransition::Completed { manifest })
        .await
    {
        Ok(_) => Ok(()),
        Err(error) => {
            record_backup_failure(
                controllers,
                operation_id,
                BackupFailureStage::Manifest,
                error,
            )
            .await;
            Err(BackupExecutionError::RecordTransition)
        }
    }
}

async fn write_artifact_and_complete(
    controllers: &OperationControllers,
    backups: &AsyncNatsBackupObjectStore,
    snapshot_source: &ControlPlaneSnapshotSource,
    operation_id: &OperationId,
) -> Result<(), BackupExecutionError> {
    let snapshot = match snapshot_source.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            record_backup_snapshot_failure(controllers, operation_id, error).await;
            return Err(BackupExecutionError::SnapshotControlPlane);
        }
    };
    let bundle = BackupBundle::new(snapshot);
    let payload = match serde_json::to_vec(&bundle) {
        Ok(payload) => payload,
        Err(error) => {
            record_backup_encode_failure(controllers, operation_id, error.to_string()).await;
            return Err(BackupExecutionError::EncodeSnapshot {
                message: error.to_string(),
            });
        }
    };
    let artifact = match backups
        .write_control_plane_bundle(operation_id, &payload)
        .await
    {
        Ok(artifact) => artifact,
        Err(error) => {
            record_backup_object_failure(controllers, operation_id, error).await;
            return Err(BackupExecutionError::WriteArtifact);
        }
    };

    record_manifest_write(controllers, operation_id, artifact.clone()).await?;
    complete_from_artifact(controllers, operation_id, artifact).await
}

async fn complete_from_artifact(
    controllers: &OperationControllers,
    operation_id: &OperationId,
    artifact: BackupArtifact,
) -> Result<(), BackupExecutionError> {
    let manifest = BackupManifest::current_control_plane_kv_only().with_artifact(artifact);

    record_completed(controllers, operation_id, manifest).await
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
enum BackupSnapshotError {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackupFailureStage {
    Snapshot,
    Manifest,
}

async fn record_backup_failure(
    controllers: &OperationControllers,
    operation_id: &OperationId,
    stage: BackupFailureStage,
    error: RecordBackupEventError,
) {
    let Ok(message) = FailureMessage::try_new(format!("backup operation update failed: {error:?}"))
    else {
        return;
    };
    let failure = match stage {
        BackupFailureStage::Snapshot => BackupOperationFailure::SnapshotFailed { message },
        BackupFailureStage::Manifest => BackupOperationFailure::ManifestWriteFailed { message },
    };
    let _ = controllers
        .record_backup_transition(operation_id, BackupTransition::Failed { failure })
        .await;
}

#[derive(Debug, Clone, Default)]
pub struct BackupTaskRegistry {
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl BackupTaskRegistry {
    pub fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut handles = self
            .handles
            .lock()
            .expect("backup task registry lock is not poisoned");
        handles.retain(|handle| !handle.is_finished());
        handles.push(tokio::spawn(future));
    }

    pub fn abort_all(&self) {
        let mut handles = self
            .handles
            .lock()
            .expect("backup task registry lock is not poisoned");
        for handle in handles.drain(..) {
            handle.abort();
        }
    }
}

#[derive(Debug)]
pub enum BackupExecutionError {
    RenewLease(OperationStatusStoreError),
    NoCurrentLease {
        operation_id: OperationId,
        expected_owner: OperationOwnerId,
    },
    LeaseOperationMismatch {
        operation_id: OperationId,
        lease: OperationOwnerLease,
    },
    LeaseNotHeld {
        operation_id: OperationId,
        lease: OperationOwnerLease,
        expected_owner: OperationOwnerId,
    },
    ReadStatus(OperationStatusReadError),
    MissingStatus {
        operation_id: OperationId,
    },
    WrongOperationKind {
        operation_id: OperationId,
    },
    SnapshotControlPlane,
    EncodeSnapshot {
        message: String,
    },
    WriteArtifact,
    RecordTransition,
}

fn backup_execution_failure(error: &BackupExecutionError) -> BackupOperationFailure {
    let Ok(message) = FailureMessage::try_new(format!("backup execution failed: {error}")) else {
        return BackupOperationFailure::SnapshotFailed {
            message: FailureMessage::try_new("backup execution failed")
                .expect("static failure message is valid"),
        };
    };

    match error {
        BackupExecutionError::EncodeSnapshot { .. }
        | BackupExecutionError::WriteArtifact
        | BackupExecutionError::RecordTransition => {
            BackupOperationFailure::ManifestWriteFailed { message }
        }
        BackupExecutionError::RenewLease(_)
        | BackupExecutionError::NoCurrentLease { .. }
        | BackupExecutionError::LeaseOperationMismatch { .. }
        | BackupExecutionError::LeaseNotHeld { .. }
        | BackupExecutionError::ReadStatus(_)
        | BackupExecutionError::MissingStatus { .. }
        | BackupExecutionError::WrongOperationKind { .. }
        | BackupExecutionError::SnapshotControlPlane => {
            BackupOperationFailure::SnapshotFailed { message }
        }
    }
}

impl fmt::Display for BackupExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RenewLease(error) => {
                write!(formatter, "owner lease could not be renewed: {error:?}")
            }
            Self::NoCurrentLease {
                operation_id,
                expected_owner,
            } => write!(
                formatter,
                "operation {} has no current owner lease for {}",
                operation_id.as_str(),
                expected_owner.as_str()
            ),
            Self::LeaseOperationMismatch {
                operation_id,
                lease,
            } => write!(
                formatter,
                "lease operation {} did not match backup operation {}",
                lease.operation_id.as_str(),
                operation_id.as_str()
            ),
            Self::LeaseNotHeld {
                operation_id,
                lease,
                expected_owner,
            } => write!(
                formatter,
                "operation {} lease is held by {}, not {}",
                operation_id.as_str(),
                lease.owner_id.as_str(),
                expected_owner.as_str()
            ),
            Self::ReadStatus(error) => {
                write!(formatter, "backup status could not be read: {error:?}")
            }
            Self::MissingStatus { operation_id } => {
                write!(
                    formatter,
                    "backup operation {} has no status",
                    operation_id.as_str()
                )
            }
            Self::WrongOperationKind { operation_id } => write!(
                formatter,
                "operation {} is not a backup operation",
                operation_id.as_str()
            ),
            Self::SnapshotControlPlane => formatter.write_str("control-plane snapshot failed"),
            Self::EncodeSnapshot { message } => {
                write!(
                    formatter,
                    "control-plane snapshot could not be encoded: {message}"
                )
            }
            Self::WriteArtifact => formatter.write_str("backup artifact could not be written"),
            Self::RecordTransition => {
                formatter.write_str("backup operation transition could not be recorded")
            }
        }
    }
}

async fn record_backup_object_failure(
    controllers: &OperationControllers,
    operation_id: &OperationId,
    error: BackupObjectStoreError,
) {
    let Ok(message) = FailureMessage::try_new(format!("backup artifact write failed: {error:?}"))
    else {
        return;
    };
    let _ = controllers
        .record_backup_transition(
            operation_id,
            BackupTransition::Failed {
                failure: BackupOperationFailure::ManifestWriteFailed { message },
            },
        )
        .await;
}

async fn record_backup_snapshot_failure(
    controllers: &OperationControllers,
    operation_id: &OperationId,
    error: BackupSnapshotError,
) {
    let Ok(message) = FailureMessage::try_new(format!("control-plane snapshot failed: {error}"))
    else {
        return;
    };
    let _ = controllers
        .record_backup_transition(
            operation_id,
            BackupTransition::Failed {
                failure: BackupOperationFailure::SnapshotFailed { message },
            },
        )
        .await;
}

async fn record_backup_encode_failure(
    controllers: &OperationControllers,
    operation_id: &OperationId,
    error: String,
) {
    let Ok(message) = FailureMessage::try_new(format!("backup artifact encode failed: {error}"))
    else {
        return;
    };
    let _ = controllers
        .record_backup_transition(
            operation_id,
            BackupTransition::Failed {
                failure: BackupOperationFailure::ManifestWriteFailed { message },
            },
        )
        .await;
}

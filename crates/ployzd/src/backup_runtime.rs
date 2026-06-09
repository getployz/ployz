//! Owned backup operation execution.

use crate::controllers::OperationControllers;
use crate::operation_lease::with_advisory_operation_lease;
use async_nats::jetstream::consumer::{AckPolicy, PullConsumer, pull};
use futures_util::{StreamExt, TryStreamExt};
use ployz_core::backup::{
    BackupArtifact, BackupBundle, BackupManifest, ControlPlaneKvSnapshot, KvBucketSnapshot,
    KvEntrySnapshot,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    BackupOperationFailure, BackupOperationState, BackupRunningStage, BackupTransition,
    FailureMessage, OperationOwnerLease, OperationStatus,
};
use ployz_nats::kv::{KV_CORE_BUCKET, KV_LOCKS_BUCKET};
use ployz_nats::objects::{AsyncNatsBackupObjectStore, BackupObjectStoreError};
use ployz_nats::observations::KV_OBS_BUCKET;
use ployz_nats::operations::{
    BackupCreateJob, KV_OPS_BUCKET, OperationStatusReadError, OperationStatusStoreError,
    PLZ_JOBS_STREAM, RecordBackupEventError,
};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;

const BACKUP_CREATE_WORKER_DURABLE: &str = "backup-create-workers";
const BACKUP_CREATE_JOB_FILTER: &str = "plz.v1.job.backup.create.*";
const BACKUP_CREATE_ACK_WAIT: Duration = Duration::from_secs(30);
const BACKUP_WORKER_RESTART_DELAY: Duration = Duration::from_secs(1);

const CONTROL_PLANE_KV_BUCKETS: [&str; 4] = [
    KV_CORE_BUCKET,
    KV_OPS_BUCKET,
    KV_OBS_BUCKET,
    KV_LOCKS_BUCKET,
];

#[derive(Clone)]
pub struct OwnedBackupLauncher {
    jetstream: async_nats::jetstream::Context,
    controllers: OperationControllers,
    backups: AsyncNatsBackupObjectStore,
    task_registry: BackupTaskRegistry,
}

impl OwnedBackupLauncher {
    #[must_use]
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        controllers: OperationControllers,
        backups: AsyncNatsBackupObjectStore,
        task_registry: BackupTaskRegistry,
    ) -> Self {
        Self {
            jetstream,
            controllers,
            backups,
            task_registry,
        }
    }

    pub async fn start_worker(&self) -> Result<(), BackupWorkerStartError> {
        let stream = self
            .jetstream
            .get_stream(PLZ_JOBS_STREAM)
            .await
            .map_err(|error| BackupWorkerStartError::OpenOperationStream {
                message: error.to_string(),
            })?;
        let consumer = stream
            .get_or_create_consumer(
                BACKUP_CREATE_WORKER_DURABLE,
                pull::Config {
                    durable_name: Some(BACKUP_CREATE_WORKER_DURABLE.to_owned()),
                    filter_subject: BACKUP_CREATE_JOB_FILTER.to_owned(),
                    ack_policy: AckPolicy::Explicit,
                    ack_wait: BACKUP_CREATE_ACK_WAIT,
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| BackupWorkerStartError::OpenConsumer {
                message: error.to_string(),
            })?;
        let worker = BackupWorker {
            consumer,
            controllers: self.controllers.clone(),
            backups: self.backups.clone(),
            snapshot_source: ControlPlaneSnapshotSource::new(self.jetstream.clone()),
        };

        self.task_registry.spawn(async move {
            worker.run_forever().await;
        });

        Ok(())
    }
}

struct BackupWorker {
    consumer: PullConsumer,
    controllers: OperationControllers,
    backups: AsyncNatsBackupObjectStore,
    snapshot_source: ControlPlaneSnapshotSource,
}

impl BackupWorker {
    async fn run_forever(self) {
        loop {
            let Ok(mut messages) = self.consumer.messages().await else {
                tokio::time::sleep(BACKUP_WORKER_RESTART_DELAY).await;
                continue;
            };

            while let Some(delivery) = messages.next().await {
                let message = match delivery {
                    Ok(message) => message,
                    Err(_error) => break,
                };
                match handle_backup_worker_message(
                    &self.controllers,
                    &self.backups,
                    &self.snapshot_source,
                    &message,
                )
                .await
                {
                    Ok(()) => {
                        let _ = message.ack().await;
                    }
                    Err(BackupWorkerMessageError::Poison { message: _message }) => {
                        let _ = message.ack().await;
                    }
                    Err(BackupWorkerMessageError::Execute(_error)) => {}
                }
            }

            tokio::time::sleep(BACKUP_WORKER_RESTART_DELAY).await;
        }
    }
}

async fn handle_backup_worker_message(
    controllers: &OperationControllers,
    backups: &AsyncNatsBackupObjectStore,
    snapshot_source: &ControlPlaneSnapshotSource,
    message: &async_nats::jetstream::Message,
) -> Result<(), BackupWorkerMessageError> {
    let job: BackupCreateJob =
        serde_json::from_slice(&message.message.payload).map_err(|error| {
            BackupWorkerMessageError::Poison {
                message: error.to_string(),
            }
        })?;

    run_owned_backup_create(
        controllers.clone(),
        backups.clone(),
        snapshot_source.clone(),
        job.operation_id,
    )
    .await
    .map_err(BackupWorkerMessageError::Execute)
}

async fn run_owned_backup_create(
    controllers: OperationControllers,
    backups: AsyncNatsBackupObjectStore,
    snapshot_source: ControlPlaneSnapshotSource,
    operation_id: OperationId,
) -> Result<(), BackupExecutionError> {
    let lease = controllers
        .claim_owner_lease(&operation_id)
        .await
        .map_err(BackupExecutionError::ClaimLease)?;
    verify_backup_lease(&lease, &operation_id, &controllers)?;
    let lease_policy = controllers.lease_policy();
    let lease_renewer = controllers.clone();
    let lease_operation_id = operation_id.clone();

    with_advisory_operation_lease(
        lease_operation_id,
        lease_policy,
        lease_renewer,
        async move { run_backup_create(&controllers, &backups, &snapshot_source, &operation_id).await },
    )
    .await
}

fn verify_backup_lease(
    lease: &OperationOwnerLease,
    operation_id: &OperationId,
    controllers: &OperationControllers,
) -> Result<(), BackupExecutionError> {
    if &lease.operation_id != operation_id {
        return Err(BackupExecutionError::LeaseOperationMismatch {
            operation_id: operation_id.clone(),
            lease: lease.clone(),
        });
    }
    if lease.owner_id != *controllers.owner_id() {
        return Err(BackupExecutionError::LeaseNotHeld {
            operation_id: operation_id.clone(),
            lease: lease.clone(),
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
    ClaimLease(OperationStatusStoreError),
    LeaseOperationMismatch {
        operation_id: OperationId,
        lease: OperationOwnerLease,
    },
    LeaseNotHeld {
        operation_id: OperationId,
        lease: OperationOwnerLease,
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

#[derive(Debug)]
pub enum BackupWorkerStartError {
    OpenOperationStream { message: String },
    OpenConsumer { message: String },
}

#[derive(Debug)]
enum BackupWorkerMessageError {
    Poison { message: String },
    Execute(BackupExecutionError),
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

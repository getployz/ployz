//! NATS-backed operation event and status adapters.

use async_nats::jetstream;
use async_nats::jetstream::message::PublishMessage;
use ployz_core::ids::{ContainerId, NodeId, OperationId, ServiceId};
use ployz_core::ops::{
    DeployProjection, DeployTransition, EventSequence, EventSequenceError, OperationEvent,
    OperationIdempotencyKey, OperationStatus, StatusProjectionError, project_deploy_transition,
};
use ployz_core::subjects::{
    op_cancelled, op_deploy_completed, op_deploy_failed, op_deploy_planning_started,
    op_deploy_running, op_deploy_submitted, op_watch,
};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::time::Duration;

use crate::streams::MessageId;

pub const PLZ_OPS_STREAM: &str = "PLZ_OPS";
pub const KV_OPS_BUCKET: &str = "KV_OPS";
const NATS_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationEventAppend {
    subject: String,
    message_id: MessageId,
    payload: OperationEvent,
}

impl OperationEventAppend {
    #[must_use]
    pub fn from_event(message_id: MessageId, payload: OperationEvent) -> Self {
        Self {
            subject: operation_event_subject(&payload),
            message_id,
            payload,
        }
    }

    #[must_use]
    pub fn deploy_submitted(
        operation_id: OperationId,
        service_id: ServiceId,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Self {
        Self::from_event(
            MessageId::new(format!("deploy.submit.{}", idempotency_key.as_str())),
            OperationEvent::DeploySubmitted {
                operation_id,
                service_id,
            },
        )
    }

    #[must_use]
    pub fn deploy_transition(operation_id: &OperationId, transition: &DeployTransition) -> Self {
        let event = deploy_transition_event(operation_id, transition);
        Self::from_event(transition_message_id(operation_id, transition), event)
    }

    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    #[must_use]
    pub fn message_id(&self) -> &MessageId {
        &self.message_id
    }

    #[must_use]
    pub fn payload(&self) -> &OperationEvent {
        &self.payload
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOperationEvent {
    pub sequence: EventSequence,
    pub duplicate: bool,
}

#[derive(Debug, Clone)]
pub struct AsyncNatsOperationEventLog {
    jetstream: jetstream::Context,
}

impl AsyncNatsOperationEventLog {
    #[must_use]
    pub fn new(jetstream: jetstream::Context) -> Self {
        Self { jetstream }
    }

    pub async fn append(
        &self,
        append: OperationEventAppend,
    ) -> Result<StoredOperationEvent, OperationEventLogError> {
        let payload =
            serde_json::to_vec(&append.payload).map_err(OperationEventLogError::EncodeEvent)?;
        let ack_future = with_event_timeout(
            "operation event publish request",
            self.jetstream.send_publish(
                append.subject,
                PublishMessage::build()
                    .payload(payload.into())
                    .message_id(append.message_id.as_str()),
            ),
        )
        .await?
        .map_err(|error| OperationEventLogError::PublishRequest {
            message: error.to_string(),
        })?;
        let ack = with_event_timeout("operation event publish ack", async { ack_future.await })
            .await?
            .map_err(|error| OperationEventLogError::PublishAck {
                message: error.to_string(),
            })?;

        Ok(StoredOperationEvent {
            sequence: EventSequence::try_new(ack.sequence).map_err(|error| {
                OperationEventLogError::InvalidAckSequence {
                    sequence: ack.sequence,
                    error,
                }
            })?,
            duplicate: ack.duplicate,
        })
    }

    pub async fn event_at_sequence(
        &self,
        sequence: EventSequence,
    ) -> Result<OperationEvent, OperationEventLogError> {
        let stream = with_event_timeout(
            "operation event stream lookup",
            self.jetstream.get_stream(PLZ_OPS_STREAM),
        )
        .await?
        .map_err(|error| OperationEventLogError::ReadEvent {
            message: error.to_string(),
        })?;
        let message = with_event_timeout(
            "operation event stream read",
            stream.get_raw_message(sequence.get()),
        )
        .await?
        .map_err(|error| OperationEventLogError::ReadEvent {
            message: error.to_string(),
        })?;

        serde_json::from_slice(&message.payload).map_err(OperationEventLogError::DecodeEvent)
    }
}

#[derive(Debug)]
pub enum OperationEventLogError {
    EncodeEvent(serde_json::Error),
    DecodeEvent(serde_json::Error),
    PublishRequest {
        message: String,
    },
    PublishAck {
        message: String,
    },
    ReadEvent {
        message: String,
    },
    Timeout {
        operation: &'static str,
    },
    InvalidAckSequence {
        sequence: u64,
        error: EventSequenceError,
    },
}

#[derive(Debug, Clone)]
pub struct AsyncNatsOperationStatusStore {
    bucket: jetstream::kv::Store,
}

#[derive(Debug, Clone)]
pub struct AsyncNatsOperationRepository {
    event_log: AsyncNatsOperationEventLog,
    status_store: AsyncNatsOperationStatusStore,
}

impl AsyncNatsOperationRepository {
    #[must_use]
    pub fn new(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
    ) -> Self {
        Self {
            event_log,
            status_store,
        }
    }

    pub async fn submit_deploy(
        &self,
        submission: DeployOperationSubmission,
    ) -> Result<StoredDeploySubmission, SubmitDeployError> {
        if let Some(existing) = self
            .status_store
            .deploy_submission(&submission.idempotency_key)
            .await
            .map_err(SubmitDeployError::StoreStatus)?
        {
            return Ok(existing);
        }

        let stored = self
            .event_log
            .append(OperationEventAppend::deploy_submitted(
                submission.operation_id.clone(),
                submission.service_id.clone(),
                &submission.idempotency_key,
            ))
            .await
            .map_err(SubmitDeployError::AppendEvent)?;
        let (operation_id, service_id) = if stored.duplicate {
            let original = self
                .event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(SubmitDeployError::AppendEvent)?;
            let OperationEvent::DeploySubmitted {
                operation_id,
                service_id,
            } = original
            else {
                return Err(SubmitDeployError::DuplicateSequenceMismatch {
                    sequence: stored.sequence,
                });
            };
            (operation_id, service_id)
        } else {
            (submission.operation_id, submission.service_id)
        };
        let status =
            OperationStatus::deploy_accepted(operation_id.clone(), service_id, stored.sequence);
        self.status_store
            .put_if_newer(&status)
            .await
            .map_err(SubmitDeployError::StoreStatus)?;
        let submitted = StoredDeploySubmission {
            operation_id,
            start_sequence: stored.sequence,
        };

        self.status_store
            .put_deploy_submission_if_absent(&submission.idempotency_key, &submitted)
            .await
            .map_err(SubmitDeployError::StoreStatus)
    }

    pub async fn record_deploy_transition(
        &self,
        operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<OperationStatusWrite, RecordDeployTransitionError> {
        let Some(existing) = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordDeployTransitionError::LoadStatus)?
        else {
            return Err(RecordDeployTransitionError::ProjectStatus(
                StatusProjectionError::MissingOperation {
                    operation_id: operation_id.clone(),
                },
            ));
        };
        let preview_sequence = next_event_sequence(&existing);
        let preview = project_deploy_transition(&existing, transition.clone(), preview_sequence)
            .map_err(RecordDeployTransitionError::ProjectStatus)?;
        let DeployProjection::Updated { .. } = preview else {
            return Ok(OperationStatusWrite::AlreadySatisfied {
                current_sequence: status_sequence(&existing),
            });
        };
        let attempted_append = OperationEventAppend::deploy_transition(operation_id, &transition);
        let attempted_event = attempted_append.payload().clone();

        let stored = self
            .event_log
            .append(attempted_append)
            .await
            .map_err(RecordDeployTransitionError::AppendEvent)?;
        let event = if stored.duplicate {
            self.event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(RecordDeployTransitionError::AppendEvent)?
        } else {
            attempted_event
        };
        let transition = deploy_transition_from_event(operation_id, event, stored.sequence)?;
        let current = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordDeployTransitionError::LoadStatus)?
            .unwrap_or(existing);
        let projection = project_deploy_transition(&current, transition, stored.sequence)
            .map_err(RecordDeployTransitionError::ProjectStatus)?;
        let DeployProjection::Updated { status } = projection else {
            return Ok(OperationStatusWrite::AlreadySatisfied {
                current_sequence: status_sequence(&current),
            });
        };

        self.put_status_with_retry(&status).await
    }

    pub async fn operation_status(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatus>, OperationStatusStoreError> {
        self.status_store.get(operation_id).await
    }

    async fn put_status_with_retry(
        &self,
        status: &OperationStatus,
    ) -> Result<OperationStatusWrite, RecordDeployTransitionError> {
        const MAX_STATUS_PROJECTION_ATTEMPTS: usize = 3;

        for _ in 0..MAX_STATUS_PROJECTION_ATTEMPTS {
            match self
                .status_store
                .put_if_newer(status)
                .await
                .map_err(RecordDeployTransitionError::StoreStatus)?
            {
                OperationStatusWrite::Stored { revision } => {
                    return Ok(OperationStatusWrite::Stored { revision });
                }
                OperationStatusWrite::AlreadySatisfied { current_sequence } => {
                    return Ok(OperationStatusWrite::AlreadySatisfied { current_sequence });
                }
                OperationStatusWrite::Stale {
                    current_sequence,
                    attempted_sequence,
                } if current_sequence >= attempted_sequence => {
                    return Ok(OperationStatusWrite::Stale {
                        current_sequence,
                        attempted_sequence,
                    });
                }
                OperationStatusWrite::Stale { .. } | OperationStatusWrite::Contended { .. } => {
                    continue;
                }
            }
        }

        Err(RecordDeployTransitionError::StatusProjectionContended)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOperationSubmission {
    pub operation_id: OperationId,
    pub service_id: ServiceId,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDeploySubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
}

#[derive(Debug)]
pub enum SubmitDeployError {
    AppendEvent(OperationEventLogError),
    StoreStatus(OperationStatusStoreError),
    DuplicateSequenceMismatch { sequence: EventSequence },
}

#[derive(Debug)]
pub enum RecordDeployTransitionError {
    LoadStatus(OperationStatusStoreError),
    AppendEvent(OperationEventLogError),
    ProjectStatus(StatusProjectionError),
    StoreStatus(OperationStatusStoreError),
    StoredEventMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
    StatusProjectionContended,
}

impl AsyncNatsOperationStatusStore {
    pub async fn from_jetstream(
        jetstream: &jetstream::Context,
    ) -> Result<Self, OperationStatusStoreError> {
        let bucket = with_status_timeout(
            "operation status bucket open",
            jetstream.get_key_value(KV_OPS_BUCKET),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::OpenBucket {
            bucket: KV_OPS_BUCKET,
            message: error.to_string(),
        })?;

        Ok(Self { bucket })
    }

    #[must_use]
    pub fn new(bucket: jetstream::kv::Store) -> Self {
        Self { bucket }
    }

    pub async fn put_if_newer(
        &self,
        status: &OperationStatus,
    ) -> Result<OperationStatusWrite, OperationStatusStoreError> {
        let key = operation_status_key(status_id(status));
        let incoming_sequence = status_sequence(status);
        let payload =
            serde_json::to_vec(status).map_err(OperationStatusStoreError::EncodeStatus)?;
        let Some(existing) = with_status_timeout(
            "operation status entry read",
            self.bucket.entry(key.clone()),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            let revision = match with_status_timeout(
                "operation status create",
                self.bucket.create(&key, payload.into()),
            )
            .await?
            {
                Ok(revision) => revision,
                Err(error) => {
                    return self
                        .classify_write_conflict(&key, incoming_sequence, error)
                        .await;
                }
            };
            return Ok(OperationStatusWrite::Stored {
                revision: KvRevision(revision),
            });
        };

        let current: OperationStatus = serde_json::from_slice(&existing.value)
            .map_err(OperationStatusStoreError::DecodeStatus)?;
        let current_sequence = status_sequence(&current);
        if current_sequence >= incoming_sequence {
            return Ok(OperationStatusWrite::Stale {
                current_sequence,
                attempted_sequence: incoming_sequence,
            });
        }

        let revision = match with_status_timeout(
            "operation status update",
            self.bucket.update(&key, payload.into(), existing.revision),
        )
        .await?
        {
            Ok(revision) => revision,
            Err(error) => {
                return self
                    .classify_write_conflict(&key, incoming_sequence, error)
                    .await;
            }
        };

        Ok(OperationStatusWrite::Stored {
            revision: KvRevision(revision),
        })
    }

    pub async fn deploy_submission(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredDeploySubmission>, OperationStatusStoreError> {
        let Some(payload) = with_status_timeout(
            "deploy submission get",
            self.bucket.get(deploy_submission_key(idempotency_key)),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(OperationStatusStoreError::DecodeSubmission)
    }

    pub async fn put_deploy_submission_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredDeploySubmission,
    ) -> Result<StoredDeploySubmission, OperationStatusStoreError> {
        if let Some(existing) = self.deploy_submission(idempotency_key).await? {
            return Ok(existing);
        }

        let key = deploy_submission_key(idempotency_key);
        let payload =
            serde_json::to_vec(submission).map_err(OperationStatusStoreError::EncodeSubmission)?;
        match with_status_timeout(
            "deploy submission create",
            self.bucket.create(&key, payload.into()),
        )
        .await?
        {
            Ok(_) => Ok(submission.clone()),
            Err(error) => {
                if let Some(existing) = self.deploy_submission(idempotency_key).await? {
                    Ok(existing)
                } else {
                    Err(OperationStatusStoreError::CasConflict {
                        message: error.to_string(),
                    })
                }
            }
        }
    }

    async fn classify_write_conflict(
        &self,
        key: &str,
        attempted_sequence: EventSequence,
        error: impl ToString,
    ) -> Result<OperationStatusWrite, OperationStatusStoreError> {
        let Some(existing) =
            with_status_timeout("operation status conflict read", self.bucket.entry(key))
                .await?
                .map_err(|error| OperationStatusStoreError::GetStatus {
                    message: error.to_string(),
                })?
        else {
            return Err(OperationStatusStoreError::CasConflict {
                message: error.to_string(),
            });
        };

        let current: OperationStatus = serde_json::from_slice(&existing.value)
            .map_err(OperationStatusStoreError::DecodeStatus)?;
        let current_sequence = status_sequence(&current);
        if current_sequence >= attempted_sequence {
            return Ok(OperationStatusWrite::Stale {
                current_sequence,
                attempted_sequence,
            });
        }

        Ok(OperationStatusWrite::Contended {
            current_sequence,
            attempted_sequence,
        })
    }

    pub async fn get(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatus>, OperationStatusStoreError> {
        let Some(payload) = with_status_timeout(
            "operation status get",
            self.bucket.get(operation_status_key(operation_id)),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(OperationStatusStoreError::DecodeStatus)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KvRevision(u64);

impl KvRevision {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatusWrite {
    Stored {
        revision: KvRevision,
    },
    AlreadySatisfied {
        current_sequence: EventSequence,
    },
    Stale {
        current_sequence: EventSequence,
        attempted_sequence: EventSequence,
    },
    Contended {
        current_sequence: EventSequence,
        attempted_sequence: EventSequence,
    },
}

#[derive(Debug)]
pub enum OperationStatusStoreError {
    OpenBucket {
        bucket: &'static str,
        message: String,
    },
    EncodeStatus(serde_json::Error),
    DecodeStatus(serde_json::Error),
    EncodeSubmission(serde_json::Error),
    DecodeSubmission(serde_json::Error),
    CasConflict {
        message: String,
    },
    GetStatus {
        message: String,
    },
    Timeout {
        operation: &'static str,
    },
}

#[must_use]
pub fn operation_status_key(operation_id: &OperationId) -> String {
    format!("ops.{}", operation_id.as_str())
}

#[must_use]
pub fn deploy_submission_key(idempotency_key: &OperationIdempotencyKey) -> String {
    format!("deploy_submissions.{}", idempotency_key.as_str())
}

fn status_id(status: &OperationStatus) -> &OperationId {
    match status {
        OperationStatus::Deploy { id, .. } => id,
    }
}

async fn with_event_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, OperationEventLogError> {
    tokio::time::timeout(NATS_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| OperationEventLogError::Timeout { operation })
}

async fn with_status_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, OperationStatusStoreError> {
    tokio::time::timeout(NATS_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| OperationStatusStoreError::Timeout { operation })
}

fn status_sequence(status: &OperationStatus) -> EventSequence {
    match status {
        OperationStatus::Deploy {
            last_event_sequence,
            ..
        } => *last_event_sequence,
    }
}

fn next_event_sequence(status: &OperationStatus) -> EventSequence {
    EventSequence::try_new(status_sequence(status).get() + 1)
        .expect("next event sequence stays greater than zero")
}

fn operation_event_subject(event: &OperationEvent) -> String {
    match event {
        OperationEvent::DeploySubmitted { operation_id, .. } => op_deploy_submitted(operation_id),
        OperationEvent::DeployPlanningStarted { operation_id } => {
            op_deploy_planning_started(operation_id)
        }
        OperationEvent::DeployRunning {
            operation_id,
            stage,
        } => op_deploy_running(operation_id, stage.clone()),
        OperationEvent::DeployContainerStarted {
            operation_id,
            node_id,
            container_id,
        } => op_deploy_container_started(operation_id, node_id, container_id),
        OperationEvent::DeployCompleted { operation_id } => op_deploy_completed(operation_id),
        OperationEvent::DeployFailed { operation_id, .. } => op_deploy_failed(operation_id),
        OperationEvent::Cancelled { operation_id, .. } => op_cancelled(operation_id),
    }
}

fn transition_message_id(operation_id: &OperationId, transition: &DeployTransition) -> MessageId {
    MessageId::new(format!(
        "deploy.event.{}.{}",
        operation_id.as_str(),
        deploy_transition_token(transition)
    ))
}

fn deploy_transition_event(
    operation_id: &OperationId,
    transition: &DeployTransition,
) -> OperationEvent {
    match transition {
        DeployTransition::Planning => OperationEvent::DeployPlanningStarted {
            operation_id: operation_id.clone(),
        },
        DeployTransition::Running { stage } => OperationEvent::DeployRunning {
            operation_id: operation_id.clone(),
            stage: stage.clone(),
        },
        DeployTransition::Completed => OperationEvent::DeployCompleted {
            operation_id: operation_id.clone(),
        },
        DeployTransition::Failed { failure } => OperationEvent::DeployFailed {
            operation_id: operation_id.clone(),
            failure: failure.clone(),
        },
        DeployTransition::Cancelled { reason } => OperationEvent::Cancelled {
            operation_id: operation_id.clone(),
            reason: reason.clone(),
        },
    }
}

fn deploy_transition_from_event(
    operation_id: &OperationId,
    event: OperationEvent,
    sequence: EventSequence,
) -> Result<DeployTransition, RecordDeployTransitionError> {
    match event {
        OperationEvent::DeployPlanningStarted {
            operation_id: event_operation_id,
        } if &event_operation_id == operation_id => Ok(DeployTransition::Planning),
        OperationEvent::DeployRunning {
            operation_id: event_operation_id,
            stage,
        } if &event_operation_id == operation_id => Ok(DeployTransition::Running { stage }),
        OperationEvent::DeployCompleted {
            operation_id: event_operation_id,
        } if &event_operation_id == operation_id => Ok(DeployTransition::Completed),
        OperationEvent::DeployFailed {
            operation_id: event_operation_id,
            failure,
        } if &event_operation_id == operation_id => Ok(DeployTransition::Failed { failure }),
        OperationEvent::Cancelled {
            operation_id: event_operation_id,
            reason,
        } if &event_operation_id == operation_id => Ok(DeployTransition::Cancelled { reason }),
        OperationEvent::DeploySubmitted { .. }
        | OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployContainerStarted { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::DeployFailed { .. }
        | OperationEvent::Cancelled { .. } => {
            Err(RecordDeployTransitionError::StoredEventMismatch {
                operation_id: operation_id.clone(),
                sequence,
            })
        }
    }
}

fn deploy_transition_token(transition: &DeployTransition) -> &'static str {
    match transition {
        DeployTransition::Planning => "planning.started",
        DeployTransition::Running { stage } => stage.as_subject(),
        DeployTransition::Completed => "completed",
        DeployTransition::Failed { .. } => "failed",
        DeployTransition::Cancelled { .. } => "cancelled",
    }
}

fn op_deploy_container_started(
    operation_id: &OperationId,
    node_id: &NodeId,
    container_id: &ContainerId,
) -> String {
    format!(
        "{}deploy.container.started.{}.{}",
        op_watch(operation_id).trim_end_matches('>'),
        node_id.as_str(),
        container_id.as_str()
    )
}

use async_nats::jetstream;
use async_nats::jetstream::message::PublishMessage;
use ployz_core::ids::{ContainerId, NodeId, OperationId, ServiceId};
use ployz_core::ops::{
    DeployTransition, EventSequence, EventSequenceError, OperationEvent, OperationIdempotencyKey,
};
use ployz_core::subjects::{
    op_cancelled, op_deploy_completed, op_deploy_failed, op_deploy_planning_started,
    op_deploy_running, op_deploy_submitted, op_watch,
};
use std::future::Future;

use crate::streams::MessageId;

use super::{NATS_OPERATION_TIMEOUT, PLZ_OPS_STREAM};

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

async fn with_event_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, OperationEventLogError> {
    tokio::time::timeout(NATS_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| OperationEventLogError::Timeout { operation })
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

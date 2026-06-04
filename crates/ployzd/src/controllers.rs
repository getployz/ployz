//! Controller workers for operation execution.

use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{
    DeployProjection, DeployTransition, EventSequence, OperationEvent, OperationStatus,
    project_deploy_transition,
};
use ployz_core::subjects::{
    op_cancelled, op_deploy_completed, op_deploy_failed, op_deploy_planning_started,
    op_deploy_running, op_deploy_submitted, op_watch,
};
use ployz_nats::streams::{
    DurableConsumerState, MessageId, OperationConsumer, OperationEventStream,
    OperationStreamMessage,
};
use std::fmt;

pub use ployz_core::ops::StatusProjectionError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySubmitCommand {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
    pub service_id: ServiceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSpine {
    events: OperationEventStream,
    statuses: OperationStatusProjection,
    submissions: Vec<SubmissionRecord>,
    deploy_submitted_consumer: DurableConsumerState,
}

impl OperationSpine {
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: OperationEventStream::default(),
            statuses: OperationStatusProjection::default(),
            submissions: Vec::new(),
            deploy_submitted_consumer: DurableConsumerState::default(),
        }
    }

    pub fn submit_deploy(&mut self, command: DeploySubmitCommand) -> AcceptedDeployOperation {
        if let Some(existing) = self
            .submissions
            .iter()
            .find(|submission| submission.idempotency_key == command.idempotency_key)
        {
            return existing.accepted.clone();
        }

        let event = OperationEvent::DeploySubmitted {
            operation_id: command.operation_id.clone(),
            service_id: command.service_id.clone(),
        };
        let append = self.events.append(
            op_deploy_submitted(&command.operation_id),
            MessageId::new(format!(
                "deploy.submit.{}",
                command.idempotency_key.as_str()
            )),
            event,
        );
        let sequence = append.sequence();
        let status = OperationStatus::deploy_accepted(
            command.operation_id.clone(),
            command.service_id,
            sequence,
        );
        self.statuses.put_projected(status);

        let accepted = AcceptedDeployOperation {
            operation_id: command.operation_id,
            start_sequence: sequence,
        };
        self.submissions.push(SubmissionRecord {
            idempotency_key: command.idempotency_key,
            accepted: accepted.clone(),
        });

        accepted
    }

    #[must_use]
    pub fn status(&self, operation_id: &OperationId) -> Option<&OperationStatus> {
        self.statuses.get(operation_id)
    }

    #[must_use]
    pub fn watch(
        &self,
        operation_id: &OperationId,
        start_sequence: EventSequence,
    ) -> Vec<OperationStreamMessage> {
        self.events.replay(
            &op_watch(operation_id).trim_end_matches('>').to_owned(),
            start_sequence,
        )
    }

    #[must_use]
    pub fn deploy_submitted_consumer(&self) -> OperationConsumer {
        OperationConsumer::new(
            self.events
                .messages()
                .iter()
                .filter(|message| {
                    matches!(message.payload, OperationEvent::DeploySubmitted { .. })
                        && !self.deploy_submitted_consumer.is_acked(message.sequence)
                })
                .cloned()
                .collect(),
        )
    }

    pub fn ack_deploy_submitted(&mut self, sequence: EventSequence) {
        self.deploy_submitted_consumer.ack(sequence);
    }

    pub fn record_deploy_transition(
        &mut self,
        operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<(), StatusProjectionError> {
        let Some(existing) = self.statuses.get(operation_id) else {
            return Err(StatusProjectionError::MissingOperation {
                operation_id: operation_id.clone(),
            });
        };
        let projection =
            project_deploy_transition(existing, transition.clone(), self.events.next_sequence())?;
        let DeployProjection::Updated { status } = projection else {
            return Ok(());
        };
        let append = self.events.append(
            deploy_transition_subject(operation_id, &transition),
            MessageId::new(format!(
                "deploy.state.{}.{}",
                operation_id.as_str(),
                deploy_transition_token(&transition),
            )),
            deploy_transition_event(operation_id, &transition),
        );
        debug_assert_eq!(append.sequence(), status_sequence(&status));
        self.statuses.put_projected(status);
        Ok(())
    }
}

impl Default for OperationSpine {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubmissionRecord {
    idempotency_key: IdempotencyKey,
    accepted: AcceptedDeployOperation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedDeployOperation {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationStatusProjection {
    statuses: Vec<OperationStatus>,
}

impl OperationStatusProjection {
    #[must_use]
    pub fn new() -> Self {
        Self {
            statuses: Vec::new(),
        }
    }

    #[must_use]
    pub fn get(&self, operation_id: &OperationId) -> Option<&OperationStatus> {
        self.statuses
            .iter()
            .find(|status| status_id(status) == operation_id)
    }

    pub fn put_projected(&mut self, status: OperationStatus) {
        let operation_id = status_id(&status).clone();
        if let Some(existing) = self
            .statuses
            .iter_mut()
            .find(|existing| status_id(existing) == &operation_id)
        {
            *existing = status;
            return;
        }

        self.statuses.push(status);
    }
}

impl Default for OperationStatusProjection {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployWorker;

impl DeployWorker {
    pub fn mark_planning(
        spine: &mut OperationSpine,
        message: &OperationStreamMessage,
    ) -> Result<(), StatusProjectionError> {
        let OperationEvent::DeploySubmitted { operation_id, .. } = &message.payload else {
            return Ok(());
        };

        spine.record_deploy_transition(operation_id, DeployTransition::Planning)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, IdempotencyKeyError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(IdempotencyKeyError::Empty);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyKeyError {
    Empty,
}

impl fmt::Display for IdempotencyKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("idempotency key is empty"),
        }
    }
}

fn status_id(status: &OperationStatus) -> &OperationId {
    match status {
        OperationStatus::Deploy { id, .. } => id,
    }
}

fn status_sequence(status: &OperationStatus) -> EventSequence {
    match status {
        OperationStatus::Deploy {
            last_event_sequence,
            ..
        } => *last_event_sequence,
    }
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

fn deploy_transition_subject(operation_id: &OperationId, transition: &DeployTransition) -> String {
    match transition {
        DeployTransition::Planning => op_deploy_planning_started(operation_id),
        DeployTransition::Running { stage } => op_deploy_running(operation_id, stage.clone()),
        DeployTransition::Completed => op_deploy_completed(operation_id),
        DeployTransition::Failed { .. } => op_deploy_failed(operation_id),
        DeployTransition::Cancelled { .. } => op_cancelled(operation_id),
    }
}

fn deploy_transition_token(transition: &DeployTransition) -> String {
    match transition {
        DeployTransition::Planning => "planning.started".to_owned(),
        DeployTransition::Running { stage } => {
            format!("running.{}", stage.as_subject())
        }
        DeployTransition::Completed => "completed".to_owned(),
        DeployTransition::Failed { .. } => "failed".to_owned(),
        DeployTransition::Cancelled { .. } => "cancelled".to_owned(),
    }
}

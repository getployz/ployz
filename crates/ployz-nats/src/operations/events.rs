use async_nats::jetstream;
use async_nats::jetstream::message::PublishMessage;
use async_nats::jetstream::message::StreamMessage;
use async_nats::jetstream::stream::{LastRawMessageErrorKind, Stream};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{CertId, ContainerId, MachineId, OperationId};
use ployz_core::install::InstallArtifactVersion;
use ployz_core::machine::{IssuedJoinToken, JoinTokenRedeemedAt, MachineAddFailure, MachineName};
use ployz_core::ops::{
    CertOperationFailure, DeployCompletionOutcome, DeployEvidence, DeployTransition, EventSequence,
    EventSequenceError, MachineSubstrateVersions, MachineUpdateFailure, OperationEvent,
    OperationEventReplayLimit, OperationEventReplayPage, ReplayedOperationEvent,
};
use ployz_core::roles::InstallRolePolicy;
use ployz_core::subjects::{
    op_cancelled, op_cert_challenge_published, op_cert_completed, op_cert_failed,
    op_cert_submitted, op_cert_validation_started, op_deploy_cleanup_finished, op_deploy_completed,
    op_deploy_container_started, op_deploy_dataplane_prepared, op_deploy_failed,
    op_deploy_health_check_started, op_deploy_plan_created, op_deploy_planning_started,
    op_deploy_running, op_deploy_submitted, op_machine_add_completed,
    op_machine_add_credential_provisioned, op_machine_add_failed, op_machine_add_joined,
    op_machine_add_submitted, op_machine_update_completed, op_machine_update_failed,
    op_machine_update_running, op_machine_update_submitted, op_watch,
};

use crate::kv::{NatsIoTimeout, with_io_timeout};
use crate::streams::MessageId;

use super::PLZ_OPS_STREAM;

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
    pub fn deploy_submitted(operation_id: OperationId, target: DeployRequest) -> Self {
        let message_operation_id = operation_id.clone();
        Self::from_event(
            submitted_message_id(&message_operation_id),
            OperationEvent::DeploySubmitted {
                operation_id,
                target,
            },
        )
    }

    #[must_use]
    pub fn deploy_transition(operation_id: &OperationId, transition: &DeployTransition) -> Self {
        let event = deploy_transition_event(operation_id, transition);
        Self::from_event(transition_message_id(operation_id, transition), event)
    }

    #[must_use]
    pub fn deploy_evidence(operation_id: &OperationId, evidence: &DeployEvidence) -> Self {
        Self::from_event(
            evidence_message_id(operation_id, evidence),
            evidence.event(operation_id),
        )
    }

    #[must_use]
    pub fn deploy_plan_created(
        operation_id: &OperationId,
        plan: &ployz_core::deploy::DeployPlan,
    ) -> Self {
        Self::deploy_evidence(
            operation_id,
            &DeployEvidence::PlanCreated { plan: plan.clone() },
        )
    }

    #[must_use]
    pub fn deploy_container_started(
        operation_id: &OperationId,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Self {
        Self::deploy_evidence(
            operation_id,
            &DeployEvidence::ContainerStarted {
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
            },
        )
    }

    #[must_use]
    pub fn deploy_health_check_started(operation_id: &OperationId) -> Self {
        Self::deploy_evidence(operation_id, &DeployEvidence::HealthCheckStarted)
    }

    #[must_use]
    pub fn cert_submitted(operation_id: OperationId, cert_id: CertId) -> Self {
        let message_operation_id = operation_id.clone();
        Self::from_event(
            submitted_message_id(&message_operation_id),
            OperationEvent::CertRenewalSubmitted {
                operation_id,
                cert_id,
            },
        )
    }

    #[must_use]
    pub fn machine_add_submitted(
        operation_id: OperationId,
        machine_id: MachineId,
        name: MachineName,
        roles: InstallRolePolicy,
        join_token: IssuedJoinToken,
    ) -> Self {
        let message_operation_id = operation_id.clone();
        Self::from_event(
            submitted_message_id(&message_operation_id),
            OperationEvent::MachineAddSubmitted {
                operation_id,
                machine_id,
                name,
                roles,
                join_token,
            },
        )
    }

    #[must_use]
    pub fn machine_add_joined(
        operation_id: &OperationId,
        machine_id: &MachineId,
        joined_at: JoinTokenRedeemedAt,
    ) -> Self {
        Self::from_event(
            MessageId::new(format!("machine.add.joined.{}", operation_id.as_str())),
            OperationEvent::MachineAddJoined {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                joined_at,
            },
        )
    }

    #[must_use]
    pub fn machine_add_credential_provisioned(
        operation_id: &OperationId,
        machine_id: &MachineId,
        step: ployz_core::machine::MachineCredentialProvisioningStep,
    ) -> Self {
        Self::from_event(
            MessageId::new(format!(
                "machine.add.credential.{}.{}",
                step.as_subject_token(),
                operation_id.as_str()
            )),
            OperationEvent::MachineAddCredentialProvisioned {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                step,
            },
        )
    }

    #[must_use]
    pub fn machine_add_completed(operation_id: &OperationId, machine_id: &MachineId) -> Self {
        Self::from_event(
            MessageId::new(format!("machine.add.terminal.{}", operation_id.as_str())),
            OperationEvent::MachineAddCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            },
        )
    }

    #[must_use]
    pub fn machine_add_failed(
        operation_id: &OperationId,
        machine_id: &MachineId,
        failure: MachineAddFailure,
    ) -> Self {
        Self::from_event(
            MessageId::new(format!("machine.add.terminal.{}", operation_id.as_str())),
            OperationEvent::MachineAddFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure,
            },
        )
    }

    #[must_use]
    pub fn machine_update_submitted(
        operation_id: OperationId,
        machine_id: MachineId,
        target_version: InstallArtifactVersion,
    ) -> Self {
        let message_operation_id = operation_id.clone();
        Self::from_event(
            submitted_message_id(&message_operation_id),
            OperationEvent::MachineUpdateSubmitted {
                operation_id,
                machine_id,
                target_version,
            },
        )
    }

    #[must_use]
    pub fn machine_update_running(operation_id: &OperationId, machine_id: &MachineId) -> Self {
        Self::from_event(
            MessageId::new(format!("machine.update.running.{}", operation_id.as_str())),
            OperationEvent::MachineUpdateRunning {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            },
        )
    }

    #[must_use]
    pub fn machine_update_completed(
        operation_id: &OperationId,
        machine_id: &MachineId,
        reported: MachineSubstrateVersions,
    ) -> Self {
        Self::from_event(
            MessageId::new(format!("machine.update.terminal.{}", operation_id.as_str())),
            OperationEvent::MachineUpdateCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                reported,
            },
        )
    }

    #[must_use]
    pub fn machine_update_failed(
        operation_id: &OperationId,
        machine_id: &MachineId,
        failure: MachineUpdateFailure,
    ) -> Self {
        Self::from_event(
            MessageId::new(format!("machine.update.terminal.{}", operation_id.as_str())),
            OperationEvent::MachineUpdateFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure,
            },
        )
    }

    #[must_use]
    pub fn cert_challenge_published(
        operation_id: &OperationId,
        cert_id: &CertId,
        challenge: ployz_core::cert::AcmeHttp01Challenge,
    ) -> Self {
        Self::from_event(
            MessageId::new(format!(
                "cert.challenge.published.{}",
                operation_id.as_str()
            )),
            OperationEvent::CertChallengePublished {
                operation_id: operation_id.clone(),
                cert_id: cert_id.clone(),
                challenge,
            },
        )
    }

    #[must_use]
    pub fn cert_validation_started(operation_id: &OperationId, cert_id: &CertId) -> Self {
        Self::from_event(
            MessageId::new(format!("cert.validation.started.{}", operation_id.as_str())),
            OperationEvent::CertValidationStarted {
                operation_id: operation_id.clone(),
                cert_id: cert_id.clone(),
            },
        )
    }

    #[must_use]
    pub fn cert_completed(
        operation_id: &OperationId,
        active_cert: ployz_core::cert::ActiveCertState,
    ) -> Self {
        Self::from_event(
            MessageId::new(format!("cert.terminal.{}", operation_id.as_str())),
            OperationEvent::CertCompleted {
                operation_id: operation_id.clone(),
                active_cert,
            },
        )
    }

    #[must_use]
    pub fn cert_failed(operation_id: &OperationId, failure: CertOperationFailure) -> Self {
        Self::from_event(
            MessageId::new(format!("cert.terminal.{}", operation_id.as_str())),
            OperationEvent::CertFailed {
                operation_id: operation_id.clone(),
                failure,
            },
        )
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
        let publish = PublishMessage::build()
            .payload(payload.into())
            .message_id(append.message_id.as_str());
        let ack_future = with_io_timeout(
            "operation event publish request",
            self.jetstream.send_publish(append.subject, publish),
        )
        .await?
        .map_err(|error| OperationEventLogError::PublishRequest {
            message: error.to_string(),
        })?;
        let ack = with_io_timeout("operation event publish ack", async { ack_future.await })
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
        let stream = self.operation_stream().await?;
        let message = with_io_timeout(
            "operation event stream read",
            stream.get_raw_message(sequence.get()),
        )
        .await?
        .map_err(|error| OperationEventLogError::ReadEvent {
            message: error.to_string(),
        })?;

        serde_json::from_slice(&message.payload).map_err(OperationEventLogError::DecodeEvent)
    }

    pub async fn replay_operation(
        &self,
        operation_id: &OperationId,
        start_sequence: EventSequence,
        limit: OperationEventReplayLimit,
    ) -> Result<OperationEventReplayPage, OperationEventReplayReadError> {
        let stream = self.operation_stream_for_replay().await?;
        let subject = op_watch(operation_id);
        let mut next_sequence = start_sequence.get();
        let limit = limit.as_usize();
        let mut events = Vec::with_capacity(limit);

        while events.len() < limit {
            let Some(message) =
                next_replay_message(&stream, subject.as_str(), next_sequence).await?
            else {
                return Ok(OperationEventReplayPage::caught_up(events));
            };
            let replayed = replayed_event_from_replay_message(message.sequence, &message.payload)?;
            events.push(replayed);
            next_sequence = message.sequence.checked_add(1).ok_or(
                OperationEventReplayReadError::InvalidNextReplaySequence {
                    sequence: message.sequence,
                },
            )?;
        }

        match next_replay_message(&stream, subject.as_str(), next_sequence).await? {
            Some(message) => Ok(OperationEventReplayPage::more(
                events,
                event_sequence_from_replay_u64(message.sequence)?,
            )),
            None => Ok(OperationEventReplayPage::caught_up(events)),
        }
    }

    pub async fn event_at_subject(
        &self,
        subject: &str,
    ) -> Result<Option<(StoredOperationEvent, OperationEvent)>, OperationEventLogError> {
        let stream = self.operation_stream().await?;
        let message = match with_io_timeout(
            "operation event subject read",
            stream.get_last_raw_message_by_subject(subject),
        )
        .await?
        {
            Ok(message) => message,
            Err(error) if error.kind() == LastRawMessageErrorKind::NoMessageFound => {
                return Ok(None);
            }
            Err(error) => {
                return Err(OperationEventLogError::ReadEvent {
                    message: error.to_string(),
                });
            }
        };
        let sequence = EventSequence::try_new(message.sequence).map_err(|error| {
            OperationEventLogError::InvalidAckSequence {
                sequence: message.sequence,
                error,
            }
        })?;
        let event = serde_json::from_slice(&message.payload)
            .map_err(OperationEventLogError::DecodeEvent)?;

        Ok(Some((
            StoredOperationEvent {
                sequence,
                duplicate: true,
            },
            event,
        )))
    }

    async fn operation_stream(&self) -> Result<Stream, OperationEventLogError> {
        with_io_timeout(
            "operation event stream lookup",
            self.jetstream.get_stream(PLZ_OPS_STREAM),
        )
        .await?
        .map_err(|error| OperationEventLogError::ReadEvent {
            message: error.to_string(),
        })
    }

    async fn operation_stream_for_replay(&self) -> Result<Stream, OperationEventReplayReadError> {
        with_io_timeout(
            "operation event stream lookup",
            self.jetstream.get_stream(PLZ_OPS_STREAM),
        )
        .await?
        .map_err(|error| OperationEventReplayReadError::ReadEvent {
            message: error.to_string(),
        })
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

#[derive(Debug)]
pub enum OperationEventReplayReadError {
    DecodeEvent(serde_json::Error),
    ReadEvent {
        message: String,
    },
    Timeout {
        operation: &'static str,
    },
    InvalidEventSequence {
        sequence: u64,
        error: EventSequenceError,
    },
    InvalidNextReplaySequence {
        sequence: u64,
    },
}

impl From<NatsIoTimeout> for OperationEventLogError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

impl From<NatsIoTimeout> for OperationEventReplayReadError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

fn operation_event_subject(event: &OperationEvent) -> String {
    match event {
        OperationEvent::DeploySubmitted { operation_id, .. } => op_deploy_submitted(operation_id),
        OperationEvent::DeployPlanningStarted { operation_id } => {
            op_deploy_planning_started(operation_id)
        }
        OperationEvent::DeployPlanCreated { operation_id, .. } => {
            op_deploy_plan_created(operation_id)
        }
        OperationEvent::DeployRunning {
            operation_id,
            stage,
        } => op_deploy_running(operation_id, *stage),
        OperationEvent::DeployDataplanePrepared { operation_id, .. } => {
            op_deploy_dataplane_prepared(operation_id)
        }
        OperationEvent::DeployContainerStarted {
            operation_id,
            machine_id,
            container_id,
        } => op_deploy_container_started(operation_id, machine_id, container_id),
        OperationEvent::DeployHealthCheckStarted { operation_id } => {
            op_deploy_health_check_started(operation_id)
        }
        OperationEvent::DeployCleanupFinished { operation_id, .. } => {
            op_deploy_cleanup_finished(operation_id)
        }
        OperationEvent::DeployCompleted { operation_id, .. } => op_deploy_completed(operation_id),
        OperationEvent::DeployFailed { operation_id, .. } => op_deploy_failed(operation_id),
        OperationEvent::CertRenewalSubmitted { operation_id, .. } => {
            op_cert_submitted(operation_id)
        }
        OperationEvent::CertChallengePublished { operation_id, .. } => {
            op_cert_challenge_published(operation_id)
        }
        OperationEvent::CertValidationStarted { operation_id, .. } => {
            op_cert_validation_started(operation_id)
        }
        OperationEvent::CertCompleted { operation_id, .. } => op_cert_completed(operation_id),
        OperationEvent::CertFailed { operation_id, .. } => op_cert_failed(operation_id),
        OperationEvent::MachineAddSubmitted { operation_id, .. } => {
            op_machine_add_submitted(operation_id)
        }
        OperationEvent::MachineAddJoined { operation_id, .. } => {
            op_machine_add_joined(operation_id)
        }
        OperationEvent::MachineAddCredentialProvisioned {
            operation_id, step, ..
        } => op_machine_add_credential_provisioned(operation_id, *step),
        OperationEvent::MachineAddCompleted { operation_id, .. } => {
            op_machine_add_completed(operation_id)
        }
        OperationEvent::MachineAddFailed { operation_id, .. } => {
            op_machine_add_failed(operation_id)
        }
        OperationEvent::MachineUpdateSubmitted { operation_id, .. } => {
            op_machine_update_submitted(operation_id)
        }
        OperationEvent::MachineUpdateRunning { operation_id, .. } => {
            op_machine_update_running(operation_id)
        }
        OperationEvent::MachineUpdateCompleted { operation_id, .. } => {
            op_machine_update_completed(operation_id)
        }
        OperationEvent::MachineUpdateFailed { operation_id, .. } => {
            op_machine_update_failed(operation_id)
        }
        OperationEvent::Cancelled { operation_id, .. } => op_cancelled(operation_id),
    }
}

async fn next_replay_message(
    stream: &Stream,
    subject: &str,
    sequence: u64,
) -> Result<Option<StreamMessage>, OperationEventReplayReadError> {
    match with_io_timeout(
        "operation event replay read",
        stream
            .raw_message_builder()
            .sequence(sequence)
            .next_by_subject(subject)
            .send(),
    )
    .await?
    {
        Ok(message) => Ok(Some(message)),
        Err(error) if error.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(None),
        Err(error) => Err(OperationEventReplayReadError::ReadEvent {
            message: error.to_string(),
        }),
    }
}

fn replayed_event_from_replay_message(
    sequence: u64,
    payload: &[u8],
) -> Result<ReplayedOperationEvent, OperationEventReplayReadError> {
    let sequence = event_sequence_from_replay_u64(sequence)?;
    let event =
        serde_json::from_slice(payload).map_err(OperationEventReplayReadError::DecodeEvent)?;

    Ok(ReplayedOperationEvent { sequence, event })
}

fn submitted_message_id(operation_id: &OperationId) -> MessageId {
    MessageId::new(format!("operation.submit.{}", operation_id.as_str()))
}

fn event_sequence_from_replay_u64(
    sequence: u64,
) -> Result<EventSequence, OperationEventReplayReadError> {
    EventSequence::try_new(sequence)
        .map_err(|error| OperationEventReplayReadError::InvalidEventSequence { sequence, error })
}

fn transition_message_id(operation_id: &OperationId, transition: &DeployTransition) -> MessageId {
    if deploy_transition_is_terminal(transition) {
        return MessageId::new(format!("deploy.terminal.{}", operation_id.as_str()));
    }

    MessageId::new(format!(
        "deploy.event.{}.{}",
        operation_id.as_str(),
        deploy_transition_token(transition)
    ))
}

fn evidence_message_id(operation_id: &OperationId, evidence: &DeployEvidence) -> MessageId {
    let value = match evidence {
        DeployEvidence::PlanCreated { .. } => {
            format!("deploy.plan.created.{}", operation_id.as_str())
        }
        DeployEvidence::DataplanePrepared { .. } => {
            format!("deploy.dataplane.prepared.{}", operation_id.as_str())
        }
        DeployEvidence::ContainerStarted {
            machine_id,
            container_id,
        } => format!(
            "deploy.container.started.{}.{}.{}",
            operation_id.as_str(),
            machine_id.as_str(),
            container_id.as_str()
        ),
        DeployEvidence::HealthCheckStarted => {
            format!("deploy.health_check.started.{}", operation_id.as_str())
        }
        DeployEvidence::CleanupFinished { .. } => {
            format!("deploy.cleanup.finished.{}", operation_id.as_str())
        }
    };
    MessageId::new(value)
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
            stage: *stage,
        },
        DeployTransition::Completed { outcome } => OperationEvent::DeployCompleted {
            operation_id: operation_id.clone(),
            outcome: *outcome,
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

const fn deploy_transition_is_terminal(transition: &DeployTransition) -> bool {
    matches!(
        transition,
        DeployTransition::Completed { .. }
            | DeployTransition::Failed { .. }
            | DeployTransition::Cancelled { .. }
    )
}

fn deploy_transition_token(transition: &DeployTransition) -> String {
    match transition {
        DeployTransition::Planning => "planning.started".to_owned(),
        DeployTransition::Running { stage } => {
            format!("running.{}", stage.as_subject())
        }
        DeployTransition::Completed {
            outcome: DeployCompletionOutcome::Completed,
        } => "completed".to_owned(),
        DeployTransition::Completed { outcome } => {
            format!("completed.{}", outcome.as_subject())
        }
        DeployTransition::Failed { .. } => "failed".to_owned(),
        DeployTransition::Cancelled { .. } => "cancelled".to_owned(),
    }
}

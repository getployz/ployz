//! The Machine Add operation: a join token is minted, the machine joins,
//! credentials are provisioned, and the machine activates. States,
//! transitions, and status projection live together here; the failure type
//! stays in `crate::machine` beside the join-flow evidence it renders.

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, OperationId};
use crate::machine::InstallRolePolicy;
use crate::machine::{IssuedJoinToken, JoinTokenRedeemedAt, MachineAddFailure, MachineName};

use super::events::OperationSubjectRef;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
    verify_subject,
};
use super::text::CancellationReason;
use super::{EventSequence, OperationStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineAddOperationState {
    Pending { join_token: IssuedJoinToken },
    Joining { joined_at: JoinTokenRedeemedAt },
    Completed,
    Failed { failure: MachineAddFailure },
    Cancelled { reason: CancellationReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineAddOperationStateName {
    Pending,
    Joining,
    Completed,
    Failed,
    Cancelled,
}

impl MachineAddOperationStateName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Joining => "joining",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl MachineAddOperationState {
    #[must_use]
    pub const fn name(&self) -> MachineAddOperationStateName {
        match self {
            Self::Pending { .. } => MachineAddOperationStateName::Pending,
            Self::Joining { .. } => MachineAddOperationStateName::Joining,
            Self::Completed => MachineAddOperationStateName::Completed,
            Self::Failed { .. } => MachineAddOperationStateName::Failed,
            Self::Cancelled { .. } => MachineAddOperationStateName::Cancelled,
        }
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}

/// Machine-add events after classification from the flat
/// [`super::OperationEvent`] stream shape. Subject-bearing variants carry
/// the machine id the event claims, verified against the status record; a
/// cancel names no subject.
pub(super) enum MachineAddEvent {
    Submitted {
        machine_id: MachineId,
    },
    CredentialProvisioned {
        machine_id: MachineId,
    },
    Transition {
        machine_id: MachineId,
        state: MachineAddOperationState,
    },
    Cancelled(CancellationReason),
}

/// The destructured fields of [`OperationStatus::MachineAdd`].
#[derive(Clone, Copy)]
pub(super) struct MachineAddFields<'status> {
    pub(super) id: &'status OperationId,
    pub(super) machine_id: &'status MachineId,
    pub(super) name: &'status MachineName,
    pub(super) roles: InstallRolePolicy,
    pub(super) host_port_assurance: crate::install::HostPortAssurance,
    pub(super) state: &'status MachineAddOperationState,
}

impl MachineAddFields<'_> {
    fn status_with(
        &self,
        state: MachineAddOperationState,
        event_sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::MachineAdd {
            id: self.id.clone(),
            machine_id: self.machine_id.clone(),
            name: self.name.clone(),
            roles: self.roles,
            host_port_assurance: self.host_port_assurance,
            state,
            last_event_sequence: event_sequence,
        }
    }
}

pub(super) fn project_event(
    fields: MachineAddFields<'_>,
    event: MachineAddEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        MachineAddEvent::Submitted {
            machine_id: event_machine_id,
        } => {
            verify_subject(
                fields.id,
                fields.machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineAdd,
            )?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        MachineAddEvent::CredentialProvisioned {
            machine_id: event_machine_id,
        } => {
            verify_subject(
                fields.id,
                fields.machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineAdd,
            )?;
            project_credential_evidence(fields, event_sequence)
        }
        MachineAddEvent::Transition {
            machine_id: event_machine_id,
            state: attempted,
        } => {
            verify_subject(
                fields.id,
                fields.machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineAdd,
            )?;
            project_state(fields, attempted, event_sequence)
        }
        MachineAddEvent::Cancelled(reason) => project_state(
            fields,
            MachineAddOperationState::Cancelled { reason },
            event_sequence,
        ),
    }
}

/// Credential-provisioning steps are evidence: they advance the status
/// cursor without changing the machine-add state. They are only recorded
/// while the operation is live; once terminal, the evidence is satisfied.
fn project_credential_evidence(
    fields: MachineAddFields<'_>,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if fields.state.is_terminal() {
        return Ok(OperationProjection::AlreadySatisfied);
    }

    Ok(OperationProjection::StatusChanged {
        status: Box::new(fields.status_with(fields.state.clone(), event_sequence)),
    })
}

pub(super) fn project_state(
    fields: MachineAddFields<'_>,
    attempted: MachineAddOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    // A repeated join is idempotent regardless of the recorded joined_at, so a
    // racing second redeem adopts the existing Joining state rather than erroring.
    if matches!(
        (fields.state, &attempted),
        (
            MachineAddOperationState::Joining { .. },
            MachineAddOperationState::Joining { .. }
        )
    ) {
        return Ok(OperationProjection::AlreadySatisfied);
    }
    project_transition(
        fields.id,
        fields.state,
        attempted,
        MachineAddOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::MachineAdd,
        |attempted| fields.status_with(attempted, event_sequence),
    )
}

fn transition_allowed(
    current: &MachineAddOperationState,
    attempted: &MachineAddOperationState,
) -> bool {
    match (current, attempted) {
        (
            MachineAddOperationState::Pending { .. },
            MachineAddOperationState::Joining { .. } | MachineAddOperationState::Cancelled { .. },
        )
        | (
            MachineAddOperationState::Joining { .. },
            MachineAddOperationState::Completed | MachineAddOperationState::Cancelled { .. },
        ) => true,
        (
            MachineAddOperationState::Pending { .. } | MachineAddOperationState::Joining { .. },
            MachineAddOperationState::Failed { failure },
        ) => failure_allowed(current, failure),
        (
            MachineAddOperationState::Pending { .. } | MachineAddOperationState::Joining { .. },
            MachineAddOperationState::Pending { .. },
        )
        | (MachineAddOperationState::Joining { .. }, MachineAddOperationState::Joining { .. })
        | (MachineAddOperationState::Pending { .. }, MachineAddOperationState::Completed)
        | (
            MachineAddOperationState::Completed
            | MachineAddOperationState::Failed { .. }
            | MachineAddOperationState::Cancelled { .. },
            _,
        ) => false,
    }
}

fn failure_allowed(current: &MachineAddOperationState, failure: &MachineAddFailure) -> bool {
    match (current, failure) {
        (
            MachineAddOperationState::Pending { .. },
            MachineAddFailure::InvalidJoinToken
            | MachineAddFailure::JoinTokenExpired { .. }
            | MachineAddFailure::AuthorizationRenderFailed { .. }
            | MachineAddFailure::NatsReloadFailed { .. }
            | MachineAddFailure::MintedCredentialUnusable { .. }
            | MachineAddFailure::ControlTaskInterrupted { .. }
            | MachineAddFailure::CredentialEvidenceWriteFailed { .. },
        )
        | (
            MachineAddOperationState::Joining { .. },
            MachineAddFailure::BootstrapFailed { .. }
            | MachineAddFailure::ReadinessFailed { .. }
            | MachineAddFailure::DataplaneProjectionAdmissionFailed { .. },
        ) => true,
        (
            MachineAddOperationState::Pending { .. },
            MachineAddFailure::BootstrapFailed { .. }
            | MachineAddFailure::ReadinessFailed { .. }
            | MachineAddFailure::DataplaneProjectionAdmissionFailed { .. },
        )
        | (
            MachineAddOperationState::Joining { .. },
            MachineAddFailure::InvalidJoinToken
            | MachineAddFailure::JoinTokenExpired { .. }
            | MachineAddFailure::AuthorizationRenderFailed { .. }
            | MachineAddFailure::NatsReloadFailed { .. }
            | MachineAddFailure::MintedCredentialUnusable { .. }
            | MachineAddFailure::ControlTaskInterrupted { .. }
            | MachineAddFailure::CredentialEvidenceWriteFailed { .. },
        )
        | (
            MachineAddOperationState::Completed
            | MachineAddOperationState::Failed { .. }
            | MachineAddOperationState::Cancelled { .. },
            _,
        ) => false,
    }
}

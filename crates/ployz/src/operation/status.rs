use std::time::SystemTime;

use crate::error::PrimitiveFailure;

use super::identity::{
    IdempotencyKey, OperationId, OperationKind, OperationOwner, OperationPayloadFingerprint,
    PrincipalId, ScopeId,
};
use super::lease::{OperationLease, OperationLeaseEpoch, OperationLiveness};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationStage(String);

impl OperationStage {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        super::identity::parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationFailureCode(String);

impl OperationFailureCode {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        super::identity::parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationTerminal {
    Succeeded,
    Failed { code: OperationFailureCode },
    Interrupted { code: OperationFailureCode },
}

impl OperationTerminal {
    #[must_use]
    pub fn succeeded() -> Self {
        Self::Succeeded
    }

    #[must_use]
    pub fn failed(code: OperationFailureCode) -> Self {
        Self::Failed { code }
    }

    #[must_use]
    pub fn interrupted(code: OperationFailureCode) -> Self {
        Self::Interrupted { code }
    }

    #[must_use]
    pub fn code(&self) -> Option<&OperationFailureCode> {
        match self {
            Self::Succeeded => None,
            Self::Failed { code } | Self::Interrupted { code } => Some(code),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationLifecycleStatus {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSubmission {
    operation: OperationId,
    kind: OperationKind,
    scope: ScopeId,
    idempotency: IdempotencyKey,
    principal: PrincipalId,
    payload_fingerprint: OperationPayloadFingerprint,
}

impl OperationSubmission {
    #[must_use]
    pub fn new(
        operation: OperationId,
        kind: OperationKind,
        scope: ScopeId,
        idempotency: IdempotencyKey,
        principal: PrincipalId,
        payload_fingerprint: OperationPayloadFingerprint,
    ) -> Self {
        Self {
            operation,
            kind,
            scope,
            idempotency,
            principal,
            payload_fingerprint,
        }
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub fn kind(&self) -> &OperationKind {
        &self.kind
    }

    #[must_use]
    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }

    #[must_use]
    pub fn idempotency(&self) -> &IdempotencyKey {
        &self.idempotency
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    #[must_use]
    pub fn payload_fingerprint(&self) -> &OperationPayloadFingerprint {
        &self.payload_fingerprint
    }

    pub fn ensure_replay_matches(&self, other: &Self) -> Result<(), PrimitiveFailure> {
        if self.kind == other.kind
            && self.scope == other.scope
            && self.idempotency == other.idempotency
            && self.payload_fingerprint == other.payload_fingerprint
        {
            return Ok(());
        }
        Err(PrimitiveFailure::OperationStateConflict)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    submission: OperationSubmission,
    lifecycle: OperationLifecycle,
    created_at: SystemTime,
    updated_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OperationLifecycle {
    Running {
        stage: OperationStage,
        lease: OperationLease,
    },
    Terminal {
        terminal: OperationTerminal,
        last_stage: Option<OperationStage>,
    },
}

impl OperationRecord {
    #[must_use]
    pub fn running(
        submission: OperationSubmission,
        lease: OperationLease,
        current_stage: OperationStage,
        created_at: SystemTime,
    ) -> Self {
        Self {
            submission,
            lifecycle: OperationLifecycle::Running {
                stage: current_stage,
                lease,
            },
            created_at,
            updated_at: created_at,
        }
    }

    pub fn from_stored(
        submission: OperationSubmission,
        status: OperationLifecycleStatus,
        current_stage: Option<OperationStage>,
        lease: Option<OperationLease>,
        terminal: Option<OperationTerminal>,
        created_at: SystemTime,
        updated_at: SystemTime,
    ) -> Result<Self, PrimitiveFailure> {
        let lifecycle = match (status, current_stage, lease, terminal) {
            (OperationLifecycleStatus::Running, Some(stage), Some(lease), None) => {
                OperationLifecycle::Running { stage, lease }
            }
            (
                OperationLifecycleStatus::Succeeded,
                last_stage,
                None,
                Some(OperationTerminal::Succeeded),
            ) => OperationLifecycle::Terminal {
                terminal: OperationTerminal::Succeeded,
                last_stage,
            },
            (
                OperationLifecycleStatus::Failed,
                last_stage,
                None,
                Some(OperationTerminal::Failed { code }),
            ) => OperationLifecycle::Terminal {
                terminal: OperationTerminal::Failed { code },
                last_stage,
            },
            (
                OperationLifecycleStatus::Interrupted,
                last_stage,
                None,
                Some(OperationTerminal::Interrupted { code }),
            ) => OperationLifecycle::Terminal {
                terminal: OperationTerminal::Interrupted { code },
                last_stage,
            },
            _ => return Err(PrimitiveFailure::MalformedPayload),
        };
        Ok(Self {
            submission,
            lifecycle,
            created_at,
            updated_at,
        })
    }

    #[must_use]
    pub fn submission(&self) -> &OperationSubmission {
        &self.submission
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        self.submission.operation()
    }

    #[must_use]
    pub fn kind(&self) -> &OperationKind {
        self.submission.kind()
    }

    #[must_use]
    pub fn scope(&self) -> &ScopeId {
        self.submission.scope()
    }

    #[must_use]
    pub fn idempotency(&self) -> &IdempotencyKey {
        self.submission.idempotency()
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        self.submission.principal()
    }

    #[must_use]
    pub fn payload_fingerprint(&self) -> &OperationPayloadFingerprint {
        self.submission.payload_fingerprint()
    }

    #[must_use]
    pub fn status(&self) -> OperationLifecycleStatus {
        match &self.lifecycle {
            OperationLifecycle::Running { .. } => OperationLifecycleStatus::Running,
            OperationLifecycle::Terminal { terminal, .. } => status_for_terminal(terminal),
        }
    }

    #[must_use]
    pub fn current_stage(&self) -> Option<&OperationStage> {
        match &self.lifecycle {
            OperationLifecycle::Running { stage, .. } => Some(stage),
            OperationLifecycle::Terminal { last_stage, .. } => last_stage.as_ref(),
        }
    }

    #[must_use]
    pub fn lease(&self) -> Option<&OperationLease> {
        match &self.lifecycle {
            OperationLifecycle::Running { lease, .. } => Some(lease),
            OperationLifecycle::Terminal { .. } => None,
        }
    }

    #[must_use]
    pub fn terminal(&self) -> Option<&OperationTerminal> {
        match &self.lifecycle {
            OperationLifecycle::Running { .. } => None,
            OperationLifecycle::Terminal { terminal, .. } => Some(terminal),
        }
    }

    #[must_use]
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    #[must_use]
    pub fn updated_at(&self) -> SystemTime {
        self.updated_at
    }

    #[must_use]
    pub fn liveness_at(&self, now: SystemTime) -> OperationLiveness {
        match &self.lifecycle {
            OperationLifecycle::Running { lease, .. } => lease.liveness_at(now),
            OperationLifecycle::Terminal { .. } => OperationLiveness::NoActiveDriver,
        }
    }

    pub fn ensure_checkpoint_allowed(
        &self,
        owner: &OperationOwner,
        epoch: OperationLeaseEpoch,
        now: SystemTime,
    ) -> Result<(), PrimitiveFailure> {
        match &self.lifecycle {
            OperationLifecycle::Running { lease, .. } => {
                lease.ensure_checkpoint_allowed(owner, epoch, now)
            }
            OperationLifecycle::Terminal { terminal, .. } => Err(error_for_terminal(terminal)),
        }
    }

    pub fn write_terminal(
        &mut self,
        terminal: OperationTerminal,
        owner: &OperationOwner,
        epoch: OperationLeaseEpoch,
        now: SystemTime,
    ) -> Result<(), PrimitiveFailure> {
        let last_stage = match &self.lifecycle {
            OperationLifecycle::Terminal {
                terminal: existing, ..
            } => {
                if existing == &terminal {
                    return Ok(());
                }
                return Err(PrimitiveFailure::OperationStateConflict);
            }
            OperationLifecycle::Running { stage, lease } => {
                lease.ensure_checkpoint_allowed(owner, epoch, now)?;
                Some(stage.clone())
            }
        };
        self.lifecycle = OperationLifecycle::Terminal {
            terminal,
            last_stage,
        };
        self.updated_at = now;
        Ok(())
    }
}

fn status_for_terminal(terminal: &OperationTerminal) -> OperationLifecycleStatus {
    match terminal {
        OperationTerminal::Succeeded => OperationLifecycleStatus::Succeeded,
        OperationTerminal::Failed { .. } => OperationLifecycleStatus::Failed,
        OperationTerminal::Interrupted { .. } => OperationLifecycleStatus::Interrupted,
    }
}

fn error_for_terminal(terminal: &OperationTerminal) -> PrimitiveFailure {
    match terminal {
        OperationTerminal::Succeeded => PrimitiveFailure::OperationAlreadySucceeded,
        OperationTerminal::Failed { .. } => PrimitiveFailure::OperationAlreadyFailed,
        OperationTerminal::Interrupted { .. } => PrimitiveFailure::OperationInterrupted,
    }
}

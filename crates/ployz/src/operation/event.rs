use std::time::SystemTime;

use crate::error::PrimitiveFailure;

use super::identity::OperationId;
use super::status::{OperationStage, OperationTerminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationEventSequence(u64);

impl OperationEventSequence {
    #[must_use]
    pub const fn first() -> Self {
        Self(1)
    }

    pub fn new(value: u64) -> Result<Self, PrimitiveFailure> {
        if value == 0 {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Result<Self, PrimitiveFailure> {
        let Some(next) = self.0.checked_add(1) else {
            return Err(PrimitiveFailure::OperationStateConflict);
        };
        Ok(Self(next))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationEventCursor {
    operation: OperationId,
    sequence: OperationEventSequence,
}

impl OperationEventCursor {
    #[must_use]
    pub fn new(operation: OperationId, sequence: OperationEventSequence) -> Self {
        Self {
            operation,
            sequence,
        }
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub fn sequence(&self) -> OperationEventSequence {
        self.sequence
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationEventKind {
    Stage { stage: OperationStage },
    Terminal { terminal: OperationTerminal },
}

impl OperationEventKind {
    #[must_use]
    pub fn stage(stage: OperationStage) -> Self {
        Self::Stage { stage }
    }

    #[must_use]
    pub fn terminal(terminal: OperationTerminal) -> Self {
        Self::Terminal { terminal }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationEvent {
    operation: OperationId,
    sequence: OperationEventSequence,
    occurred_at: SystemTime,
    kind: OperationEventKind,
}

impl OperationEvent {
    #[must_use]
    pub fn new(operation: OperationId, kind: OperationEventKind, occurred_at: SystemTime) -> Self {
        Self {
            operation,
            sequence: OperationEventSequence::first(),
            occurred_at,
            kind,
        }
    }

    pub fn from_sequence(
        operation: OperationId,
        sequence: OperationEventSequence,
        kind: OperationEventKind,
        occurred_at: SystemTime,
    ) -> Result<Self, PrimitiveFailure> {
        Ok(Self {
            operation,
            sequence,
            occurred_at,
            kind,
        })
    }

    pub fn next(
        &self,
        kind: OperationEventKind,
        occurred_at: SystemTime,
    ) -> Result<Self, PrimitiveFailure> {
        Self::from_sequence(
            self.operation.clone(),
            self.sequence.next()?,
            kind,
            occurred_at,
        )
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub fn sequence(&self) -> OperationEventSequence {
        self.sequence
    }

    #[must_use]
    pub fn occurred_at(&self) -> SystemTime {
        self.occurred_at
    }

    #[must_use]
    pub fn kind(&self) -> &OperationEventKind {
        &self.kind
    }

    #[must_use]
    pub fn cursor(&self) -> OperationEventCursor {
        OperationEventCursor::new(self.operation.clone(), self.sequence)
    }
}

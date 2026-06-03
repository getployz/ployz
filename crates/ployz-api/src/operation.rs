use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{ProtocolError, ProtocolResult};

pub const METHOD_OPERATION_SUBMIT: &str = "operation.submit";
pub const METHOD_OPERATION_GET: &str = "operation.get";
pub const METHOD_OPERATION_STREAM: &str = "operation.stream";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationIdDto(String);

impl OperationIdDto {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn to_domain(&self) -> ProtocolResult<ployz::operation::OperationId> {
        ployz::operation::OperationId::parse(self.0.clone()).map_err(ProtocolError::from)
    }
}

impl From<&ployz::operation::OperationId> for OperationIdDto {
    fn from(value: &ployz::operation::OperationId) -> Self {
        Self(value.as_str().to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationSubmissionDto {
    operation: String,
    kind: String,
    scope: String,
    idempotency: String,
    principal: String,
    payload_fingerprint: String,
}

impl OperationSubmissionDto {
    #[must_use]
    pub fn from_domain(value: &ployz::operation::OperationSubmission) -> Self {
        Self {
            operation: value.operation().as_str().to_owned(),
            kind: value.kind().as_str().to_owned(),
            scope: value.scope().as_str().to_owned(),
            idempotency: value.idempotency().as_str().to_owned(),
            principal: value.principal().as_str().to_owned(),
            payload_fingerprint: value.payload_fingerprint().as_str().to_owned(),
        }
    }

    pub fn to_domain(&self) -> ProtocolResult<ployz::operation::OperationSubmission> {
        Ok(ployz::operation::OperationSubmission::new(
            ployz::operation::OperationId::parse(self.operation.clone())
                .map_err(ProtocolError::from)?,
            ployz::operation::OperationKind::parse(self.kind.clone())
                .map_err(ProtocolError::from)?,
            ployz::operation::ScopeId::parse(self.scope.clone()).map_err(ProtocolError::from)?,
            ployz::operation::IdempotencyKey::parse(self.idempotency.clone())
                .map_err(ProtocolError::from)?,
            ployz::operation::PrincipalId::parse(self.principal.clone())
                .map_err(ProtocolError::from)?,
            ployz::operation::OperationPayloadFingerprint::parse(self.payload_fingerprint.clone())
                .map_err(ProtocolError::from)?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationGetRequestDto {
    operation: OperationIdDto,
}

impl OperationGetRequestDto {
    #[must_use]
    pub fn new(operation: OperationIdDto) -> Self {
        Self { operation }
    }

    #[must_use]
    pub fn operation(&self) -> &OperationIdDto {
        &self.operation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationStreamRequestDto {
    operation: OperationIdDto,
    cursor: Option<OperationEventCursorDto>,
}

impl OperationStreamRequestDto {
    #[must_use]
    pub fn new(operation: OperationIdDto, cursor: Option<OperationEventCursorDto>) -> Self {
        Self { operation, cursor }
    }

    #[must_use]
    pub fn operation(&self) -> &OperationIdDto {
        &self.operation
    }

    pub fn cursor(&self) -> ProtocolResult<Option<ployz::operation::OperationEventCursor>> {
        let cursor = self
            .cursor
            .as_ref()
            .map(OperationEventCursorDto::to_domain)
            .transpose()?;
        if let Some(cursor) = &cursor
            && cursor.operation() != &self.operation.to_domain()?
        {
            return Err(ProtocolError::malformed_payload(
                "stream cursor operation must match request operation",
            ));
        }
        Ok(cursor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationEventCursorDto {
    operation: String,
    sequence: u64,
}

impl OperationEventCursorDto {
    #[must_use]
    pub fn from_domain(value: &ployz::operation::OperationEventCursor) -> Self {
        Self {
            operation: value.operation().as_str().to_owned(),
            sequence: value.sequence().value(),
        }
    }

    pub fn to_domain(&self) -> ProtocolResult<ployz::operation::OperationEventCursor> {
        Ok(ployz::operation::OperationEventCursor::new(
            ployz::operation::OperationId::parse(self.operation.clone())
                .map_err(ProtocolError::from)?,
            ployz::operation::OperationEventSequence::new(self.sequence)
                .map_err(ProtocolError::from)?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationStatusResponseDto {
    record: OperationRecordDto,
    liveness: OperationLivenessDto,
}

impl OperationStatusResponseDto {
    #[must_use]
    pub fn new(record: OperationRecordDto, liveness: OperationLivenessDto) -> Self {
        Self { record, liveness }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRecordDto {
    submission: OperationSubmissionDto,
    lifecycle_status: OperationLifecycleStatusDto,
    current_stage: Option<String>,
    terminal: Option<OperationTerminalDto>,
    lease: Option<OperationLeaseDto>,
    created_at_ms: u64,
    updated_at_ms: u64,
}

impl OperationRecordDto {
    #[must_use]
    pub fn from_domain(value: &ployz::operation::OperationRecord) -> Self {
        Self {
            submission: OperationSubmissionDto::from_domain(value.submission()),
            lifecycle_status: OperationLifecycleStatusDto::from_domain(value.status()),
            current_stage: value.current_stage().map(|stage| stage.as_str().to_owned()),
            terminal: value.terminal().map(OperationTerminalDto::from_domain),
            lease: value.lease().map(OperationLeaseDto::from_domain),
            created_at_ms: system_time_ms(value.created_at()),
            updated_at_ms: system_time_ms(value.updated_at()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationLifecycleStatusDto {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

impl OperationLifecycleStatusDto {
    #[must_use]
    pub fn from_domain(value: ployz::operation::OperationLifecycleStatus) -> Self {
        match value {
            ployz::operation::OperationLifecycleStatus::Running => Self::Running,
            ployz::operation::OperationLifecycleStatus::Succeeded => Self::Succeeded,
            ployz::operation::OperationLifecycleStatus::Failed => Self::Failed,
            ployz::operation::OperationLifecycleStatus::Interrupted => Self::Interrupted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationTerminalDto {
    Succeeded,
    Failed { code: String },
    Interrupted { code: String },
}

impl OperationTerminalDto {
    #[must_use]
    pub fn from_domain(value: &ployz::operation::OperationTerminal) -> Self {
        match value {
            ployz::operation::OperationTerminal::Succeeded => Self::Succeeded,
            ployz::operation::OperationTerminal::Failed { code } => Self::Failed {
                code: code.as_str().to_owned(),
            },
            ployz::operation::OperationTerminal::Interrupted { code } => Self::Interrupted {
                code: code.as_str().to_owned(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationLeaseDto {
    owner: String,
    epoch: u64,
    renewed_at_ms: u64,
    expires_at_ms: u64,
}

impl OperationLeaseDto {
    #[must_use]
    pub fn from_domain(value: &ployz::operation::OperationLease) -> Self {
        Self {
            owner: value.owner().as_str().to_owned(),
            epoch: value.epoch().value(),
            renewed_at_ms: system_time_ms(value.renewed_at()),
            expires_at_ms: system_time_ms(value.expires_at()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationLivenessDto {
    Held {
        owner: String,
        epoch: u64,
        expires_at_ms: u64,
    },
    Expired {
        owner: String,
        epoch: u64,
        expired_at_ms: u64,
    },
    NoActiveDriver,
}

impl OperationLivenessDto {
    #[must_use]
    pub fn from_domain(value: &ployz::operation::OperationLiveness) -> Self {
        match value {
            ployz::operation::OperationLiveness::Held {
                owner,
                epoch,
                expires_at,
            } => Self::Held {
                owner: owner.as_str().to_owned(),
                epoch: epoch.value(),
                expires_at_ms: system_time_ms(*expires_at),
            },
            ployz::operation::OperationLiveness::Expired {
                owner,
                epoch,
                expired_at,
            } => Self::Expired {
                owner: owner.as_str().to_owned(),
                epoch: epoch.value(),
                expired_at_ms: system_time_ms(*expired_at),
            },
            ployz::operation::OperationLiveness::NoActiveDriver => Self::NoActiveDriver,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationEventDto {
    operation: String,
    sequence: u64,
    occurred_at_ms: u64,
    cursor: OperationEventCursorDto,
    kind: OperationEventKindDto,
}

impl OperationEventDto {
    #[must_use]
    pub fn from_domain(value: &ployz::operation::OperationEvent) -> Self {
        Self {
            operation: value.operation().as_str().to_owned(),
            sequence: value.sequence().value(),
            occurred_at_ms: system_time_ms(value.occurred_at()),
            cursor: OperationEventCursorDto::from_domain(&value.cursor()),
            kind: OperationEventKindDto::from_domain(value.kind()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationEventKindDto {
    Stage { stage: String },
    Terminal { terminal: OperationTerminalDto },
}

impl OperationEventKindDto {
    #[must_use]
    pub fn from_domain(value: &ployz::operation::OperationEventKind) -> Self {
        match value {
            ployz::operation::OperationEventKind::Stage { stage } => Self::Stage {
                stage: stage.as_str().to_owned(),
            },
            ployz::operation::OperationEventKind::Terminal { terminal } => Self::Terminal {
                terminal: OperationTerminalDto::from_domain(terminal),
            },
        }
    }
}

fn system_time_ms(value: SystemTime) -> u64 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

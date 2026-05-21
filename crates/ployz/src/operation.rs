//! Product-neutral operation context shared by Ployz domains.

use std::time::SystemTime;

use crate::error::PrimitiveFailure;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalId(String);

impl PrincipalId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(String);

impl ScopeId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorityEpoch(u64);

impl AuthorityEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityContext {
    pub principal: PrincipalId,
    pub scope: ScopeId,
    pub epoch: AuthorityEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityCheck {
    Allowed(AuthorityContext),
    Denied,
    Unknown,
}

pub trait AuthorityPort<E> {
    fn check(&self, principal: &PrincipalId, scope: &ScopeId) -> Result<AuthorityCheck, E>;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationId(String);

impl OperationId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(String);

impl ResourceId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FenceEpoch(u64);

impl FenceEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceToken {
    pub resource: ResourceId,
    pub holder: PrincipalId,
    pub epoch: FenceEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationContext {
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub authority: AuthorityContext,
    pub fence: Option<FenceToken>,
    pub deadline: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationEvidence {
    pub operation: OperationId,
    pub recorded_at: SystemTime,
    pub kind: EvidenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceKind {
    Checkpoint,
    Observation,
    Failure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalMarker {
    Succeeded,
    Failed,
    Interrupted,
}

pub trait OperationPort {
    fn record_evidence(&self, evidence: OperationEvidence) -> Result<(), PrimitiveFailure>;

    fn terminalize(
        &self,
        operation: &OperationId,
        marker: TerminalMarker,
    ) -> Result<(), PrimitiveFailure>;
}

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> Result<T, PrimitiveFailure> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(PrimitiveFailure::MalformedPayload);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_operation_id_is_malformed_payload() {
        assert_eq!(
            OperationId::parse(""),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }
}

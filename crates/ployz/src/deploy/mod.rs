//! Deploy orchestration product ports.

use std::time::SystemTime;

use crate::error::{DeployFailure, PrimitiveFailure};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalId(String);

impl PrincipalId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(String);

impl ScopeId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorityEpoch(u64);

impl AuthorityEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
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

pub trait AuthorityPort {
    fn check(
        &self,
        principal: &PrincipalId,
        scope: &ScopeId,
    ) -> Result<AuthorityCheck, DeployFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationId(String);

impl OperationId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
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
    Failed(DeployFailure),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_authority_is_not_allowed() {
        let check = AuthorityCheck::Unknown;

        assert!(!matches!(check, AuthorityCheck::Allowed(_)));
    }
}

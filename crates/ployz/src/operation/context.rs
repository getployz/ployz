use std::time::SystemTime;

use crate::operation::authority::AuthorityContext;
use crate::operation::claims::SubmittedFenceToken;
use crate::operation::identity::{
    IdempotencyKey, OperationId, PrincipalId, ScopeId, parse_non_empty,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandKind(String);

impl CommandKind {
    pub fn parse(value: impl Into<String>) -> Result<Self, crate::error::PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn into_polis(self) -> polis::Result<polis::CommandKind> {
        polis::CommandKind::parse(self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FingerprintedResource(String);

impl FingerprintedResource {
    pub fn parse(value: impl Into<String>) -> Result<Self, crate::error::PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn into_polis(self) -> polis::Result<polis::FingerprintedResource> {
        polis::FingerprintedResource::parse(self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationContext {
    operation: OperationId,
    idempotency: IdempotencyKey,
    authority: AuthorityContext,
    submitted_fence: Option<SubmittedFenceToken>,
    deadline: SystemTime,
}

impl MutationContext {
    #[must_use]
    pub(crate) fn new(
        operation: OperationId,
        idempotency: IdempotencyKey,
        authority: AuthorityContext,
        submitted_fence: Option<SubmittedFenceToken>,
        deadline: SystemTime,
    ) -> Self {
        Self {
            operation,
            idempotency,
            authority,
            submitted_fence,
            deadline,
        }
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub fn idempotency(&self) -> &IdempotencyKey {
        &self.idempotency
    }

    #[must_use]
    pub fn authority(&self) -> &AuthorityContext {
        &self.authority
    }

    #[must_use]
    pub fn submitted_fence(&self) -> Option<&SubmittedFenceToken> {
        self.submitted_fence.as_ref()
    }

    #[must_use]
    pub fn deadline(&self) -> SystemTime {
        self.deadline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationIntent {
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub principal: PrincipalId,
    pub scope: ScopeId,
    pub command: CommandKind,
    pub payload_hash: Vec<u8>,
    pub resources: Vec<FingerprintedResource>,
    pub submitted_fence: Option<SubmittedFenceToken>,
    pub deadline: SystemTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PrimitiveFailure;

    #[test]
    fn empty_command_kind_is_malformed_payload() {
        assert_eq!(
            CommandKind::parse(" "),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }

    #[test]
    fn empty_fingerprinted_resource_is_malformed_payload() {
        assert_eq!(
            FingerprintedResource::parse(" "),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }
}

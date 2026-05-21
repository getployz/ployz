use std::time::SystemTime;

use polis::{CommandKind, FingerprintedResource};

use crate::operation::authority::AuthorityContext;
use crate::operation::claims::SubmittedFenceToken;
use crate::operation::identity::{IdempotencyKey, OperationId, PrincipalId, ScopeId};

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

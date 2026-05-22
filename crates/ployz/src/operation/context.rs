use std::time::SystemTime;

use crate::operation::authority::AuthorityContext;
use crate::operation::claims::SubmittedFenceToken;
use crate::operation::identity::{IdempotencyKey, OperationId};
#[cfg(feature = "test-support")]
use crate::operation::identity::{PrincipalId, ScopeId};

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
    #[cfg(any(test, feature = "test-support"))]
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

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_authorized(
        operation: OperationId,
        idempotency: IdempotencyKey,
        principal: PrincipalId,
        scope: ScopeId,
        authority_epoch: crate::operation::authority::AuthorityEpoch,
        submitted_fence: Option<SubmittedFenceToken>,
        deadline: SystemTime,
    ) -> Self {
        Self::new(
            operation,
            idempotency,
            AuthorityContext::new(principal, scope, authority_epoch),
            submitted_fence,
            deadline,
        )
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

use std::time::SystemTime;

use crate::error::PrimitiveFailure;
use crate::operation::authority::{AuthorityContext, AuthorityDecision, AuthorityPort};
use crate::operation::claims::SubmittedFenceToken;
use crate::operation::identity::{IdempotencyKey, OperationId, PrincipalId, ScopeId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationContextRequest {
    operation: OperationId,
    idempotency: IdempotencyKey,
    principal: PrincipalId,
    scope: ScopeId,
    submitted_fence: Option<SubmittedFenceToken>,
    deadline: SystemTime,
}

impl MutationContextRequest {
    #[must_use]
    pub fn new(
        operation: OperationId,
        idempotency: IdempotencyKey,
        principal: PrincipalId,
        scope: ScopeId,
        deadline: SystemTime,
    ) -> Self {
        Self {
            operation,
            idempotency,
            principal,
            scope,
            submitted_fence: None,
            deadline,
        }
    }

    #[must_use]
    pub fn with_submitted_fence(mut self, submitted_fence: SubmittedFenceToken) -> Self {
        self.submitted_fence = Some(submitted_fence);
        self
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

    #[cfg(any(test, feature = "test-support"))]
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct MutationWriteIdentity {
    scope: ScopeId,
    operation: OperationId,
    idempotency: IdempotencyKey,
}

impl MutationWriteIdentity {
    #[must_use]
    pub(crate) fn new(scope: ScopeId, operation: OperationId, idempotency: IdempotencyKey) -> Self {
        Self {
            scope,
            operation,
            idempotency,
        }
    }

    #[must_use]
    pub(crate) fn from_context(context: &MutationContext) -> Self {
        Self {
            scope: context.authority().scope().clone(),
            operation: context.operation().clone(),
            idempotency: context.idempotency().clone(),
        }
    }

    #[must_use]
    pub(crate) fn scope(&self) -> &ScopeId {
        &self.scope
    }

    #[must_use]
    pub(crate) fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub(crate) fn idempotency(&self) -> &IdempotencyKey {
        &self.idempotency
    }
}

pub struct MutationAuthorizer<A> {
    authority: A,
}

impl<A> MutationAuthorizer<A> {
    #[must_use]
    pub fn new(authority: A) -> Self {
        Self { authority }
    }
}

impl<A> MutationAuthorizer<A>
where
    A: AuthorityPort,
{
    pub fn authorize(
        &self,
        request: MutationContextRequest,
    ) -> Result<MutationContext, PrimitiveFailure> {
        let decision = self.authority.decide(&request.principal, &request.scope)?;
        let epoch = match decision {
            AuthorityDecision::Allowed(epoch) => epoch,
            AuthorityDecision::Denied => return Err(PrimitiveFailure::Unauthorized),
            AuthorityDecision::Unknown => return Err(PrimitiveFailure::FreshnessUnknown),
        };
        Ok(MutationContext::new(
            request.operation,
            request.idempotency,
            AuthorityContext::new(request.principal, request.scope, epoch),
            request.submitted_fence,
            request.deadline,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::AuthorityEpoch;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn mutation_authorizer_builds_authorized_context() {
        let request = request("node-a", "cluster");

        let context = MutationAuthorizer::new(AllowAuthority)
            .authorize(request)
            .expect("authorized context");

        assert_eq!(context.authority().principal().as_str(), "node-a");
        assert_eq!(context.authority().scope().as_str(), "cluster");
        assert_eq!(context.authority().epoch(), AuthorityEpoch::new(7));
    }

    #[test]
    fn mutation_authorizer_preserves_submitted_fence() {
        let fence = SubmittedFenceToken {
            resource: crate::operation::ResourceId::parse("volume:data").expect("resource"),
            holder: PrincipalId::parse("node-a").expect("holder"),
            epoch: crate::operation::FenceEpoch::new(3).expect("fence epoch"),
            claim_hash: crate::operation::ClaimHash::parse("claim-hash-a").expect("hash"),
        };
        let request = request("node-a", "cluster").with_submitted_fence(fence.clone());

        let context = MutationAuthorizer::new(AllowAuthority)
            .authorize(request)
            .expect("authorized context");

        assert_eq!(context.submitted_fence(), Some(&fence));
    }

    #[test]
    fn mutation_authorizer_separates_denied_from_unknown_authority() {
        assert_eq!(
            MutationAuthorizer::new(DenyAuthority).authorize(request("node-a", "cluster")),
            Err(PrimitiveFailure::Unauthorized)
        );
        assert_eq!(
            MutationAuthorizer::new(UnknownAuthority).authorize(request("node-a", "cluster")),
            Err(PrimitiveFailure::FreshnessUnknown)
        );
    }

    fn request(principal: &str, scope: &str) -> MutationContextRequest {
        MutationContextRequest::new(
            OperationId::parse("operation-1").expect("operation"),
            IdempotencyKey::parse("idem-1").expect("idempotency"),
            PrincipalId::parse(principal).expect("principal"),
            ScopeId::parse(scope).expect("scope"),
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    struct AllowAuthority;

    impl AuthorityPort for AllowAuthority {
        fn decide(
            &self,
            _principal: &PrincipalId,
            _scope: &ScopeId,
        ) -> Result<AuthorityDecision, PrimitiveFailure> {
            Ok(AuthorityDecision::Allowed(AuthorityEpoch::new(7)))
        }
    }

    struct DenyAuthority;

    impl AuthorityPort for DenyAuthority {
        fn decide(
            &self,
            _principal: &PrincipalId,
            _scope: &ScopeId,
        ) -> Result<AuthorityDecision, PrimitiveFailure> {
            Ok(AuthorityDecision::Denied)
        }
    }

    struct UnknownAuthority;

    impl AuthorityPort for UnknownAuthority {
        fn decide(
            &self,
            _principal: &PrincipalId,
            _scope: &ScopeId,
        ) -> Result<AuthorityDecision, PrimitiveFailure> {
            Ok(AuthorityDecision::Unknown)
        }
    }
}

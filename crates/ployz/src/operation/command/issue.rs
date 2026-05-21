use std::marker::PhantomData;

use polis::AttemptRequest;

use crate::error::PrimitiveFailure;
use crate::operation::authority::{AuthorityContext, AuthorityPort};
use crate::operation::context::{AttemptIssue, AttemptSpec, MutationContext};
use crate::operation::polis_boundary::map_polis_to_primitive;

pub struct AttemptIssuer<A> {
    authority: A,
}

impl<A> AttemptIssuer<A> {
    #[must_use]
    pub fn new(authority: A) -> Self {
        Self { authority }
    }
}

impl<A> AttemptIssuer<A>
where
    A: AuthorityPort,
{
    pub(crate) fn issue<C>(
        &self,
        issue: AttemptIssue,
        spec: AttemptSpec,
    ) -> Result<IssuedAttempt<C>, PrimitiveFailure> {
        let epoch = match self
            .authority
            .decide(&issue.principal, &issue.scope)?
            .epoch()
        {
            Some(epoch) => epoch,
            None => return Err(PrimitiveFailure::Unauthorized),
        };
        let operation =
            polis::OperationId::parse(issue.operation.as_str()).map_err(map_polis_to_primitive)?;
        let idempotency = polis::IdempotencyKey::parse(issue.idempotency.as_str())
            .map_err(map_polis_to_primitive)?;
        let actor =
            polis::PrincipalId::parse(issue.principal.as_str()).map_err(map_polis_to_primitive)?;
        let scope = polis::ScopeId::parse(issue.scope.as_str()).map_err(map_polis_to_primitive)?;
        let (fingerprint_builder, submitted_fence) = spec
            .into_fingerprint_builder(actor, scope, polis::GrantEpoch::new(epoch.value()))
            .map_err(map_polis_to_primitive)?;
        let fingerprint = fingerprint_builder
            .finish()
            .map_err(map_polis_to_primitive)?;
        let operation_request =
            AttemptRequest::new(operation, idempotency, fingerprint, issue.deadline);
        let context = MutationContext::new(
            issue.operation,
            issue.idempotency,
            AuthorityContext::new(issue.principal, issue.scope, epoch),
            submitted_fence,
            issue.deadline,
        );
        Ok(IssuedAttempt::new(context, operation_request))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedAttempt<C = ()> {
    pub(super) context: MutationContext,
    pub(super) operation_request: AttemptRequest,
    _command: PhantomData<fn() -> C>,
}

impl<C> IssuedAttempt<C> {
    #[must_use]
    pub(crate) fn new(
        context: MutationContext,
        operation_request: impl Into<AttemptRequest>,
    ) -> Self {
        Self {
            context,
            operation_request: operation_request.into(),
            _command: PhantomData,
        }
    }

    #[must_use]
    pub fn context(&self) -> &MutationContext {
        &self.context
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::operation::{
        AuthorityDecision, AuthorityEpoch, ClaimHash, FenceEpoch, IdempotencyKey, OperationId,
        PrincipalId, ResourceId, ScopeId, SubmittedFenceToken,
    };

    enum TestCommand {}

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

    fn fence(epoch: u64, claim_hash: &str) -> SubmittedFenceToken {
        SubmittedFenceToken {
            resource: ResourceId::parse("resource:test").expect("resource"),
            holder: PrincipalId::parse("node-a").expect("holder"),
            epoch: FenceEpoch::new(epoch).expect("epoch"),
            claim_hash: ClaimHash::parse(claim_hash).expect("claim hash"),
        }
    }

    fn issue_for_fence(submitted_fence: Option<SubmittedFenceToken>) -> IssuedAttempt<TestCommand> {
        let mut spec = AttemptSpec::new("test", "test.v1")
            .field("payload", "value")
            .field_u64("generation", 1)
            .resource("resource:test");
        if let Some(fence) = submitted_fence {
            spec = spec.submitted_fence(fence);
        }
        AttemptIssuer::new(AllowAuthority)
            .issue::<TestCommand>(
                AttemptIssue {
                    operation: OperationId::parse("op-1").expect("operation"),
                    idempotency: IdempotencyKey::parse("idem-1").expect("idempotency"),
                    principal: PrincipalId::parse("node-a").expect("principal"),
                    scope: ScopeId::parse("cluster").expect("scope"),
                    deadline: SystemTime::UNIX_EPOCH,
                },
                spec,
            )
            .expect("envelope")
    }

    #[test]
    fn command_issuer_builds_command_envelope_from_allowed_authority() {
        let envelope = issue_for_fence(None);

        assert_eq!(
            envelope.context().authority().epoch(),
            AuthorityEpoch::new(7)
        );
    }

    #[test]
    fn command_issuer_includes_submitted_fence_in_fingerprint() {
        let envelope = issue_for_fence(Some(fence(3, "claim-hash-a")));

        let fingerprint = envelope
            .operation_request
            .fingerprint()
            .submitted_fence()
            .expect("submitted fence");

        assert_eq!(fingerprint.resource(), "resource:test");
        assert_eq!(fingerprint.holder(), "node-a");
        assert_eq!(fingerprint.epoch(), 3);
        assert_eq!(fingerprint.claim_hash(), b"claim-hash-a");
    }

    #[test]
    fn command_issuer_fingerprint_distinguishes_missing_and_submitted_fence() {
        let without_fence = issue_for_fence(None);
        let with_fence = issue_for_fence(Some(fence(3, "claim-hash-a")));

        assert_ne!(
            without_fence.operation_request.fingerprint(),
            with_fence.operation_request.fingerprint()
        );
    }

    #[test]
    fn command_issuer_fingerprint_distinguishes_submitted_fences() {
        let first = issue_for_fence(Some(fence(3, "claim-hash-a")));
        let second = issue_for_fence(Some(fence(4, "claim-hash-b")));

        assert_ne!(
            first.operation_request.fingerprint(),
            second.operation_request.fingerprint()
        );
    }
}

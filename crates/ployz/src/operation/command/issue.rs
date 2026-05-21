use std::marker::PhantomData;

use polis::OperationRequest;

use crate::error::PrimitiveFailure;
use crate::operation::authority::{AuthorityContext, AuthorityPort};
use crate::operation::context::{MutationContext, MutationIntent};
use crate::operation::polis_boundary::map_polis_to_primitive;

pub struct CommandIssuer<A> {
    authority: A,
}

impl<A> CommandIssuer<A> {
    #[must_use]
    pub fn new(authority: A) -> Self {
        Self { authority }
    }
}

impl<A> CommandIssuer<A>
where
    A: AuthorityPort,
{
    pub fn issue<C>(&self, intent: MutationIntent) -> Result<CommandEnvelope<C>, PrimitiveFailure> {
        let epoch = match self
            .authority
            .decide(&intent.principal, &intent.scope)?
            .epoch()
        {
            Some(epoch) => epoch,
            None => return Err(PrimitiveFailure::Unauthorized),
        };
        let operation =
            polis::OperationId::parse(intent.operation.as_str()).map_err(map_polis_to_primitive)?;
        let idempotency = polis::IdempotencyKey::parse(intent.idempotency.as_str())
            .map_err(map_polis_to_primitive)?;
        let actor =
            polis::PrincipalId::parse(intent.principal.as_str()).map_err(map_polis_to_primitive)?;
        let scope = polis::ScopeId::parse(intent.scope.as_str()).map_err(map_polis_to_primitive)?;
        let command = intent
            .command
            .into_polis()
            .map_err(map_polis_to_primitive)?;
        let resources = intent
            .resources
            .into_iter()
            .map(|resource| resource.into_polis())
            .collect::<polis::Result<Vec<_>>>()
            .map_err(map_polis_to_primitive)?;
        let submitted_fence_fingerprint = intent
            .submitted_fence
            .as_ref()
            .map(crate::operation::SubmittedFenceToken::fingerprint)
            .transpose()?;
        let fingerprint = polis::RequestFingerprint::new(
            actor,
            scope,
            command,
            intent.payload_hash,
            resources,
            submitted_fence_fingerprint,
            polis::GrantEpoch::new(epoch.value()),
        )
        .map_err(map_polis_to_primitive)?;
        let operation_request =
            OperationRequest::new(operation, idempotency, fingerprint, intent.deadline);
        let context = MutationContext::new(
            intent.operation,
            intent.idempotency,
            AuthorityContext::new(intent.principal, intent.scope, epoch),
            intent.submitted_fence,
            intent.deadline,
        );
        Ok(CommandEnvelope::new(context, operation_request))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEnvelope<C = ()> {
    pub(super) context: MutationContext,
    pub(super) operation_request: OperationRequest,
    _command: PhantomData<fn() -> C>,
}

impl<C> CommandEnvelope<C> {
    #[must_use]
    pub(crate) fn new(context: MutationContext, operation_request: OperationRequest) -> Self {
        Self {
            context,
            operation_request,
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
        AuthorityDecision, AuthorityEpoch, ClaimHash, CommandKind, FenceEpoch,
        FingerprintedResource, IdempotencyKey, OperationId, PrincipalId, ResourceId, ScopeId,
        SubmittedFenceToken,
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

    fn issue_for_fence(
        submitted_fence: Option<SubmittedFenceToken>,
    ) -> CommandEnvelope<TestCommand> {
        CommandIssuer::new(AllowAuthority)
            .issue::<TestCommand>(MutationIntent {
                operation: OperationId::parse("op-1").expect("operation"),
                idempotency: IdempotencyKey::parse("idem-1").expect("idempotency"),
                principal: PrincipalId::parse("node-a").expect("principal"),
                scope: ScopeId::parse("cluster").expect("scope"),
                command: CommandKind::parse("test").expect("command"),
                payload_hash: vec![1],
                resources: vec![FingerprintedResource::parse("resource:test").expect("resource")],
                submitted_fence,
                deadline: SystemTime::UNIX_EPOCH,
            })
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

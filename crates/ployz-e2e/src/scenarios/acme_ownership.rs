use std::time::{Duration, UNIX_EPOCH};

use ployz::acme::{
    ChallengeOwnership, ChallengeOwnershipPort, ChallengeSlot, Hostname, HttpsBinding,
};
use ployz::deploy::{
    AuthorityContext, AuthorityEpoch, FenceEpoch, FenceToken, IdempotencyKey, MutationContext,
    OperationId, PrincipalId, ResourceId, ScopeId,
};
use ployz::error::CertificateFailure;

struct ClaimingAcme;

impl ChallengeOwnershipPort for ClaimingAcme {
    fn claim_challenge(
        &self,
        context: &MutationContext,
        _binding: &HttpsBinding,
    ) -> Result<ChallengeOwnership, CertificateFailure> {
        let Some(_fence) = &context.fence else {
            return Ok(ChallengeOwnership::Rejected(
                CertificateFailure::UnauthorizedBinding,
            ));
        };
        Ok(ChallengeOwnership::Owned {
            slot: ChallengeSlot::parse("http-01:app.example.com")?,
        })
    }
}

fn context(with_fence: bool) -> MutationContext {
    let fence = with_fence.then(|| FenceToken {
        resource: ResourceId::parse("cert:app.example.com").expect("resource"),
        holder: PrincipalId::parse("node-a").expect("holder"),
        epoch: FenceEpoch::new(3),
    });
    MutationContext {
        operation: OperationId::parse("acme-1").expect("operation"),
        idempotency: IdempotencyKey::parse("idem-acme-1").expect("idempotency"),
        authority: AuthorityContext {
            principal: PrincipalId::parse("node-a").expect("principal"),
            scope: ScopeId::parse("cluster").expect("scope"),
            epoch: AuthorityEpoch::new(7),
        },
        fence,
        deadline: UNIX_EPOCH + Duration::from_secs(60),
    }
}

#[test]
fn acme_ownership_uses_product_claim_context_without_polis_imports() {
    let binding = HttpsBinding::new(Hostname::parse("app.example.com").expect("hostname"));

    assert!(matches!(
        ClaimingAcme
            .claim_challenge(&context(true), &binding)
            .expect("claim"),
        ChallengeOwnership::Owned { .. }
    ));
    assert!(matches!(
        ClaimingAcme
            .claim_challenge(&context(false), &binding)
            .expect("claim rejection"),
        ChallengeOwnership::Rejected(CertificateFailure::UnauthorizedBinding)
    ));
}

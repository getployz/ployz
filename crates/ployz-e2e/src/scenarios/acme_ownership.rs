use std::time::{Duration, UNIX_EPOCH};

use ployz::acme::{
    ChallengeOwnership, ChallengeOwnershipPort, ChallengeSlot, Hostname, HttpsBinding,
};
use ployz::error::CertificateFailure;
use ployz::operation::{
    AuthorityEpoch, ClaimHash, CommandIssue, FenceEpoch, IdempotencyKey, MutationContext,
    OperationId, PrincipalId, ResourceId, ScopeId, SubmittedFenceToken,
};

struct ClaimingAcme;

impl ChallengeOwnershipPort for ClaimingAcme {
    fn claim_challenge(
        &self,
        context: &MutationContext,
        _binding: &HttpsBinding,
    ) -> Result<ChallengeOwnership, CertificateFailure> {
        let Some(fence) = context.submitted_fence() else {
            return Ok(ChallengeOwnership::Rejected(
                CertificateFailure::UnauthorizedBinding,
            ));
        };
        if fence.resource.as_str() != "cert:app.example.com"
            || fence.holder.as_str() != "node-a"
            || fence.epoch != FenceEpoch::new(3).expect("fence epoch")
            || fence.claim_hash.as_str() != "claim-hash-a"
        {
            return Ok(ChallengeOwnership::Rejected(
                CertificateFailure::UnauthorizedBinding,
            ));
        }
        Ok(ChallengeOwnership::Owned {
            slot: ChallengeSlot::parse("http-01:app.example.com")?,
        })
    }
}

fn context(fence: FenceInput) -> MutationContext {
    let submitted_fence = match fence {
        FenceInput::Missing => None,
        FenceInput::Present {
            resource,
            holder,
            epoch,
            claim_hash,
        } => Some(SubmittedFenceToken {
            resource: ResourceId::parse(resource).expect("resource"),
            holder: PrincipalId::parse(holder).expect("holder"),
            epoch: FenceEpoch::new(epoch).expect("fence epoch"),
            claim_hash: ClaimHash::parse(claim_hash).expect("claim hash"),
        }),
    };
    MutationContext::test_new(
        CommandIssue {
            operation: OperationId::parse("acme-1").expect("operation"),
            idempotency: IdempotencyKey::parse("idem-acme-1").expect("idempotency"),
            principal: PrincipalId::parse("node-a").expect("principal"),
            scope: ScopeId::parse("cluster").expect("scope"),
            deadline: UNIX_EPOCH + Duration::from_secs(60),
        },
        AuthorityEpoch::new(7),
        submitted_fence,
    )
}

enum FenceInput {
    Missing,
    Present {
        resource: &'static str,
        holder: &'static str,
        epoch: u64,
        claim_hash: &'static str,
    },
}

#[test]
fn acme_ownership_uses_product_claim_context_without_polis_imports() {
    let binding = HttpsBinding::new(Hostname::parse("app.example.com").expect("hostname"));

    assert!(matches!(
        ClaimingAcme
            .claim_challenge(
                &context(FenceInput::Present {
                    resource: "cert:app.example.com",
                    holder: "node-a",
                    epoch: 3,
                    claim_hash: "claim-hash-a"
                }),
                &binding
            )
            .expect("claim"),
        ChallengeOwnership::Owned { .. }
    ));
    assert!(matches!(
        ClaimingAcme
            .claim_challenge(&context(FenceInput::Missing), &binding)
            .expect("claim rejection"),
        ChallengeOwnership::Rejected(CertificateFailure::UnauthorizedBinding)
    ));
    assert!(matches!(
        ClaimingAcme
            .claim_challenge(
                &context(FenceInput::Present {
                    resource: "cert:app.example.com",
                    holder: "node-a",
                    epoch: 3,
                    claim_hash: "superseded-claim"
                }),
                &binding
            )
            .expect("superseded claim rejection"),
        ChallengeOwnership::Rejected(CertificateFailure::UnauthorizedBinding)
    ));
    assert!(matches!(
        ClaimingAcme
            .claim_challenge(
                &context(FenceInput::Present {
                    resource: "cert:other.example.com",
                    holder: "node-a",
                    epoch: 3,
                    claim_hash: "claim-hash-a"
                }),
                &binding
            )
            .expect("wrong resource rejection"),
        ChallengeOwnership::Rejected(CertificateFailure::UnauthorizedBinding)
    ));
    assert!(matches!(
        ClaimingAcme
            .claim_challenge(
                &context(FenceInput::Present {
                    resource: "cert:app.example.com",
                    holder: "node-b",
                    epoch: 3,
                    claim_hash: "claim-hash-a"
                }),
                &binding
            )
            .expect("wrong holder rejection"),
        ChallengeOwnership::Rejected(CertificateFailure::UnauthorizedBinding)
    ));
    assert!(matches!(
        ClaimingAcme
            .claim_challenge(
                &context(FenceInput::Present {
                    resource: "cert:app.example.com",
                    holder: "node-a",
                    epoch: 2,
                    claim_hash: "claim-hash-a"
                }),
                &binding
            )
            .expect("wrong epoch rejection"),
        ChallengeOwnership::Rejected(CertificateFailure::UnauthorizedBinding)
    ));
}

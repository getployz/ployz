//! A Build Grant under a preview rule: every check a grant Machine runs on its own facts
//! before it opens a preview build's Sealed record.
//!
//! The rule's signature, this Machine's place in it, the review for a reviewed build, the
//! GitHub token and its claims, the audience recomputed from what this Machine holds, the
//! preview the token's branch or the review names, the commit's authority, and the Sealed
//! record all pass first. Then the token is spent. A trusted push is one by an account the
//! rule trusts, to a branch other than the parent's, of that branch's head. A reviewed
//! build is a dispatch on the parent's branch of exactly the commit the review names.

use ployz_core::{
    BuildGrantId, PreviewGrant, SealedRecord,
    authority::{
        AuthorityRefusal, BuildClaim, BuildStage, CommitAuthority, PolicyDigest, PreviewKey,
        Timestamp, preview::verify_review, rule::verify_signature,
    },
};

use super::{
    Error, LocalMachine,
    preview_deploy::PreviewRefusal,
    rule_grant::{AdmittedRule, Github},
};

fn refused(refusal: AuthorityRefusal) -> Error {
    Error::PreviewGrantRefused(PreviewRefusal::Rule(refusal))
}

impl LocalMachine {
    /// Admit a grant for `holder` under `grant`'s preview rule, with `sealed` the record
    /// Cloud relays, at `now`. Every check runs before anything opens, and the token is
    /// spent last.
    ///
    /// # Errors
    /// Returns [`Error::PreviewGrantRefused`] naming what does not match, a token GitHub
    /// did not sign or that names another repository, event, workflow, builder or runner,
    /// or a branch head GitHub did not answer for, and [`Error::ApprovalRecord`] when the
    /// spent token cannot be recorded.
    pub(crate) async fn admit_preview_grant(
        &self,
        grant: &PreviewGrant,
        holder: Option<&BuildGrantId>,
        sealed: Option<&SealedRecord>,
        github: &impl Github,
        now: Timestamp,
    ) -> Result<AdmittedRule, Error> {
        let record = self.record();
        let trusted = record.trusted_keys();
        let (rule, _) = verify_signature(&grant.rule, &trusted, now).map_err(refused)?;
        let held = |field| {
            tracing::warn!(rule = %rule.rule, field, "refused a preview build's grant");
            refused(AuthorityRefusal::Mismatch { field })
        };
        let cluster =
            self.held_sealing(record.phase())
                .and_then(|sealing| match sealing {
                    ployz_core::Sealing::Held { public_key } => Some(public_key),
                    ployz_core::Sealing::NeedsAccess
                    | ployz_core::Sealing::AwaitsRecovery { .. } => None,
                })
                .ok_or_else(|| refused(AuthorityRefusal::NoClusterIdentity))?;
        if rule.cluster != cluster {
            return Err(held("a Cluster"));
        }
        record.machine().ok_or(Error::NotParticipating)?;
        if !rule.machines.contains(&record.id()) {
            return Err(held("a Machine"));
        }
        self.approvals.mark_rule(&rule, refused)?;
        if rule.ended {
            return Err(held("an ended rule"));
        }
        let service = rule
            .service(&grant.service)
            .ok_or_else(|| held("a Service"))?;
        let holder = holder.ok_or_else(|| held("a holder"))?;
        let review = match (grant.authority, &grant.review) {
            (CommitAuthority::Head, None) => None,
            (CommitAuthority::Review(_), Some(review)) => {
                let checked = verify_review(review, &rule, &trusted, now).map_err(|refusal| {
                    tracing::warn!(rule = %rule.rule, %refusal, "refused a preview build's review");
                    refused(refusal)
                })?;
                if checked.digest_for(&grant.service).is_none() {
                    return Err(held("a Service"));
                }
                Some(checked)
            }
            _ => return Err(held("a review")),
        };
        let claims = github
            .claims(&rule, grant.proof.as_str(), now)
            .await
            .map_err(|refusal| Error::PreviewGrantRefused(PreviewRefusal::Proof(refusal)))?;
        let expected = BuildClaim {
            cluster,
            rule: rule.rule,
            generation: rule.generation,
            service: grant.service.clone(),
            policy: PolicyDigest::of(&service.policy).map_err(|_| held("a policy"))?,
            commit: grant.commit,
            authority: grant.authority,
            stage: BuildStage::Grant {
                holder: *holder,
                recipient: grant.recipient,
            },
        };
        let audience = expected.audience().map_err(|_| held("an audience"))?;
        if claims.aud != audience {
            return Err(held("an audience"));
        }
        let key = rule
            .preview_key(&claims, grant.commit, grant.authority, review.as_ref())
            .map_err(|refusal| {
                tracing::warn!(rule = %rule.rule, %refusal, "refused a preview build's grant");
                refused(refusal)
            })?;
        if let (CommitAuthority::Head, PreviewKey::Branch(branch)) = (grant.authority, &key) {
            let head = github
                .head(rule.source.repository_id, branch, grant.reader.as_ref())
                .await
                .map_err(|error| Error::PreviewGrantRefused(PreviewRefusal::Head(error)))?;
            if head != grant.commit {
                return Err(held("a commit"));
            }
        }
        let digest = sealed
            .map(|record| rule.build_secrets(service, record))
            .transpose()
            .map_err(refused)?;
        if digest != service.secrets {
            return Err(held("the Sealed values"));
        }
        self.approvals
            .consume_rule_proof(&rule, refused, &claims.jti, claims.exp, now)?;
        tracing::info!(
            rule = %rule.rule,
            generation = rule.generation,
            service = %grant.service,
            commit = %grant.commit,
            preview = %key,
            reviewed_by = review.as_ref().map(|review| review.signer.to_string()),
            "admitted a preview build's grant"
        );
        Ok(AdmittedRule {
            recipient: grant.recipient,
            audience,
            rule: rule.rule,
            generation: rule.generation,
            service: grant.service.clone(),
            commit: grant.commit,
        })
    }
}

#[cfg(test)]
#[path = "preview_grant_tests.rs"]
pub(crate) mod tests;

//! Deploys under an automatic deploy rule: an Operator's new Container admitted on a
//! rule and evidence of its build, in place of a person's signed version.
//!
//! The rule's signature, a live generation and a builder Ployz still trusts pass first.
//! Then the build's evidence: online, the pinned builder's token, whose audience is the
//! claim Cloud relays, and the branch head read from GitHub now; or a verified build, a
//! MAC a Machine of this Cluster issued once that online check passed. The Container must
//! also run the image the claim names, built with secrets sealed from this Machine's own
//! attest key, with every other setting the rule approves. Nothing here trusts Cloud's
//! word for a fact.
//!
//!   signature and Cluster ─▶ mark ─▶ floor ─▶ evidence (token and head, or verified
//!     build) ─▶ verify_creation ─▶ mark again and issue (online) ─▶ hook once

use std::time::Instant;

use hkdf::hmac::Mac as _;
use ployz_core::{
    ContainerKind, ContainerWant, QualifiedService, RunTarget, Sealing, ServiceName,
    authority::{
        AuthorityRefusal, AutoDeployRule, Budget, BuildClaim, RuleEvidence, RuleFacts, RuleProof,
        Signed, Timestamp, VerifiedBuild, deploy::Creation, refusal::field, rule::verify_signature,
    },
};

use super::{AdmittedBy, Error, LocalMachine, rule_grant::Github};
use crate::sealing_key::SealingKey;

/// Why a Machine refused a deploy under a rule.
#[derive(Debug, thiserror::Error)]
pub enum RuleDeployRefusal {
    #[error("{}", refusal_text(.0))]
    Rule(AuthorityRefusal),
    #[error("{0}")]
    Proof(crate::github::oidc::ProofRefusal),
    #[error("this server could not read the approved branch's head from GitHub")]
    Head(#[source] crate::github::head::HeadError),
}

impl RuleDeployRefusal {
    /// What a person runs next, when approving the rule again is what fixes it: a stale,
    /// swapped or replayed proof is Cloud's to fix, not theirs.
    pub(crate) fn next(&self) -> Option<&'static str> {
        match self {
            Self::Rule(
                AuthorityRefusal::Mismatch {
                    field: "a payload" | "a Service" | "a Machine" | "an Environment" | "a Cluster",
                }
                | AuthorityRefusal::UnknownKey { .. }
                | AuthorityRefusal::UnknownPasskey
                | AuthorityRefusal::Forgotten { .. },
            ) => Some("ployz github rule approve from an approved device"),
            Self::Rule(_) | Self::Proof(_) | Self::Head(_) => None,
        }
    }
}

/// A rule's build that passed every online check here: the token, its audience, the
/// commit, the head, the attest key, the image and the template. Only the online branch
/// of [`LocalMachine::admit_rule_deploy`] makes one, so [`issue`] signs nothing else.
struct CheckedOnline<'a> {
    rule: &'a Signed<AutoDeployRule>,
    claim: &'a BuildClaim,
}

/// The verified build of a build that passed its online check here, or `None` for a claim
/// with no canonical form, which no check passes.
fn issue(checked: &CheckedOnline<'_>, key: &SealingKey) -> Option<VerifiedBuild> {
    let input = VerifiedBuild::input(checked.rule, checked.claim).ok()?;
    let mut mac = key.verified_build();
    mac.update(&input);
    Some(VerifiedBuild::from_bytes(
        mac.finalize().into_bytes().into(),
    ))
}

/// Whether `verified` is the verified build of `claim` under `rule`, compared in constant
/// time. It answers only yes or no, never the MAC it computed.
fn holds(
    rule: &Signed<AutoDeployRule>,
    claim: &BuildClaim,
    verified: &VerifiedBuild,
    key: &SealingKey,
) -> bool {
    let Ok(input) = VerifiedBuild::input(rule, claim) else {
        return false;
    };
    let mut mac = key.verified_build();
    mac.update(&input);
    mac.verify_slice(verified.as_bytes()).is_ok()
}

/// What a rule admitted, and the verified build an online check issued for it.
#[derive(Debug)]
pub(crate) struct RuleAdmitted {
    pub(crate) by: AdmittedBy,
    pub(crate) verified: Option<VerifiedBuild>,
}

impl LocalMachine {
    /// Admit an Operator's new Container on `proof`, at `now`, with the rule's signature
    /// charged to `budget`, and name the rule, its generation and its signer. A build
    /// checked online gets its verified build; a hook takes its place once per push.
    ///
    /// # Errors
    /// Returns [`Error::RuleDeployRefused`] naming what does not hold, and
    /// [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(crate) async fn admit_rule_deploy(
        &self,
        want: &ContainerWant,
        proof: &RuleProof,
        github: &impl Github,
        now: Timestamp,
        budget: &mut Budget,
    ) -> Result<RuleAdmitted, Error> {
        let ContainerWant::Running {
            container: RunTarget::New(new),
            ..
        } = want
        else {
            unreachable!("only a new creation is admitted under a rule");
        };
        let started = Instant::now();
        let refused = |refusal| Error::RuleDeployRefused(RuleDeployRefusal::Rule(refusal));
        let record = self.record();
        let trusted = record.trusted_keys();
        budget.reserve(&proof.rule, &trusted).map_err(refused)?;
        let key = self.held_sealing_key(record.phase());
        let (cluster, key) = match (self.held_sealing(record.phase()), key) {
            (Some(Sealing::Held { public_key }), Some(key)) => (public_key, key),
            _ => return Err(refused(AuthorityRefusal::NoClusterIdentity)),
        };
        let (rule, signer) =
            verify_signature(&proof.rule, &trusted, Some(cluster)).map_err(refused)?;
        let held = |field| {
            tracing::warn!(rule = %rule.rule, field, "refused a deploy under a rule");
            refused(AuthorityRefusal::Mismatch { field })
        };
        let attest = key.attest().attest();
        record.machine().ok_or(Error::NotParticipating)?;
        let machine = record.id();
        let superseded = |refusal| {
            tracing::warn!(
                rule = %rule.rule,
                generation = rule.generation,
                "refused a deploy under a rule: {refusal}"
            );
            refused(refusal)
        };
        // An ended generation still supersedes every one below it.
        self.approvals.mark_rule(&rule, superseded)?;
        let service = QualifiedService::new(new.slot.namespace.clone(), new.slot.service.clone());
        self.rule_floor(cluster, &service, &rule, now)
            .map_err(|refusal| logged(&rule, &new.slot.service, refusal))?;
        let claim = &proof.claim;
        let head = match &proof.evidence {
            RuleEvidence::Online(token) => {
                // GitHub's token signature is one more check on the same budget.
                budget.reserve_one().map_err(refused)?;
                let claims = github
                    .claims(&rule, token.as_str(), now)
                    .await
                    .map_err(|refusal| {
                        tracing::warn!(rule = %rule.rule, %refusal, "refused a deploy under a rule");
                        Error::RuleDeployRefused(RuleDeployRefusal::Proof(refusal))
                    })?;
                if claim.audience().ok().as_deref() != Some(claims.aud.as_str()) {
                    return Err(held("an audience"));
                }
                // GitHub's `sha` is the commit the push built.
                if claims.sha != claim.commit {
                    return Err(held("a commit"));
                }
                let head = github
                    .head(
                        rule.source.repository_id,
                        &rule.source.branch,
                        proof.reader.as_ref(),
                    )
                    .await
                    .map_err(|error| Error::RuleDeployRefused(RuleDeployRefusal::Head(error)))?;
                Some(head)
            }
            RuleEvidence::Verified(_) => None,
        };
        let facts = RuleFacts {
            cluster,
            attest,
            machine: &machine,
            namespace: &new.slot.namespace,
            service: &new.slot.service,
            head,
        };
        rule.verify_creation(
            claim,
            &facts,
            Creation {
                service: &new.slot.service,
                kind: new.slot.kind,
                spec: &new.resolved_spec,
            },
        )
        .map_err(|refusal| logged(&rule, &new.slot.service, refusal))?;
        let verified = match &proof.evidence {
            RuleEvidence::Verified(verified) => {
                if !holds(&proof.rule, claim, verified, &key) {
                    return Err(held("a verified build"));
                }
                None
            }
            RuleEvidence::Online(_) => {
                // Checked again: a newer generation may have landed while GitHub answered.
                self.approvals.mark_rule(&rule, superseded)?;
                issue(
                    &CheckedOnline {
                        rule: &proof.rule,
                        claim,
                    },
                    &key,
                )
            }
        };
        if new.slot.kind == ContainerKind::PreDeployHook {
            self.approvals
                .consume_rule_hook(rule.rule, claim, &new.slot.creation_key, refused)?;
        }
        tracing::info!(
            kind = %new.slot.kind,
            admission_us = started.elapsed().as_micros(),
            offline = matches!(proof.evidence, RuleEvidence::Verified(_)),
            "deploy of {} admitted under rule {} gen {} commit {}",
            new.slot.service,
            rule.rule,
            rule.generation,
            claim.commit.short(),
        );
        Ok(RuleAdmitted {
            by: AdmittedBy::Rule {
                service,
                signer: signer.into(),
                rule: rule.rule,
                generation: rule.generation,
                issued_at: rule.issued_at,
            },
            verified,
        })
    }

    /// Refuse `rule` for `service` when it was issued before the Service's floor here, or
    /// past this Machine's clock by more than the guard. A rule a forget retired stays
    /// refused until it is approved again.
    ///
    /// # Errors
    /// Returns the refusal, and [`AuthorityRefusal::Mismatch`] naming the record when it
    /// cannot be read.
    pub(super) fn rule_floor(
        &self,
        cluster: ployz_core::SealingPublicKey,
        service: &QualifiedService,
        rule: &AutoDeployRule,
        now: Timestamp,
    ) -> Result<(), AuthorityRefusal> {
        let (floor, now) =
            self.approvals
                .floor(cluster, service, now, |_| AuthorityRefusal::Mismatch {
                    field: "the approval record",
                })?;
        ployz_decide::service_floor::admits(rule.issued_at, floor, now)
            .map_err(super::admission::floor_refusal)
    }

    /// Admit `want` on the rule proof Cloud attached, reading GitHub through the daemon's
    /// one client.
    ///
    /// # Errors
    /// As [`Self::admit_rule_deploy`]; without a usable GitHub client, every rule deploy
    /// is refused.
    pub(crate) async fn admit_rule_deploy_online(
        &self,
        want: &ContainerWant,
        proof: &RuleProof,
        budget: &mut Budget,
    ) -> Result<RuleAdmitted, Error> {
        let github = super::rule_grant::GithubOnline::shared().ok_or(Error::RuleDeployRefused(
            RuleDeployRefusal::Rule(AuthorityRefusal::Mismatch {
                field: "a GitHub client",
            }),
        ))?;
        self.admit_rule_deploy(want, proof, github, Timestamp::now(), budget)
            .await
    }
}

fn logged(rule: &AutoDeployRule, service: &ServiceName, refusal: AuthorityRefusal) -> Error {
    tracing::warn!(
        rule = %rule.rule,
        generation = rule.generation,
        "deploy of {service} refused under the rule: {refusal}"
    );
    Error::RuleDeployRefused(RuleDeployRefusal::Rule(refusal))
}

/// What a refused rule deploy says to the person who reads the Deployment.
fn refusal_text(refusal: &AuthorityRefusal) -> String {
    let text = match refusal {
        AuthorityRefusal::Mismatch { field: "a payload" } => {
            "This service's settings differ from what the automatic deploy rule approves. Approve the rule again."
        }
        AuthorityRefusal::Mismatch { field: "an image" } => {
            "This server runs only the image GitHub built for this commit under the rule."
        }
        AuthorityRefusal::Mismatch {
            field: "an audience",
        } => "This push was built under another approval of the automatic deploy rule. Push again.",
        AuthorityRefusal::Mismatch { field: "a commit" } => {
            "This commit is no longer the head of its branch."
        }
        AuthorityRefusal::Mismatch {
            field: "a verified build",
        } => "No server of this cluster checked this build with GitHub. Push again.",
        AuthorityRefusal::RetiredBuilder { .. } => {
            "This automatic deploy rule pins a builder Ployz no longer trusts. Approve the rule again to pin the current builder."
        }
        AuthorityRefusal::Mismatch {
            field: "a generation",
        } => "A newer approval of the automatic deploy rule replaced this one.",
        AuthorityRefusal::Mismatch {
            field: "the attest key",
        } => "This image wasn't built with this cluster's build secrets.",
        AuthorityRefusal::Mismatch {
            field: "an ended rule",
        } => "Automatic deploys for this environment are off.",
        AuthorityRefusal::Mismatch { field: "a Service" } => {
            "The automatic deploy rule doesn't cover this service."
        }
        AuthorityRefusal::Mismatch { field: "a Machine" } => {
            "The automatic deploy rule was approved before this server joined. Approve it again."
        }
        AuthorityRefusal::Mismatch {
            field: "an Environment",
        } => "The automatic deploy rule is for another environment.",
        AuthorityRefusal::Mismatch { field: "a Cluster" } => {
            "The automatic deploy rule is for another cluster."
        }
        AuthorityRefusal::Mismatch {
            field: "a hook run",
        } => "This push already ran its pre-deploy command here.",
        AuthorityRefusal::Mismatch {
            field: "the signer" | "a passkey",
        } => "An automatic deploy rule needs your passkey or an approved device. Approve it again.",
        AuthorityRefusal::UnknownKey { .. } | AuthorityRefusal::UnknownPasskey => {
            "This server doesn't trust the device that approved the automatic deploy rule."
        }
        AuthorityRefusal::Forgotten { .. } => {
            "This service's older versions were forgotten after the automatic deploy rule was approved. Approve the rule again. Until then it doesn't deploy this service, and Cloud can't move or drain what it built."
        }
        AuthorityRefusal::Mismatch {
            field: field::AHEAD,
        } => {
            "The automatic deploy rule was approved more than 10 minutes ahead of this server's clock. Check the clock of the device that approved it, and this server's."
        }
        AuthorityRefusal::Malformed { .. }
        | AuthorityRefusal::BadSignature { .. }
        | AuthorityRefusal::ApproveOnly { .. }
        | AuthorityRefusal::Mismatch { .. }
        | AuthorityRefusal::Unsigned { .. }
        | AuthorityRefusal::NeedsSignature { .. }
        | AuthorityRefusal::NoClusterIdentity
        | AuthorityRefusal::Expired
        | AuthorityRefusal::ProofReplayed
        | AuthorityRefusal::Overspent => {
            return format!("This server refused the automatic deploy rule: {refusal}.");
        }
    };
    text.to_owned()
}

#[cfg(test)]
#[path = "rule_deploy_tests.rs"]
mod tests;

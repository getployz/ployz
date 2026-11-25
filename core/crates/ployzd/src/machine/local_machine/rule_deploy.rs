//! Deploys under an automatic deploy rule: an Operator's new Container admitted on a
//! rule and evidence of its build, in place of a person's signed version.
//!
//! The rule's signature, a live generation and a builder Ployz still trusts pass first.
//! Then the build's evidence: online, the pinned builder's token, whose audience is the
//! claim Cloud relays, and the branch head read from GitHub now; or a verified build, a
//! MAC a Machine of this Cluster issued once that online check passed. The Container must
//! also run the image the claim names, built with secrets sealed from this Machine's own
//! attest key, with every other setting the rule approves. Nothing here trusts Cloud's
//! word for a fact. A Service a preview rule pins has no build: its template names its
//! image, so it skips the evidence.
//!
//!   signature and Cluster ─▶ mark ─▶ floor ─▶ evidence (token and head, or verified
//!     build) ─▶ verify_creation ─▶ mark again and issue (online) ─▶ hook once
//!                         └──▶ pinned: verify_pinned

use std::time::Instant;

use hkdf::hmac::Mac as _;
use ployz_core::{
    ContainerKind, ContainerWant, QualifiedService, RunTarget, Sealing, ServiceName,
    authority::{
        AuthorityRefusal, AutoDeployRule, Budget, BuildClaim, CommitAuthority, MismatchField,
        RuleEvidence, RuleFacts, RuleProof, Signed, Timestamp, VerifiedBuild, deploy::Creation,
        rule::verify_signature,
    },
};

use super::{Admit, AdmittedBy, Error, LocalMachine, rule_grant::Github};
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
                    field:
                        MismatchField::Payload
                        | MismatchField::Service
                        | MismatchField::Machine
                        | MismatchField::Environment
                        | MismatchField::Cluster,
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
    /// The place a hook's push takes when its create runs.
    pub(crate) hook: Option<HookPlace>,
}

/// The place a push's hook takes under its rule. It stays with the hook's admission until
/// the create takes it, under the turn that checked the caller, so an admission that never
/// reaches its create, as one whose caller was removed while it waited, leaves the place to
/// a retry.
#[derive(Clone, Debug)]
pub(crate) struct HookPlace {
    rule: ployz_core::authority::RuleId,
    /// The preview a preview rule's hook runs in, each of which runs a push's hook once.
    preview: Option<ployz_core::Namespace>,
    claim: BuildClaim,
    slot: ployz_core::CreationKey,
}

impl HookPlace {
    /// Take the place, refusing it when another slot took it for the same push.
    ///
    /// # Errors
    /// Returns [`Error::RuleDeployRefused`] for a place another slot took, and
    /// [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(crate) fn take(&self, approvals: &super::ConsumptionLog) -> Result<(), Error> {
        let place = (self.rule, self.preview.as_ref());
        approvals.consume_rule_hook(place, &self.claim, &self.slot, |refusal| {
            Error::RuleDeployRefused(RuleDeployRefusal::Rule(refusal))
        })
    }
}

impl LocalMachine {
    /// Admit an Operator's new Container on `proof`, at `now`, with the rule's signature
    /// charged to `budget`, and name the rule, its generation and its signer. A build
    /// checked online gets its verified build; a hook gets the place its create takes,
    /// once per push. A check marks no generation.
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
        admit: Admit,
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
        let held = |field: MismatchField| {
            tracing::warn!(rule = %rule.rule, field = field.text(), "refused a deploy under a rule");
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
        self.approvals.mark_rule(&rule, admit, superseded)?;
        let service = QualifiedService::new(new.slot.namespace.clone(), new.slot.service.clone());
        self.rule_floor(cluster, &service, &rule, now)
            .map_err(|refusal| logged(&rule, &new.slot.service, refusal))?;
        let by = AdmittedBy::Rule {
            service,
            signer: signer.into(),
            rule: rule.rule,
            generation: rule.generation,
            issued_at: rule.issued_at,
        };
        let facts = |head| RuleFacts {
            cluster,
            attest,
            machine: &machine,
            namespace: &new.slot.namespace,
            service: &new.slot.service,
            head,
        };
        let creation = Creation {
            service: &new.slot.service,
            kind: new.slot.kind,
            spec: &new.resolved_spec,
        };
        let claim = match (&proof.evidence, &proof.claim) {
            (RuleEvidence::Pinned, None) => {
                rule.verify_pinned(&facts(None), creation)
                    .map_err(|refusal| logged(&rule, &new.slot.service, refusal))?;
                tracing::info!(
                    kind = %new.slot.kind,
                    "deploy of {} admitted under rule {} gen {} with its pinned image",
                    new.slot.service,
                    rule.rule,
                    rule.generation,
                );
                return Ok(RuleAdmitted {
                    by,
                    verified: None,
                    hook: None,
                });
            }
            (RuleEvidence::Online(_) | RuleEvidence::Verified(_), Some(claim)) => claim,
            _ => return Err(held(MismatchField::Stage)),
        };
        let head = match &proof.evidence {
            RuleEvidence::Online(token) => {
                // GitHub's token signature is one more check on the same budget.
                budget.reserve_one().map_err(refused)?;
                let claims = github
                    .claims(&rule, token.as_str(), &claim.authority, now)
                    .await
                    .map_err(|refusal| {
                        tracing::warn!(rule = %rule.rule, %refusal, "refused a deploy under a rule");
                        Error::RuleDeployRefused(RuleDeployRefusal::Proof(refusal))
                    })?;
                if claim.audience().ok().as_deref() != Some(claims.aud.as_str()) {
                    return Err(held(MismatchField::Audience));
                }
                match claim.authority {
                    // The approval names the commit: Cloud's dispatch ran on another.
                    CommitAuthority::Approved(_) => None,
                    CommitAuthority::Head => {
                        // GitHub's `sha` is the commit the push built.
                        if claims.sha != claim.commit {
                            return Err(held(MismatchField::Commit));
                        }
                        let head = github
                            .head(
                                rule.source.repository_id,
                                rule.head_branch(&claims),
                                proof.reader.as_ref(),
                            )
                            .await
                            .map_err(|error| {
                                Error::RuleDeployRefused(RuleDeployRefusal::Head(error))
                            })?;
                        Some(head)
                    }
                }
            }
            RuleEvidence::Verified(_) | RuleEvidence::Pinned => None,
        };
        if let CommitAuthority::Approved(approval) = &claim.authority {
            budget.reserve(approval, &trusted).map_err(refused)?;
        }
        rule.verify_authority(
            &claim.authority,
            &trusted,
            cluster,
            claim.commit,
            Some(&new.slot.namespace),
        )
        .map_err(|refusal| logged(&rule, &new.slot.service, refusal))?;
        rule.verify_creation(claim, &facts(head), creation)
            .map_err(|refusal| logged(&rule, &new.slot.service, refusal))?;
        let verified = match &proof.evidence {
            RuleEvidence::Verified(verified) => {
                if !holds(&proof.rule, claim, verified, &key) {
                    return Err(held(MismatchField::VerifiedBuild));
                }
                None
            }
            RuleEvidence::Pinned => None,
            RuleEvidence::Online(_) => {
                // Checked again: a newer generation may have landed while GitHub answered.
                self.approvals.mark_rule(&rule, admit, superseded)?;
                issue(
                    &CheckedOnline {
                        rule: &proof.rule,
                        claim,
                    },
                    &key,
                )
            }
        };
        let hook = (new.slot.kind == ContainerKind::PreDeployHook).then(|| HookPlace {
            rule: rule.rule,
            preview: rule.preview.as_ref().map(|_| new.slot.namespace.clone()),
            claim: claim.clone(),
            slot: new.slot.creation_key.clone(),
        });
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
        Ok(RuleAdmitted { by, verified, hook })
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
                    field: MismatchField::ApprovalRecord,
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
        admit: Admit,
    ) -> Result<RuleAdmitted, Error> {
        let github = super::rule_grant::GithubOnline::shared().ok_or(Error::RuleDeployRefused(
            RuleDeployRefusal::Rule(AuthorityRefusal::Mismatch {
                field: MismatchField::GitHubClient,
            }),
        ))?;
        self.admit_rule_deploy(want, proof, github, Timestamp::now(), budget, admit)
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
        AuthorityRefusal::Mismatch { field } => match field.rule_deploy_refusal() {
            Some(text) => text,
            None => return format!("This server refused the automatic deploy rule: {refusal}."),
        },
        AuthorityRefusal::RetiredBuilder { .. } => {
            "This automatic deploy rule pins a builder Ployz no longer trusts. Approve the rule again to pin the current builder."
        }
        AuthorityRefusal::UnknownKey { .. } | AuthorityRefusal::UnknownPasskey => {
            "This server doesn't trust the device that approved the automatic deploy rule."
        }
        AuthorityRefusal::Forgotten { .. } => {
            "This service's older versions were forgotten after the automatic deploy rule was approved. Approve the rule again. Until then it doesn't deploy this service, and Cloud can't move or drain what it built."
        }
        AuthorityRefusal::Malformed { .. }
        | AuthorityRefusal::BadSignature { .. }
        | AuthorityRefusal::OutOfScope { .. }
        | AuthorityRefusal::VolumeNotGranted { .. }
        | AuthorityRefusal::Unsigned { .. }
        | AuthorityRefusal::NeedsSignature { .. }
        | AuthorityRefusal::SignedName { .. }
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

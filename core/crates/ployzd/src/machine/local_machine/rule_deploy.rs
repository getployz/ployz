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
//!
//! A registry rule builds nothing. Its creation names an image of the approved repository
//! by digest, and this Machine pulls that digest from the registry itself, with the
//! Service's sealed login. The ID that pull placed is what the Container runs, whether it
//! is made, adopted or started, so a local image under the same name never runs unless the
//! registry serves the same content.
//!
//!   signature and Cluster ─▶ mark ─▶ floor ─▶ verify_registry_creation ─▶ pull ─▶ mark
//!     again ─▶ pin the pulled ID ─▶ hook once per Service, image and generation

use std::time::Instant;

use hkdf::hmac::Mac as _;
use ployz_core::{
    ContainerKind, ContainerWant, ImageDigest, QualifiedService, RunTarget, SealedLogin, Sealing,
    ServiceName,
    authority::{
        AuthorityRefusal, AutoDeployRule, Budget, BuildClaim, ChargedAuthority, CommitAuthority,
        MismatchField, RegistryPin, RuleEvidence, RuleFacts, RuleProof, RuleSource, Signed,
        Timestamp, VerifiedBuild, deploy::Creation, rule::verify_signature,
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
    /// Carries the pull's reason in its text, since a refusal reaches the caller without
    /// its causes.
    #[error(
        "This server couldn't pull the image the automatic deploy rule approves from its registry: {}",
        ployz_core::error_chain::row(.0)
    )]
    Pull(crate::docker_image::RegistryPull),
}

/// Where a registry rule's image comes from: the registry itself, asked by this Machine.
pub(crate) trait Registry {
    /// Pull `pin` with `login`, and answer the ID of the image the pull placed.
    ///
    /// # Errors
    /// Returns the pull's refusal, as [`pull_refused`] names it.
    async fn pull(
        &self,
        pin: &RegistryPin,
        login: Option<&SealedLogin>,
    ) -> Result<ImageDigest, Error>;
}

/// This Machine's Docker, reached only when a registry rule pulls, so a GitHub rule's
/// deploy never needs it.
impl Registry for LocalMachine {
    async fn pull(
        &self,
        pin: &RegistryPin,
        login: Option<&SealedLogin>,
    ) -> Result<ImageDigest, Error> {
        self.docker()?
            .pull_registry_pin(pin, login)
            .await
            .map_err(pull_refused)
    }
}

/// A registry pull's failure as a rule deploy's refusal: a registry that serves other
/// content under the digest is refused by name.
pub(crate) fn pull_refused(error: crate::docker_image::RegistryPull) -> Error {
    use crate::docker_image::RegistryPull;
    match error {
        RegistryPull::OtherDigest { .. } => {
            Error::RuleDeployRefused(RuleDeployRefusal::Rule(AuthorityRefusal::Mismatch {
                field: MismatchField::RegistryDigest,
            }))
        }
        error @ (RegistryPull::Registry(_)
        | RegistryPull::Login(_)
        | RegistryPull::Inspect(_)
        | RegistryPull::Pull(_)
        | RegistryPull::Moved) => Error::RuleDeployRefused(RuleDeployRefusal::Pull(error)),
    }
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
            ) => Some("Approve again under Automatic deploys in Ployz Cloud"),
            Self::Rule(_) | Self::Proof(_) | Self::Head(_) | Self::Pull(_) => None,
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
    /// The image a registry rule's pull placed, which the Container must run.
    pub(crate) pin: Option<ImageDigest>,
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
    push: HookPush,
    slot: ployz_core::CreationKey,
}

/// What a hook's place is for: one GitHub build, or one registry image a Service runs
/// under one generation.
#[derive(Clone, Debug)]
enum HookPush {
    Build(Box<BuildClaim>),
    Image {
        service: ServiceName,
        generation: u64,
        pin: RegistryPin,
    },
}

impl HookPlace {
    /// Take the place, refusing it when another slot took it for the same push.
    ///
    /// # Errors
    /// Returns [`Error::RuleDeployRefused`] for a place another slot took, and
    /// [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(crate) fn take(&self, approvals: &super::ConsumptionLog) -> Result<(), Error> {
        let refused = |refusal| Error::RuleDeployRefused(RuleDeployRefusal::Rule(refusal));
        match &self.push {
            HookPush::Build(claim) => approvals.consume_rule_hook(
                (self.rule, self.preview.as_ref()),
                claim,
                &self.slot,
                refused,
            ),
            HookPush::Image {
                service,
                generation,
                pin,
            } => approvals.consume_registry_hook(
                (self.rule, *generation),
                service,
                pin,
                &self.slot,
                refused,
            ),
        }
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
        (github, registry): (&impl Github, &impl Registry),
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
        // Every check the proof can cost is charged before any runs: the rule, the approval
        // its claim carries, and GitHub's token signature.
        let charged = budget.charge(&proof.rule, &trusted).map_err(refused)?;
        let authority = match &proof.claim {
            Some(claim) => claim.authority.charge(budget, &trusted).map_err(refused)?,
            None => ChargedAuthority::Head,
        };
        if let RuleEvidence::Online(_) = proof.evidence {
            budget.reserve_one().map_err(refused)?;
        }
        let key = self.held_sealing_key(record.phase());
        let (cluster, key) = match (self.held_sealing(record.phase()), key) {
            (Some(Sealing::Held { public_key }), Some(key)) => (public_key, key),
            _ => return Err(refused(AuthorityRefusal::NoClusterIdentity)),
        };
        let (rule, signer) = verify_signature(charged, Some(cluster)).map_err(refused)?;
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
        let facts = RuleFacts {
            cluster,
            attest,
            machine: &machine,
            namespace: &new.slot.namespace,
            service: &new.slot.service,
            head: None,
        };
        let creation = Creation {
            service: &new.slot.service,
            kind: new.slot.kind,
            spec: &new.resolved_spec,
        };
        if matches!(proof.evidence, RuleEvidence::Pinned) {
            if proof.claim.is_some() {
                return Err(held(MismatchField::Stage));
            }
            rule.verify_pinned(&facts, creation)
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
                pin: None,
            });
        }
        let claim = match (proof.build_claim().map_err(refused)?, &rule.source) {
            (Some(claim), RuleSource::Github(_)) => claim,
            (None, RuleSource::Registry(_)) => {
                let pin = rule
                    .verify_registry_creation(&facts, creation)
                    .map_err(|refusal| logged(&rule, &new.slot.service, refusal))?;
                let pulled = registry
                    .pull(&pin, new.registry_login.as_ref())
                    .await
                    .inspect_err(|error| {
                        tracing::warn!(rule = %rule.rule, image = %pin.reference(), "refused a deploy under a registry rule: {}", ployz_core::error_chain::row(error));
                    })?;
                // Checked again: a newer generation may have landed while the registry
                // answered.
                self.approvals.mark_rule(&rule, admit, superseded)?;
                // The caller's pin counts for nothing beside the Server's own pull, and
                // one that names other content is refused rather than overridden.
                if new
                    .image_id
                    .as_ref()
                    .is_some_and(|theirs| *theirs != pulled)
                {
                    return Err(held(MismatchField::Image));
                }
                let hook = (new.slot.kind == ContainerKind::PreDeployHook).then(|| HookPlace {
                    rule: rule.rule,
                    preview: None,
                    push: HookPush::Image {
                        service: new.slot.service.clone(),
                        generation: rule.generation,
                        pin: pin.clone(),
                    },
                    slot: new.slot.creation_key.clone(),
                });
                tracing::info!(
                    kind = %new.slot.kind,
                    admission_us = started.elapsed().as_micros(),
                    "deploy of {} admitted under registry rule {} gen {} image {} as {pulled}",
                    new.slot.service,
                    rule.rule,
                    rule.generation,
                    pin.reference(),
                );
                return Ok(RuleAdmitted {
                    by,
                    verified: None,
                    hook,
                    pin: Some(pulled),
                });
            }
            (Some(_), RuleSource::Registry(_)) | (None, RuleSource::Github(_)) => {
                return Err(held(MismatchField::RuleSource));
            }
        };
        let github_source = rule.github().map_err(refused)?;
        let head = match &proof.evidence {
            RuleEvidence::Online(token) => {
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
                                github_source.repository_id,
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
            RuleEvidence::Registry => return Err(held(MismatchField::RuleSource)),
        };
        rule.verify_authority(authority, cluster, claim.commit, Some(&new.slot.namespace))
            .map_err(|refusal| logged(&rule, &new.slot.service, refusal))?;
        let facts = RuleFacts { head, ..facts };
        rule.verify_creation(claim, &facts, creation)
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
            RuleEvidence::Registry => return Err(held(MismatchField::RuleSource)),
        };
        let hook = (new.slot.kind == ContainerKind::PreDeployHook).then(|| HookPlace {
            rule: rule.rule,
            preview: rule.preview.as_ref().map(|_| new.slot.namespace.clone()),
            push: HookPush::Build(Box::new(claim.clone())),
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
        Ok(RuleAdmitted {
            by,
            verified,
            hook,
            pin: None,
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
        self.admit_rule_deploy(want, proof, (github, self), Timestamp::now(), budget, admit)
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
            "This automatic deploy rule pins a builder Ployz no longer trusts. Approve again under Automatic deploys."
        }
        AuthorityRefusal::UnknownKey { .. } | AuthorityRefusal::UnknownPasskey => {
            "This server doesn't trust the device that approved the automatic deploy rule."
        }
        AuthorityRefusal::Forgotten { .. } => {
            "This service's older versions were forgotten after the automatic deploy rule was approved. Approve again under Automatic deploys. Until then it doesn't deploy this service, and Cloud can't move or drain what it built."
        }
        AuthorityRefusal::Malformed { .. }
        | AuthorityRefusal::BadSignature { .. }
        | AuthorityRefusal::OutOfScope { .. }
        | AuthorityRefusal::Unscoped { .. }
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

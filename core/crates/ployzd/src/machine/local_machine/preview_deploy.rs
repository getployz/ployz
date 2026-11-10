//! Deploys under a preview rule: an Operator's Container change in a preview's derived
//! Namespace, admitted on the rule, the builder's image token and, for a reviewed build,
//! the person's review, in place of a deploy approval.
//!
//! The rule's signature, this Machine's place in it, a live generation, the review, the
//! token's claims and audience, the preview those claims name, and the branch head read
//! from GitHub now all pass before anything runs. A creation must also run the image the
//! token names, built with secrets sealed from this Machine's own attest key, with the
//! settings the rule or the review approves once the preview's own names are put back.
//!
//!   signature ─▶ mark ─▶ review ─▶ claim (token, or this Machine's accepted build)
//!     ─▶ preview ─▶ head ─▶ creation: verify_creation ─▶ accepted build ─▶ hook once
//!                         ─▶ step:     verify_step (Start: verify_creation of its spec)

use std::time::Instant;

use ployz_core::{
    ContainerKind, ContainerNow, ContainerWant, Namespace, QualifiedService, RunTarget, Sealing,
    ServiceName,
    authority::{
        AuthorityRefusal, BuildClaim, CheckedReview, CommitAuthority, CommitSha, PreviewFacts,
        PreviewKey, PreviewProof, PreviewRule, RestartStep, Timestamp, deploy::Creation,
        preview::verify_review, rule::verify_signature,
    },
};

use super::{Error, LocalMachine, restart_approval::pinned, rule_grant::Github};

/// Why a Machine refused a preview's build grant or deploy.
#[derive(Debug, thiserror::Error)]
pub enum PreviewRefusal {
    #[error("{}", refusal_text(.0))]
    Rule(AuthorityRefusal),
    #[error("{0}")]
    Proof(crate::github::oidc::ProofRefusal),
    #[error("this server could not read the preview branch's head from GitHub")]
    Head(#[source] crate::github::head::HeadError),
}

impl PreviewRefusal {
    /// What a person runs next, when approving the rule again is what fixes it.
    pub(crate) fn next(&self) -> Option<&'static str> {
        match self {
            Self::Rule(
                AuthorityRefusal::Mismatch {
                    field: "a payload" | "a Service" | "a Machine" | "a Cluster",
                }
                | AuthorityRefusal::UnknownKey { .. }
                | AuthorityRefusal::UnknownPasskey,
            ) => Some("ployz github preview approve from an approved device"),
            Self::Rule(_) | Self::Proof(_) | Self::Head(_) => None,
        }
    }
}

/// Which preview a request is for and on what authority, once its rule, review and token
/// pass.
struct Admitted {
    rule: PreviewRule,
    review: Option<CheckedReview>,
    key: PreviewKey,
    head: Option<CommitSha>,
}

impl LocalMachine {
    /// Admit an Operator's Container change in a preview Namespace on `proof`, at `now`.
    /// A creation records the accepted build, and a hook takes its place once per push in
    /// each preview. A step on an existing Container consumes nothing. Returns the want,
    /// pinned to the Container seen for a step.
    ///
    /// # Errors
    /// Returns [`Error::PreviewDeployRefused`] naming what does not hold,
    /// [`Error::ApprovalRecord`] when the record cannot be read or written, and
    /// [`Error::DockerUnavailable`] or [`Error::Docker`] when a Container cannot be read.
    pub(crate) async fn admit_preview_deploy(
        &self,
        want: ContainerWant,
        proof: &PreviewProof,
        github: &impl Github,
        now: Timestamp,
    ) -> Result<ContainerWant, Error> {
        let started = Instant::now();
        let record = self.record();
        let (cluster, attest) = match (
            self.held_sealing(record.phase()),
            self.held_attest(record.phase()),
        ) {
            (Some(Sealing::Held { public_key }), Some(attest)) => (public_key, attest),
            _ => return Err(refused(AuthorityRefusal::NoClusterIdentity)),
        };
        record.machine().ok_or(Error::NotParticipating)?;
        let machine = record.id();
        let claim = &proof.claim;
        let Some((step, locator)) = RestartStep::of(&want) else {
            let ContainerWant::Running {
                container: RunTarget::New(new),
                ..
            } = &want
            else {
                unreachable!("only a new creation has no lifecycle step");
            };
            let Admitted {
                rule,
                review,
                key,
                head,
            } = self
                .admit_preview(proof, &new.slot.namespace, github, now)
                .await?;
            let facts = PreviewFacts {
                cluster,
                attest,
                machine: &machine,
                namespace: &new.slot.namespace,
                service: &new.slot.service,
                key: &key,
                head,
            };
            let created = rule
                .verify_creation(
                    claim,
                    &facts,
                    Creation {
                        service: &new.slot.service,
                        kind: new.slot.kind,
                        spec: &new.resolved_spec,
                    },
                    review.as_ref(),
                )
                .map_err(|refusal| logged(&rule, &new.slot.service, refusal))?;
            if created.runs_setup
                && !self
                    .first_in(&new.slot.namespace, &new.slot.service)
                    .await?
            {
                return Err(logged(
                    &rule,
                    &new.slot.service,
                    AuthorityRefusal::Mismatch {
                        field: "a Setup Command",
                    },
                ));
            }
            // Checked again: a newer generation may have landed while GitHub answered.
            self.approvals.accept(
                &rule,
                Some((&new.slot.namespace, &key)),
                claim,
                superseded(&rule),
            )?;
            if new.slot.kind == ContainerKind::PreDeployHook {
                self.approvals.consume_rule_hook(
                    (rule.rule, Some(&new.slot.namespace)),
                    claim,
                    &new.slot.creation_key,
                    refused,
                )?;
            }
            tracing::info!(
                kind = %new.slot.kind,
                preview = %key,
                setup = created.runs_setup,
                admission_us = started.elapsed().as_micros(),
                "deploy of {} admitted under preview rule {} gen {} commit {}",
                new.slot.service,
                rule.rule,
                rule.generation,
                short(claim),
            );
            return Ok(want);
        };
        let ContainerNow::Present(observed) = self.container_now(locator).await? else {
            return Err(refused(AuthorityRefusal::Mismatch {
                field: "a Container",
            }));
        };
        let Admitted {
            rule,
            review,
            key,
            head,
        } = self
            .admit_preview(proof, &observed.identity().namespace, github, now)
            .await?;
        let identity = observed.identity();
        let starts = step == RestartStep::Start;
        if starts && observed.kind == ContainerKind::PreDeployHook {
            return Err(refused(AuthorityRefusal::Mismatch {
                field: "a hook run",
            }));
        }
        // Starting a copy runs its spec, so it must be what the rule or review approves.
        // Any other step may act on any copy in the preview's Namespace.
        let service = if starts {
            &identity.name
        } else {
            &claim.service
        };
        let facts = PreviewFacts {
            cluster,
            attest,
            machine: &machine,
            namespace: &identity.namespace,
            service,
            key: &key,
            head,
        };
        let checked = if starts {
            rule.verify_creation(
                claim,
                &facts,
                Creation {
                    service,
                    kind: observed.kind,
                    spec: &observed.resolved_spec,
                },
                review.as_ref(),
            )
            .map(drop)
        } else {
            rule.verify_step(claim, &facts, review.as_ref())
        };
        checked.map_err(|refusal| logged(&rule, &identity.name, refusal))?;
        tracing::info!(
            ?step,
            preview = %key,
            container = observed.container_id.as_str().get(..12).unwrap_or_default(),
            "step on {identity} admitted under preview rule {} gen {} commit {}",
            rule.rule,
            rule.generation,
            short(claim),
        );
        Ok(pinned(want, observed.container_id))
    }

    /// The rule, review and preview `proof` names for a request in `namespace`, once the
    /// rule's signature, its mark, the review and the token's claims pass, with the
    /// preview branch's head read now for a trusted push. A build this Machine already
    /// admitted for `namespace` stands in for an expired token.
    async fn admit_preview(
        &self,
        proof: &PreviewProof,
        namespace: &Namespace,
        github: &impl Github,
        now: Timestamp,
    ) -> Result<Admitted, Error> {
        let trusted = self.record().trusted_keys();
        let (rule, _) = verify_signature(&proof.rule, &trusted, now).map_err(refused)?;
        // An ended generation still supersedes every one below it.
        self.approvals.mark_rule(&rule, superseded(&rule))?;
        let review = proof
            .review
            .as_ref()
            .map(|review| verify_review(review, &rule, &trusted, now))
            .transpose()
            .map_err(|refusal| {
                tracing::warn!(rule = %rule.rule, %refusal, "refused a preview review");
                refused(refusal)
            })?;
        let claim = &proof.claim;
        let key = match self
            .approvals
            .accepted_preview(rule.rule, namespace, claim)?
        {
            Some(key) => key,
            None => {
                let claims = github
                    .claims(&rule, proof.token.as_str(), now)
                    .await
                    .map_err(|refusal| {
                        tracing::warn!(rule = %rule.rule, %refusal, "refused a preview deploy");
                        Error::PreviewDeployRefused(PreviewRefusal::Proof(refusal))
                    })?;
                if claim.audience().ok().as_deref() != Some(claims.aud.as_str()) {
                    return Err(refused(AuthorityRefusal::Mismatch {
                        field: "an audience",
                    }));
                }
                rule.preview_key(&claims, claim.commit, claim.authority, review.as_ref())
                    .map_err(|refusal| logged(&rule, &claim.service, refusal))?
            }
        };
        let head = match (claim.authority, &key) {
            (CommitAuthority::Head, PreviewKey::Branch(branch)) => Some(
                github
                    .head(rule.source.repository_id, branch, proof.reader.as_ref())
                    .await
                    .map_err(|error| Error::PreviewDeployRefused(PreviewRefusal::Head(error)))?,
            ),
            (CommitAuthority::Head | CommitAuthority::Review(_), _) => None,
        };
        Ok(Admitted {
            rule,
            review,
            key,
            head,
        })
    }

    /// Whether `service` has no Service Container in `namespace` on this Machine, so its
    /// Hook Container may still run the Setup Commands: none deployed here yet.
    async fn first_in(&self, namespace: &Namespace, service: &ServiceName) -> Result<bool, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let copies = containers
            .service_copies(&QualifiedService::new(namespace.clone(), service.clone()))
            .await?;
        Ok(copies.is_empty())
    }

    /// Under enforcement, admit `want` on the preview proof Cloud attached, reading GitHub
    /// through the daemon's one client.
    ///
    /// # Errors
    /// As [`Self::admit_preview_deploy`]; without a usable GitHub client, every preview
    /// deploy is refused.
    pub(crate) async fn admit_preview_deploy_online(
        &self,
        want: ContainerWant,
        proof: &PreviewProof,
    ) -> Result<ContainerWant, Error> {
        let github = super::rule_grant::GithubOnline::shared().ok_or(refused(
            AuthorityRefusal::Mismatch {
                field: "a GitHub client",
            },
        ))?;
        self.admit_preview_deploy(want, proof, github, Timestamp::now())
            .await
    }
}

fn refused(refusal: AuthorityRefusal) -> Error {
    Error::PreviewDeployRefused(PreviewRefusal::Rule(refusal))
}

fn superseded(rule: &PreviewRule) -> impl FnOnce(AuthorityRefusal) -> Error + '_ {
    move |refusal| {
        tracing::warn!(
            rule = %rule.rule,
            generation = rule.generation,
            "refused a preview deploy: {refusal}"
        );
        refused(refusal)
    }
}

fn logged(rule: &PreviewRule, service: &ServiceName, refusal: AuthorityRefusal) -> Error {
    tracing::warn!(
        rule = %rule.rule,
        generation = rule.generation,
        "preview deploy of {service} refused under the rule: {refusal}"
    );
    refused(refusal)
}

/// The first 12 hex digits of the claim's commit.
fn short(claim: &BuildClaim) -> String {
    let mut commit = claim.commit.to_string();
    commit.truncate(12);
    commit
}

/// What a refused preview says to the person who reads the Deployment or the build.
pub(crate) fn refusal_text(refusal: &AuthorityRefusal) -> String {
    let text = match refusal {
        AuthorityRefusal::Mismatch { field: "a payload" } => {
            "This preview's settings differ from what the preview rule approves. Approve the rule again."
        }
        AuthorityRefusal::Mismatch { field: "an image" } => {
            "This server runs only the image GitHub built for this commit under the preview rule."
        }
        AuthorityRefusal::Mismatch {
            field: "an audience",
        } => "This build was made under another approval of the preview rule. Push again.",
        AuthorityRefusal::Mismatch { field: "a commit" } => {
            "This commit is no longer the head of its branch, or isn't the one that was reviewed."
        }
        AuthorityRefusal::Mismatch {
            field: "a generation",
        } => "A newer approval of the preview rule replaced this one.",
        AuthorityRefusal::Mismatch {
            field: "the attest key",
        } => "This image wasn't built with this cluster's build secrets.",
        AuthorityRefusal::Mismatch {
            field: "an ended rule",
        } => "Previews for this environment are off.",
        AuthorityRefusal::Mismatch { field: "a Service" } => {
            "The preview rule doesn't cover this service."
        }
        AuthorityRefusal::Mismatch { field: "a Machine" } => {
            "The preview rule was approved before this server joined. Approve it again."
        }
        AuthorityRefusal::Mismatch {
            field: "a Namespace",
        } => "This isn't the namespace the preview rule gives this branch.",
        AuthorityRefusal::Mismatch { field: "a Cluster" } => {
            "The preview rule is for another cluster."
        }
        AuthorityRefusal::Mismatch {
            field: "a hook run",
        } => "This push already ran its pre-deploy command in this preview.",
        AuthorityRefusal::Replayed { .. } => {
            "This push already ran its pre-deploy command in this preview."
        }
        AuthorityRefusal::Mismatch {
            field: "a Setup Command",
        } => "This preview already exists here, so its setup command doesn't run again.",
        AuthorityRefusal::Mismatch {
            field: "a Generated Value",
        } => "A generated value doesn't match what the preview rule generates.",
        AuthorityRefusal::Mismatch {
            field: ployz_core::authority::preview::SEALED_VALUE,
        } => "This preview uses a secret the preview rule doesn't share with it.",
        AuthorityRefusal::Mismatch {
            field: "an account",
        } => {
            "The preview rule doesn't trust the account that pushed. Approve it again with --trust and their GitHub ID."
        }
        AuthorityRefusal::Mismatch { field: "a branch" } => {
            "A push to the branch previews start from isn't a preview."
        }
        AuthorityRefusal::Mismatch { field: "a review" } => {
            "This build was made under another review. Review the preview again."
        }
        AuthorityRefusal::Mismatch {
            field: "the signer",
        } => {
            "A preview that uses secrets needs a review approved with a passkey or from an approved device."
        }
        AuthorityRefusal::Expired => "The preview's approval expired. Approve it again.",
        AuthorityRefusal::UnknownKey { .. } | AuthorityRefusal::UnknownPasskey => {
            "This server doesn't trust the device that approved the preview rule or review."
        }
        AuthorityRefusal::Malformed { .. }
        | AuthorityRefusal::BadSignature { .. }
        | AuthorityRefusal::ApproveOnly { .. }
        | AuthorityRefusal::Mismatch { .. }
        | AuthorityRefusal::Unsigned { .. }
        | AuthorityRefusal::NoClusterIdentity
        | AuthorityRefusal::ProofReplayed => {
            return format!("This server refused the preview: {refusal}.");
        }
    };
    text.to_owned()
}

#[cfg(test)]
#[path = "preview_deploy_tests.rs"]
mod tests;

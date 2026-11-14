//! Deploys under an automatic deploy rule: an Operator's Container change admitted on a
//! rule and the pinned builder's image token, in place of a person's deploy approval.
//!
//! The rule's signature, this Machine's place in it, a live generation, the token's
//! audience over the claim Cloud relays, and why the claim's commit may run now all pass
//! before anything runs: the branch head read from GitHub now, or a person's live
//! approval of the commit, which also waives the token's lifetime. A creation must also
//! run the image the token names, built with secrets sealed from this Machine's own
//! attest key, with every other setting the rule approves. Nothing here trusts Cloud's
//! word for a fact.
//!
//!   signature ─▶ mark ─▶ approval? ─▶ claim (token, or this Machine's accepted build)
//!     ─▶ head, unless approved
//!     ─▶ creation: verify_creation ─▶ accepted build ─▶ hook once
//!     ─▶ step:     verify_step (Start: verify_creation of the copy's own spec)

use std::time::Instant;

use ployz_core::{
    ContainerKind, ContainerNow, ContainerWant, RunTarget, Sealing, ServiceName,
    authority::{
        ApprovalFacts, AuthorityRefusal, AutoDeployRule, CommitAuthority, CommitNow, RestartStep,
        RuleFacts, RuleProof, Timestamp,
        deploy::{self, Creation},
        rule::{verify_commit_approval, verify_signature},
    },
};

use crate::github::oidc::Freshness;

use super::{Error, LocalMachine, restart_approval::pinned, rule_grant::Github};

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
                | AuthorityRefusal::UnknownPasskey,
            ) => Some("ployz github rule approve from an approved device"),
            Self::Rule(_) | Self::Proof(_) | Self::Head(_) => None,
        }
    }
}

impl LocalMachine {
    /// Admit an Operator's Container change under Device enforcement on `proof`, at
    /// `now`. A creation records the accepted build, and a hook takes its place once per
    /// push. A step on an existing Container consumes nothing. Returns the want, pinned
    /// to the Container seen for a step.
    ///
    /// # Errors
    /// Returns [`Error::RuleDeployRefused`] naming what does not hold,
    /// [`Error::ApprovalRecord`] when the record cannot be read or written, and
    /// [`Error::DockerUnavailable`] or [`Error::Docker`] when a Container cannot be read.
    pub(crate) async fn admit_rule_deploy(
        &self,
        want: ContainerWant,
        proof: &RuleProof,
        github: &impl Github,
        now: Timestamp,
    ) -> Result<ContainerWant, Error> {
        let started = Instant::now();
        let refused = |refusal| Error::RuleDeployRefused(RuleDeployRefusal::Rule(refusal));
        let record = self.record();
        let (rule, _) =
            verify_signature(&proof.rule, &record.trusted_keys(), now).map_err(refused)?;
        let held = |field| {
            tracing::warn!(rule = %rule.rule, field, "refused a deploy under a rule");
            refused(AuthorityRefusal::Mismatch { field })
        };
        let (cluster, attest) = match (
            self.held_sealing(record.phase()),
            self.held_attest(record.phase()),
        ) {
            (Some(Sealing::Held { public_key }), Some(attest)) => (public_key, attest),
            _ => return Err(refused(AuthorityRefusal::NoClusterIdentity)),
        };
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
        let claim = &proof.claim;
        let approved = proof
            .approval
            .as_ref()
            .map(|approval| {
                verify_commit_approval(
                    approval,
                    &rule,
                    &ApprovalFacts {
                        cluster,
                        machine: &machine,
                        trusted: &record.trusted_keys(),
                        now,
                    },
                )
                .map_err(|refusal| logged(&rule, &claim.service, refusal))
            })
            .transpose()?;
        if !self.approvals.accepted(&rule, claim)? {
            // The person, not freshness, is the authority under a live approval.
            let freshness = approved
                .as_ref()
                .map_or(Freshness::Now(now), Freshness::Approved);
            let claims = github
                .claims(&rule, proof.token.as_str(), freshness)
                .await
                .map_err(|refusal| {
                    tracing::warn!(rule = %rule.rule, %refusal, "refused a deploy under a rule");
                    Error::RuleDeployRefused(RuleDeployRefusal::Proof(refusal))
                })?;
            if claim.audience().ok().as_deref() != Some(claims.aud.as_str()) {
                return Err(held("an audience"));
            }
            // GitHub's `sha` is the commit a push built. An approved build ran on a
            // dispatch at whatever the head was, and its audience alone names the commit.
            if claim.authority == CommitAuthority::Head && claims.sha != claim.commit {
                return Err(held("a commit"));
            }
        }
        let commit = match &approved {
            Some(approved) => CommitNow::Approved(approved),
            None => CommitNow::Head(
                github
                    .head(
                        rule.source.repository_id,
                        &rule.source.branch,
                        proof.reader.as_ref(),
                    )
                    .await
                    .map_err(|error| Error::RuleDeployRefused(RuleDeployRefusal::Head(error)))?,
            ),
        };
        let Some((step, locator)) = RestartStep::of(&want) else {
            let ContainerWant::Running {
                container: RunTarget::New(new),
                ..
            } = &want
            else {
                unreachable!("only a new creation has no lifecycle step");
            };
            let facts = RuleFacts {
                cluster,
                attest,
                machine: &machine,
                namespace: &new.slot.namespace,
                service: &new.slot.service,
                commit,
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
            // Checked again: a newer generation may have landed while GitHub answered.
            self.approvals.accept(&rule, None, claim, superseded)?;
            if new.slot.kind == ContainerKind::PreDeployHook {
                self.approvals.consume_rule_hook(
                    (rule.rule, None),
                    claim,
                    approved.as_ref(),
                    &new.slot.creation_key,
                    refused,
                )?;
            }
            match &approved {
                Some(approved) => tracing::info!(
                    kind = %new.slot.kind,
                    admission_us = started.elapsed().as_micros(),
                    "deploy of {} admitted under rule {} gen {} commit {} approved by {}",
                    new.slot.service,
                    rule.rule,
                    rule.generation,
                    claim.commit.short(),
                    approved.by(),
                ),
                None => tracing::info!(
                    kind = %new.slot.kind,
                    admission_us = started.elapsed().as_micros(),
                    "deploy of {} admitted under rule {} gen {} commit {}",
                    new.slot.service,
                    rule.rule,
                    rule.generation,
                    claim.commit.short(),
                ),
            }
            return Ok(want);
        };
        let ContainerNow::Present(observed) = self.container_now(locator).await? else {
            return Err(held("a Container"));
        };
        let identity = observed.identity();
        let starts = step == RestartStep::Start;
        if starts && observed.kind == ContainerKind::PreDeployHook {
            return Err(held("a hook run"));
        }
        // Starting a copy runs its spec, so the copy must be the image and settings this
        // rule approves. Any other step may act on any copy in the rule's Environment.
        let service = if starts {
            &identity.name
        } else {
            &claim.service
        };
        let facts = RuleFacts {
            cluster,
            attest,
            machine: &machine,
            namespace: &identity.namespace,
            service,
            commit,
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
            )
            .map(drop)
        } else {
            rule.verify_step(claim, &facts)
        };
        checked.map_err(|refusal| logged(&rule, &identity.name, refusal))?;
        tracing::info!(
            ?step,
            container = observed.container_id.as_str().get(..12).unwrap_or_default(),
            "step on {identity} admitted under rule {} gen {} commit {}",
            rule.rule,
            rule.generation,
            claim.commit.short(),
        );
        Ok(pinned(want, observed.container_id))
    }

    /// Under enforcement, admit `want` on the rule proof Cloud attached, reading GitHub
    /// through the daemon's one client.
    ///
    /// # Errors
    /// As [`Self::admit_rule_deploy`]; without a usable GitHub client, every rule deploy
    /// is refused.
    pub(crate) async fn admit_rule_deploy_online(
        &self,
        want: ContainerWant,
        proof: &RuleProof,
    ) -> Result<ContainerWant, Error> {
        let github = super::rule_grant::GithubOnline::shared().ok_or(Error::RuleDeployRefused(
            RuleDeployRefusal::Rule(AuthorityRefusal::Mismatch {
                field: "a GitHub client",
            }),
        ))?;
        self.admit_rule_deploy(want, proof, github, Timestamp::now())
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
            field: "an approval",
        } => "The approval that came with this deploy is for another service or commit.",
        AuthorityRefusal::Mismatch {
            field: "an authority",
        } => "This image deploys only with a person's approval of its commit.",
        AuthorityRefusal::Expired => {
            "The approval for this rollback expired. Run ployz deployment redeploy again."
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
        AuthorityRefusal::Mismatch {
            field: deploy::HOST_ACCESS,
        } => {
            "This service reaches the server itself, so only an approved device can approve its commit. Run ployz deployment redeploy on that device."
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
        AuthorityRefusal::Replayed { .. } => "This push already ran its pre-deploy command here.",
        AuthorityRefusal::Mismatch {
            field: "the signer" | "a passkey",
        } => {
            "An automatic deploy rule needs your passkey or an approved device, not a browser \
             session. Approve it again."
        }
        AuthorityRefusal::UnknownKey { .. } | AuthorityRefusal::UnknownPasskey => {
            "This server doesn't trust the device that approved the automatic deploy rule."
        }
        AuthorityRefusal::Malformed { .. }
        | AuthorityRefusal::BadSignature { .. }
        | AuthorityRefusal::ApproveOnly { .. }
        | AuthorityRefusal::Mismatch { .. }
        | AuthorityRefusal::Unsigned { .. }
        | AuthorityRefusal::NoClusterIdentity
        | AuthorityRefusal::ProofReplayed
        | AuthorityRefusal::Unreceipted
        | AuthorityRefusal::ForeignMachineKey { .. }
        | AuthorityRefusal::BadMachineSignature { .. }
        | AuthorityRefusal::MalformedMachineStatement { .. } => {
            return format!("This server refused the automatic deploy rule: {refusal}.");
        }
    };
    text.to_owned()
}

#[cfg(test)]
#[path = "rule_deploy_tests.rs"]
mod tests;

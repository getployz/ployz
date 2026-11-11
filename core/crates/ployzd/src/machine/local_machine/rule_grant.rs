//! A Build Grant under an automatic deploy rule: every check a grant Machine runs on its
//! own facts before it opens the build's Sealed record.
//!
//! The rule's signature, this Machine's place in it, the GitHub token and its claims, the
//! audience recomputed from what this Machine holds, the commit's authority read from
//! GitHub now, and the Sealed record's digest all pass first. Then the token is spent.
//! Nothing here opens a Sealed value, and nothing trusts Cloud's word for a fact.

use std::future::Future;

use ployz_core::{
    BuildGrantId, GithubToken, RuleGrant, SealedRecord,
    authority::{
        ApprovalDigest, ApprovalFacts, AuthorityRefusal, AutoDeployRule, BuildClaim, BuildStage,
        ClaimRule, CommitAuthority, CommitSha, GithubClaims, PolicyDigest, RecipientKey,
        SealedDigest, Timestamp,
        rule::{RuleEvent, verify_commit_approval, verify_signature},
    },
};

use super::{Error, LocalMachine};
use crate::github::{
    head::{Ancestry, GithubApi, HeadError},
    oidc::{Freshness, IssuerKeys, ProofRefusal},
};

/// What a rule grant learns from GitHub: a token's verified claims, a branch head, and
/// how a branch stands to a commit.
pub(crate) trait Github: Sync {
    /// The claims of `token` once its signature, issuer and every claim `rule` names
    /// check out, and its lifetime at `freshness`.
    fn claims(
        &self,
        rule: &impl ClaimRule,
        token: &str,
        freshness: Freshness<'_>,
    ) -> impl Future<Output = Result<GithubClaims, ProofRefusal>> + Send;

    /// The commit `branch` of repository `repository_id` points at now.
    fn head(
        &self,
        repository_id: u64,
        branch: &str,
        reader: Option<&GithubToken>,
    ) -> impl Future<Output = Result<CommitSha, HeadError>> + Send;

    /// How commit `head` of repository `repository_id` stands to `commit`.
    fn ancestry(
        &self,
        repository_id: u64,
        commit: CommitSha,
        head: CommitSha,
        reader: Option<&GithubToken>,
    ) -> impl Future<Output = Result<Ancestry, HeadError>> + Send;
}

/// GitHub itself: its issuer's keys over TLS, and `api.github.com`.
pub(crate) struct GithubOnline {
    keys: IssuerKeys,
    api: GithubApi,
}

impl GithubOnline {
    /// The daemon's one client, so every grant shares the cached key set. `None` when the
    /// HTTPS client could not be built, which refuses every rule grant.
    pub(crate) fn shared() -> Option<&'static Self> {
        static SHARED: std::sync::OnceLock<Option<GithubOnline>> = std::sync::OnceLock::new();
        SHARED
            .get_or_init(|| match (IssuerKeys::github(), GithubApi::github()) {
                (Ok(keys), Ok(api)) => Some(Self { keys, api }),
                (Err(error), _) => {
                    tracing::warn!(%error, "could not build the GitHub token key client");
                    None
                }
                (_, Err(error)) => {
                    tracing::warn!(%error, "could not build the GitHub API client");
                    None
                }
            })
            .as_ref()
    }
}

impl Github for GithubOnline {
    async fn claims(
        &self,
        rule: &impl ClaimRule,
        token: &str,
        freshness: Freshness<'_>,
    ) -> Result<GithubClaims, ProofRefusal> {
        self.keys.check(rule, token, freshness).await
    }

    async fn head(
        &self,
        repository_id: u64,
        branch: &str,
        reader: Option<&GithubToken>,
    ) -> Result<CommitSha, HeadError> {
        self.api
            .branch_head(repository_id, branch, reader_token(reader).as_ref())
            .await
    }

    async fn ancestry(
        &self,
        repository_id: u64,
        commit: CommitSha,
        head: CommitSha,
        reader: Option<&GithubToken>,
    ) -> Result<Ancestry, HeadError> {
        self.api
            .compare(repository_id, commit, head, reader_token(reader).as_ref())
            .await
    }
}

fn reader_token(reader: Option<&GithubToken>) -> Option<crate::github::head::GithubToken> {
    reader.map(|token| crate::github::head::GithubToken::new(token.as_str().into()))
}

/// GitHub as a test fixes it: tokens verify against `keys` at `now`, every branch points
/// at `head`, and `head` stands to every commit as `ancestry` says, or reads as
/// unreadable for `None`. A compare against any other commit reads as unreadable.
#[cfg(test)]
pub(crate) struct FixedGithub {
    pub(crate) keys: crate::github::oidc::KeySet,
    pub(crate) head: CommitSha,
    pub(crate) ancestry: Option<Ancestry>,
    pub(crate) now: Timestamp,
}

#[cfg(test)]
impl FixedGithub {
    /// The clock a test's tokens are valid at, in place of `_now`.
    pub(crate) const fn now(&self, _now: Timestamp) -> Timestamp {
        self.now
    }
}

#[cfg(test)]
impl Github for FixedGithub {
    async fn claims(
        &self,
        rule: &impl ClaimRule,
        token: &str,
        freshness: Freshness<'_>,
    ) -> Result<GithubClaims, ProofRefusal> {
        let claims = crate::github::oidc::verify(token, &self.keys, freshness)
            .map_err(ProofRefusal::Token)?;
        rule.verify_claims(&claims).map_err(ProofRefusal::Claim)?;
        Ok(claims)
    }

    async fn head(
        &self,
        _repository_id: u64,
        _branch: &str,
        _reader: Option<&GithubToken>,
    ) -> Result<CommitSha, HeadError> {
        Ok(self.head)
    }

    async fn ancestry(
        &self,
        _repository_id: u64,
        _commit: CommitSha,
        head: CommitSha,
        _reader: Option<&GithubToken>,
    ) -> Result<Ancestry, HeadError> {
        self.ancestry
            .filter(|_| head == self.head)
            .ok_or_else(|| HeadError::Unreadable {
                branch: head.to_string(),
            })
    }
}

/// A rule grant every check passed, its token spent: what the minted grant remembers and
/// seals its build secrets under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AdmittedRule {
    pub(crate) recipient: RecipientKey,
    /// The grant token's audience, which binds the sealed response to this one build.
    pub(crate) audience: String,
    pub(crate) rule: ployz_core::authority::RuleId,
    pub(crate) generation: u64,
    pub(crate) service: ployz_core::ServiceName,
    pub(crate) commit: CommitSha,
}

impl LocalMachine {
    /// Admit a grant for `holder` under `grant`'s rule, with `sealed` the record Cloud
    /// relays, at `now`. Every check runs before anything opens, in the order
    /// `d-build-proof.md` lists them, and the token is spent last. A build of a commit
    /// that is not the head needs a person's live approval naming it, a dispatched run,
    /// and the commit on the rule's branch as GitHub answers now.
    ///
    /// # Errors
    /// Returns [`Error::RuleGrantRefused`] naming what does not match,
    /// [`Error::RuleProofRefused`] for a token GitHub did not sign or that names another
    /// repository, ref, event, workflow, builder or runner, [`Error::RuleHeadUnread`]
    /// when GitHub does not answer for the branch head, and [`Error::ApprovalRecord`] when
    /// the spent token cannot be recorded.
    pub(crate) async fn admit_rule_grant(
        &self,
        grant: &RuleGrant,
        holder: Option<&BuildGrantId>,
        sealed: Option<&SealedRecord>,
        github: &impl Github,
        now: Timestamp,
    ) -> Result<AdmittedRule, Error> {
        let record = self.record();
        let (rule, _) = verify_signature(&grant.rule, &record.trusted_keys(), now)
            .map_err(Error::RuleGrantRefused)?;
        let refused = |field| {
            tracing::warn!(rule = %rule.rule, field, "refused a rule build's grant");
            Error::RuleGrantRefused(AuthorityRefusal::Mismatch { field })
        };
        let cluster =
            self.held_sealing(record.phase())
                .and_then(|sealing| match sealing {
                    ployz_core::Sealing::Held { public_key } => Some(public_key),
                    ployz_core::Sealing::NeedsAccess
                    | ployz_core::Sealing::AwaitsRecovery { .. } => None,
                })
                .ok_or(Error::RuleGrantRefused(AuthorityRefusal::NoClusterIdentity))?;
        if rule.cluster != cluster {
            return Err(refused("a Cluster"));
        }
        record.machine().ok_or(Error::NotParticipating)?;
        if !rule.machines.contains(&record.id()) {
            return Err(refused("a Machine"));
        }
        // An ended generation still supersedes every one below it.
        self.approvals.mark_rule(&rule, Error::RuleGrantRefused)?;
        if rule.ended {
            return Err(refused("an ended rule"));
        }
        let service = rule
            .service(&grant.service)
            .ok_or_else(|| refused("a Service"))?;
        let holder = holder.ok_or_else(|| refused("a holder"))?;
        let claims = github
            .claims(&rule, grant.proof.as_str(), now.into())
            .await
            .map_err(Error::RuleProofRefused)?;
        let expected = BuildClaim {
            cluster,
            rule: rule.rule,
            generation: rule.generation,
            service: grant.service.clone(),
            policy: PolicyDigest::of(&service.policy).map_err(|_| refused("a policy"))?,
            commit: grant.commit,
            authority: grant.authority,
            stage: BuildStage::Grant {
                holder: *holder,
                recipient: grant.recipient,
            },
        };
        let audience = expected.audience().map_err(|_| refused("an audience"))?;
        if claims.aud != audience {
            return Err(refused("an audience"));
        }
        match grant.authority {
            // A reviewed build is a preview's, which only a preview rule admits.
            CommitAuthority::Review(_) => return Err(refused("an authority")),
            CommitAuthority::Head => {
                if grant.commit != claims.sha {
                    return Err(refused("a commit"));
                }
                let head = github
                    .head(
                        rule.source.repository_id,
                        &rule.source.branch,
                        grant.reader.as_ref(),
                    )
                    .await
                    .map_err(Error::RuleHeadUnread)?;
                if head != grant.commit {
                    return Err(refused("a commit"));
                }
            }
            CommitAuthority::Approved(digest) => {
                let approval = grant
                    .approval
                    .as_ref()
                    .filter(|approval| ApprovalDigest::of(approval) == digest)
                    .ok_or_else(|| refused("an approval"))?;
                let approved = verify_commit_approval(
                    approval,
                    &rule,
                    &ApprovalFacts {
                        cluster,
                        machine: &record.id(),
                        trusted: &record.trusted_keys(),
                        now,
                    },
                )
                .map_err(|refusal| {
                    tracing::warn!(rule = %rule.rule, %refusal, "refused a rule build's approval");
                    Error::RuleGrantRefused(refusal)
                })?;
                if !approved.names(&rule, &grant.service, grant.commit) {
                    return Err(refused("an approval"));
                }
                // GitHub starts an approved build only when someone asks it to; a push
                // always builds its own head.
                if claims.event_name != RuleEvent::WorkflowDispatch.as_str() {
                    return Err(refused("event_name"));
                }
                on_branch(github, &rule, grant).await?;
                tracing::info!(
                    rule = %rule.rule,
                    approver = %approved.by(),
                    "a person approved this build's commit"
                );
            }
        }
        let digest = sealed
            .map(SealedDigest::of)
            .transpose()
            .map_err(|_| refused("the Sealed values"))?;
        if digest != service.secrets {
            return Err(refused("the Sealed values"));
        }
        self.approvals.consume_rule_proof(
            &rule,
            Error::RuleGrantRefused,
            &claims.jti,
            claims.exp,
            now,
        )?;
        tracing::info!(
            rule = %rule.rule,
            generation = rule.generation,
            service = %grant.service,
            commit = %grant.commit,
            "admitted a rule build's grant"
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

/// Refuse unless `grant`'s commit is the head of `rule`'s branch or one of its ancestors,
/// as GitHub answers this Machine now. The head comes from the branch's own ref, and the
/// compare names two commits, so a tag named like the branch can't stand in for it. A
/// commit GitHub cannot find while it can read the branch is not on it.
async fn on_branch(
    github: &impl Github,
    rule: &AutoDeployRule,
    grant: &RuleGrant,
) -> Result<(), Error> {
    let source = &rule.source;
    let reader = grant.reader.as_ref();
    let head = github
        .head(source.repository_id, &source.branch, reader)
        .await
        .map_err(Error::RuleHeadUnread)?;
    match github
        .ancestry(source.repository_id, grant.commit, head, reader)
        .await
    {
        Ok(ancestry) if ancestry.on_branch() => Ok(()),
        Ok(_) | Err(HeadError::Unreadable { .. }) => {
            tracing::warn!(rule = %rule.rule, commit = %grant.commit, %head, "refused a commit not on the rule's branch");
            Err(Error::RuleGrantRefused(AuthorityRefusal::Mismatch {
                field: "a commit",
            }))
        }
        Err(error) => Err(Error::RuleHeadUnread(error)),
    }
}

#[cfg(test)]
#[path = "rule_grant_tests.rs"]
pub(crate) mod tests;

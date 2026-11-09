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
        AuthorityRefusal, AutoDeployRule, BuildClaim, BuildStage, CommitAuthority, CommitSha,
        GithubClaims, PolicyDigest, RecipientKey, SealedDigest, Timestamp, rule::verify_signature,
    },
};

use super::{Error, LocalMachine};
use crate::github::{
    head::{GithubApi, HeadError},
    oidc::{IssuerKeys, ProofRefusal},
};

/// What a rule grant learns from GitHub: a token's verified claims and a branch head.
pub(crate) trait Github: Sync {
    /// The claims of `token` once its signature, issuer, lifetime and every claim `rule`
    /// names check out at `now`.
    fn claims(
        &self,
        rule: &AutoDeployRule,
        token: &str,
        now: Timestamp,
    ) -> impl Future<Output = Result<GithubClaims, ProofRefusal>> + Send;

    /// The commit `branch` of repository `repository_id` points at now.
    fn head(
        &self,
        repository_id: u64,
        branch: &str,
        reader: Option<&GithubToken>,
    ) -> impl Future<Output = Result<CommitSha, HeadError>> + Send;
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
        rule: &AutoDeployRule,
        token: &str,
        now: Timestamp,
    ) -> Result<GithubClaims, ProofRefusal> {
        self.keys.check(rule, token, now).await
    }

    async fn head(
        &self,
        repository_id: u64,
        branch: &str,
        reader: Option<&GithubToken>,
    ) -> Result<CommitSha, HeadError> {
        let reader =
            reader.map(|token| crate::github::head::GithubToken::new(token.as_str().into()));
        self.api
            .branch_head(repository_id, branch, reader.as_ref())
            .await
    }
}

/// GitHub as a test fixes it: tokens verify against `keys` at `now`, and every branch
/// points at `head`.
#[cfg(test)]
pub(crate) struct FixedGithub {
    pub(crate) keys: crate::github::oidc::KeySet,
    pub(crate) head: CommitSha,
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
        rule: &AutoDeployRule,
        token: &str,
        now: Timestamp,
    ) -> Result<GithubClaims, ProofRefusal> {
        let claims =
            crate::github::oidc::verify(token, &self.keys, now).map_err(ProofRefusal::Token)?;
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
    /// `d-build-proof.md` lists them, and the token is spent last.
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
        let refused = |field| Error::RuleGrantRefused(AuthorityRefusal::Mismatch { field });
        let record = self.record();
        let (rule, _) = verify_signature(&grant.rule, &record.trusted_keys(), now)
            .map_err(Error::RuleGrantRefused)?;
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
        if rule.ended {
            return Err(refused("an ended rule"));
        }
        let service = rule.service(&grant.service).ok_or(refused("a Service"))?;
        let holder = holder.ok_or(refused("a holder"))?;
        let claims = github
            .claims(&rule, grant.proof.as_str(), now)
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
            tracing::warn!(rule = %rule.rule, claim = "aud", "refused a rule build's grant");
            return Err(refused("an audience"));
        }
        match grant.authority {
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
        }
        let digest = sealed
            .map(SealedDigest::of)
            .transpose()
            .map_err(|_| refused("the Sealed values"))?;
        if digest != service.secrets {
            return Err(refused("the Sealed values"));
        }
        self.approvals
            .consume_rule_proof(&rule, &claims.jti, claims.exp, now)?;
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

#[cfg(test)]
#[path = "rule_grant_tests.rs"]
pub(crate) mod tests;

//! The automatic deploy rule a person signs once, the build claim a pinned GitHub builder
//! names in the audience of each token it asks GitHub for, and the token claims a Machine
//! checks against the rule.
//!
//! A rule names the repository by ID, the branch, the Environment, the builder workflow
//! pinned by commit and, per Service, the settings and build policy it approves. A Machine
//! verifies a GitHub token's signature itself, then checks every claim here against the
//! signed rule. Nothing here verifies a JWT or touches a key, so it builds for the browser.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use super::{
    canonical::{self, CanonicalError, Timestamp},
    deploy::{Creation, PayloadDigest, content_addressed, drop_nulls},
    keys::{Approver, Purpose, Signed, Statement},
    refusal::{AuthorityRefusal, field, first_mismatch},
    restart::SignerKey,
    session::{ActionFamily, GrantScope},
    trusted_key::TrustedKeys,
};
use crate::value::hex_newtype;
use crate::{
    BuildGrantId, ContainerKind, GithubToken, MachineId, Namespace, RequestedServiceSpec,
    SealedRecord, SealingPublicKey, ServiceName,
};

/// What prefixes every build audience, so a token minted for a Ployz build names no other
/// relying party's audience.
pub const AUDIENCE_PREFIX: &str = "ployz-build:";

hex_newtype!(
    /// One rule across all its generations: 128 random bits the approver's client makes,
    /// so Cloud cannot pick one that collides with another rule's.
    RuleId, "rule ID", 16, ""
);
hex_newtype!(
    /// A Git commit as GitHub names it: 40 lowercase hex digits.
    CommitSha, "commit", 20, ""
);
impl CommitSha {
    /// The first 12 hex digits, for a line a person reads.
    #[must_use]
    pub fn short(&self) -> String {
        let mut text = self.to_string();
        text.truncate(12);
        text
    }
}

hex_newtype!(
    /// SHA-256 of a [`BuildPolicy`] in canonical JSON.
    PolicyDigest, "policy digest", 32, ""
);
hex_newtype!(
    /// SHA-256 of a build's [`SealedRecord`] in canonical JSON: ciphertexts only.
    SealedDigest, "sealed digest", 32, ""
);
hex_newtype!(
    /// The content digest of a built image, `sha256:<64 hex>`.
    ImageDigest, "image digest", 32, "sha256:"
);
hex_newtype!(
    /// The X25519 key a builder makes for one build, which its build secrets are sealed to.
    RecipientKey, "recipient key", 32, ""
);
hex_newtype!(
    /// SHA-256 of a preview review's signing input, which is also the challenge a passkey
    /// answers to approve it.
    ReviewDigest, "review digest", 32, ""
);
hex_newtype!(
    /// SHA-256 of a commit approval's signing input, which a build under it names.
    ApprovalDigest, "approval digest", 32, ""
);
hex_newtype!(
    /// The X25519 key every Machine of a Cluster derives from its Sealing Key, which seals
    /// build secrets to a builder and which the builder writes into its image token.
    AttestKey, "attest key", 32, ""
);

impl PolicyDigest {
    /// # Errors
    /// Returns a [`CanonicalError`] when the policy has no canonical form.
    pub fn of(policy: &BuildPolicy) -> Result<Self, CanonicalError> {
        Ok(Self(
            Sha256::digest(canonical::to_canonical(policy)?).into(),
        ))
    }
}

impl SealedDigest {
    /// The digest of `record` as Cloud relays it. A field serde writes as `null` is left
    /// out, as absent.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the record has no canonical form.
    pub fn of(record: &SealedRecord) -> Result<Self, CanonicalError> {
        let mut value = serde_json::to_value(record)
            .map_err(|error| CanonicalError::Parse(error.to_string()))?;
        drop_nulls(&mut value);
        Ok(Self(
            Sha256::digest(canonical::to_canonical(&value)?).into(),
        ))
    }
}

/// A workflow event a rule builds on, as GitHub writes it in `event_name`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RuleEvent {
    Push,
    WorkflowDispatch,
}

impl RuleEvent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::WorkflowDispatch => "workflow_dispatch",
        }
    }
}

/// Where the builder's job runs, as GitHub writes it in `runner_environment`. Only
/// GitHub's own runners isolate one job from the next.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub enum Runner {
    #[serde(rename = "github-hosted")]
    GithubHosted,
}

impl Runner {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GithubHosted => "github-hosted",
        }
    }
}

/// The reusable workflow that builds, pinned by commit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "RuleBuilder")]
pub struct Builder {
    /// `job_workflow_ref` as GitHub writes it:
    /// `<owner>/<repository>/<path>@<ref>`.
    pub r#ref: String,
    pub sha: CommitSha,
}

/// The commit of `getployz/build` whose reusable workflow a rule approved from the CLI
/// pins by default. It must be in `getployz/build`'s own history: GitHub won't start a
/// run that calls a commit only a fork holds. Cloud's rule template pins the same.
pub const BUILDER_SHA: &str = "a19b4fca3adea371c25ec29f181e8c7d21037d1e";

impl Builder {
    /// `getployz/build`'s reusable workflow at [`BUILDER_SHA`].
    #[must_use]
    pub fn pinned() -> Self {
        Self {
            r#ref: format!("getployz/build/.github/workflows/build.yml@{BUILDER_SHA}"),
            sha: CommitSha::parse(BUILDER_SHA).expect("the pinned builder is a commit"),
        }
    }
}

/// The repository, branch and workflow a rule builds from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuleSource {
    /// GitHub's repository ID, which a rename or transfer keeps.
    pub repository_id: u64,
    /// GitHub's ID of the repository's owner.
    pub owner_id: u64,
    /// `owner/name`, which `workflow_ref` names and a person reads.
    pub repository: String,
    pub branch: String,
    /// The caller workflow's path in the repository, such as
    /// `.github/workflows/ployz-build.yml`.
    pub workflow: String,
    /// Sorted and distinct.
    #[serde(deserialize_with = "canonical::sorted_distinct")]
    pub events: Vec<RuleEvent>,
    pub builder: Builder,
    pub runner: Runner,
}

/// How a build describes its image, with the settings that method reads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PolicyMethod {
    Dockerfile {
        /// Relative to the context.
        path: String,
    },
    Railpack {
        /// Railpack's build command, or `None` for the one it detects.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        command: Option<String>,
    },
}

/// Which cache a rule build may use. A registry cache is not one of them: Cloud can write
/// it through Registry Credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PolicyCache {
    Off,
    /// The GitHub Actions cache, scoped by the policy's digest.
    Actions,
}

/// Every input the builder passes to `ployz build` other than the source tree. Repository
/// files may change the code, the Dockerfile included, but not this: the builder reports
/// the digest of the policy it used, and the Machine compares it with the rule's.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BuildPolicy {
    pub method: PolicyMethod,
    /// The build context, relative to the repository root.
    pub context: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub target: Option<String>,
    /// Plain build args.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(optional, as = "Option<BTreeMap<String, String>>")]
    pub args: BTreeMap<String, String>,
    /// The names of the build secrets, whose sealed values the rule's `secrets` digest
    /// binds.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    #[ts(optional, as = "Option<BTreeSet<String>>")]
    pub secrets: BTreeSet<String>,
    /// Such as `linux/amd64`.
    pub platforms: BTreeSet<String>,
    pub cache: PolicyCache,
}

impl BuildPolicy {
    /// The policy a Service built from `root_dir` with `settings` builds with: its
    /// method, context, and its plain and Sealed variables as build args and secrets.
    /// `platforms` and `cache` come from the rule, not from the Service.
    #[must_use]
    pub fn derive(
        root_dir: &str,
        settings: &crate::config::ServiceBuildConfig,
        service: &RequestedServiceSpec,
        platforms: BTreeSet<String>,
        cache: PolicyCache,
    ) -> Self {
        let method = match settings.build_method {
            crate::config::BuildMethod::Dockerfile => PolicyMethod::Dockerfile {
                path: settings
                    .dockerfile_path
                    .clone()
                    .unwrap_or_else(|| crate::config::DEFAULT_DOCKERFILE_PATH.to_owned()),
            },
            crate::config::BuildMethod::Railpack => PolicyMethod::Railpack {
                command: settings.command.clone(),
            },
        };
        let mut args = BTreeMap::new();
        let mut secrets = BTreeSet::new();
        for (name, value) in &service.container.environment {
            match value.text() {
                Some(text) => {
                    args.insert(name.clone(), text.to_owned());
                }
                None => {
                    secrets.insert(name.clone());
                }
            }
        }
        Self {
            method,
            context: root_dir.to_owned(),
            target: None,
            args,
            secrets,
            platforms,
            cache,
        }
    }
}

/// One Service a rule deploys on every push.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuleService {
    pub name: ServiceName,
    /// [`PayloadDigest`] of the Service's requested spec with `container.image` emptied:
    /// every setting but the image, which the image token fills.
    pub template: PayloadDigest,
    /// The policy itself, not its digest, so the builder reads what it must build with
    /// from the rule it was relayed.
    pub policy: BuildPolicy,
    /// The build's Sealed record, or `None` for a build with no secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub secrets: Option<SealedDigest>,
}

/// A person's standing approval of every push to one branch: built by one pinned builder,
/// deployed with the settings it names, into one Environment on the Machines it names.
/// Signed by a Device, or answered by a passkey's own prompt; a browser key under a
/// session grant cannot sign it, because it authorizes every future push.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct AutoDeployRule {
    /// The Cluster's Sealing public key, which every Machine holds and Cloud cannot mint.
    pub cluster: SealingPublicKey,
    pub rule: RuleId,
    /// Each approval of the rule adds one. A Machine refuses one lower than it has seen.
    pub generation: u64,
    pub namespace: Namespace,
    /// Sorted and distinct.
    #[serde(deserialize_with = "canonical::sorted_distinct")]
    pub machines: Vec<MachineId>,
    pub source: RuleSource,
    /// Sorted by name and distinct.
    #[serde(deserialize_with = "services_by_name")]
    pub services: Vec<RuleService>,
    pub issued_at: Timestamp,
    /// True for the generation that switches the rule off.
    pub ended: bool,
    pub signer: SignerKey,
}

impl Statement for AutoDeployRule {
    const PURPOSE: Purpose = Purpose::Rule;
}

impl AutoDeployRule {
    /// What the rule approves for `name`, or `None` when it names no such Service.
    #[must_use]
    pub fn service(&self, name: &ServiceName) -> Option<&RuleService> {
        self.services.iter().find(|service| service.name == *name)
    }

    /// Check a verified GitHub token's claims against this rule, in the order
    /// `d-build-proof.md` lists them. The audience is the caller's: it depends on the
    /// holder, recipient or image in hand.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first claim that does not match.
    pub fn verify_claims(&self, claims: &GithubClaims) -> Result<(), AuthorityRefusal> {
        self.source
            .verify_claims(claims, &format!("refs/heads/{}", self.source.branch))
    }
}

impl RuleSource {
    /// Check `claims` came from this repository's workflow at `git_ref`, run by the pinned
    /// builder on a GitHub-hosted runner for an event this source builds on.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first claim that does not match.
    pub fn verify_claims(
        &self,
        claims: &GithubClaims,
        git_ref: &str,
    ) -> Result<(), AuthorityRefusal> {
        first_mismatch([
            ("repository_id", claims.repository_id == self.repository_id),
            (
                "repository_owner_id",
                claims.repository_owner_id == self.owner_id,
            ),
            ("ref_type", claims.ref_type == "branch"),
            ("ref", claims.git_ref == git_ref),
            (
                "event_name",
                self.events
                    .iter()
                    .any(|event| event.as_str() == claims.event_name),
            ),
            (
                "workflow_ref",
                claims.workflow_ref == format!("{}/{}@{git_ref}", self.repository, self.workflow),
            ),
            (
                "job_workflow_sha",
                claims.job_workflow_sha == self.builder.sha,
            ),
            (
                "job_workflow_ref",
                claims.job_workflow_ref == self.builder.r#ref,
            ),
            (
                "runner_environment",
                claims.runner_environment == self.runner.as_str(),
            ),
        ])
    }
}

/// A signed rule a GitHub token's claims are checked against, and what a Machine's record
/// keeps of it.
pub trait ClaimRule: Sync {
    fn identity(&self) -> RuleIdentity<'_>;

    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first claim that does not match.
    fn verify_claims(&self, claims: &GithubClaims) -> Result<(), AuthorityRefusal>;
}

impl ClaimRule for AutoDeployRule {
    fn identity(&self) -> RuleIdentity<'_> {
        RuleIdentity {
            rule: self.rule,
            generation: self.generation,
            repository_id: self.source.repository_id,
            branch: &self.source.branch,
            namespace: &self.namespace,
        }
    }

    fn verify_claims(&self, claims: &GithubClaims) -> Result<(), AuthorityRefusal> {
        Self::verify_claims(self, claims)
    }
}

/// What a Machine records of a rule the first time it sees it, which no later generation
/// may change, and the generation in hand.
#[derive(Clone, Copy, Debug)]
pub struct RuleIdentity<'a> {
    pub rule: RuleId,
    pub generation: u64,
    pub repository_id: u64,
    pub branch: &'a str,
    pub namespace: &'a Namespace,
}

/// A rule statement, which names the Device that signed it.
pub trait RuleStatement: Statement {
    fn signer(&self) -> SignerKey;
}

impl RuleStatement for AutoDeployRule {
    fn signer(&self) -> SignerKey {
        self.signer
    }
}

/// Open a rule signed by a Device this Machine trusts, or answered by a trusted passkey's
/// own prompt, as its body says. A browser key is refused even under a valid session
/// grant: a rule stands for every future push.
///
/// # Errors
/// Returns the envelope's refusal from [`Signed::approvers`], a malformed body, and
/// [`AuthorityRefusal::Mismatch`] naming the signer when no Device or passkey signed as
/// the body says.
pub fn verify_signature<T: RuleStatement>(
    rule: &Signed<T>,
    trusted: &TrustedKeys,
    now: Timestamp,
) -> Result<(T, Approver), AuthorityRefusal> {
    let approvers = rule.approvers(trusted, now)?;
    let body = rule.open()?;
    approvers
        .into_iter()
        .find(|approver| {
            !matches!(approver, Approver::Browser { .. }) && approver.signs_as(body.signer())
        })
        .map(|approver| (body, approver))
        .ok_or(AuthorityRefusal::Mismatch {
            field: "the signer",
        })
}

/// Which authority a build claims for its commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CommitAuthority {
    /// The commit is the branch head the Machine reads from GitHub itself.
    Head,
    /// A person reviewed the commit for a preview: the review with this digest names it.
    Review(ReviewDigest),
    /// A person approved the commit, which is not the branch head: the
    /// [`CommitApproval`] with this digest names it.
    Approved(ApprovalDigest),
}

/// A person's approval of one commit of one Service under a rule, where the branch head
/// otherwise would be the authority, such as a rollback. Under it the grant Machine
/// builds the commit once GitHub says the branch holds it, and a Machine deploys the
/// commit's image with the rule's settings. Every other check stays the Machine's.
///
/// The same keys may sign it as a deploy approval: a Device, or a browser key under a
/// session grant for the rule's Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CommitApproval {
    pub cluster: SealingPublicKey,
    pub rule: RuleId,
    pub generation: u64,
    pub service: ServiceName,
    pub commit: CommitSha,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub signer: SignerKey,
}

impl Statement for CommitApproval {
    const PURPOSE: Purpose = Purpose::Workflow;
}

impl ApprovalDigest {
    /// The digest of `approval` as it was signed, without the signatures or grants beside
    /// it: what the person approved, not who carried it.
    #[must_use]
    pub fn of(approval: &Signed<CommitApproval>) -> Self {
        Self(Sha256::digest(approval.signing_input()).into())
    }
}

/// What a Machine knows when it checks a commit approval: its own keys and clock.
#[derive(Clone, Copy, Debug)]
pub struct ApprovalFacts<'a> {
    /// The Sealing public key this Machine holds.
    pub cluster: SealingPublicKey,
    pub machine: &'a MachineId,
    pub trusted: &'a TrustedKeys,
    pub now: Timestamp,
}

/// A commit approval [`verify_commit_approval`] found live, signed by a key this Machine
/// trusts, for the rule and generation in hand. Nothing else makes one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedCommitApproval {
    body: CommitApproval,
    digest: ApprovalDigest,
    by: Approver,
}

impl CheckedCommitApproval {
    #[must_use]
    pub const fn body(&self) -> &CommitApproval {
        &self.body
    }

    #[must_use]
    pub const fn digest(&self) -> ApprovalDigest {
        self.digest
    }

    #[must_use]
    pub const fn by(&self) -> &Approver {
        &self.by
    }

    /// Whether this approves building or deploying `commit` of `service` under `rule`.
    #[must_use]
    pub fn names(&self, rule: &AutoDeployRule, service: &ServiceName, commit: CommitSha) -> bool {
        self.body.rule == rule.rule
            && self.body.generation == rule.generation
            && &self.body.service == service
            && self.body.commit == commit
    }
}

/// Check `approval` is live now, signed by a Device this Machine trusts or by a browser
/// key whose session grant covers deploys of `rule`'s Environment on this Machine, and
/// names this Cluster, `rule`'s ID and generation. A rule that ended admits none.
///
/// # Errors
/// Returns the envelope's refusal from [`Signed::approvers`], a malformed body,
/// [`AuthorityRefusal::Mismatch`] naming the first fact that does not hold or a session
/// grant that does not cover the deploy, and [`AuthorityRefusal::Expired`] outside its
/// window.
pub fn verify_commit_approval(
    approval: &Signed<CommitApproval>,
    rule: &AutoDeployRule,
    facts: &ApprovalFacts<'_>,
) -> Result<CheckedCommitApproval, AuthorityRefusal> {
    let approvers = approval.approvers(facts.trusted, facts.now)?;
    let body = approval.open()?;
    let by = approvers
        .into_iter()
        .find(|approver| approver.signs_as(body.signer))
        .ok_or(AuthorityRefusal::Mismatch {
            field: "the signer",
        })?;
    first_mismatch([
        (
            field::CLUSTER,
            body.cluster == facts.cluster && rule.cluster == facts.cluster,
        ),
        ("a rule", body.rule == rule.rule),
        ("a generation", body.generation == rule.generation),
        ("an ended rule", !rule.ended),
    ])?;
    if let Approver::Browser { grant, .. } = &by {
        grant.body.covers(&GrantScope {
            cluster: facts.cluster,
            environment: &rule.namespace,
            machines: std::slice::from_ref(facts.machine),
            family: ActionFamily::Deploy,
            issued_at: body.issued_at,
            expires_at: body.expires_at,
        })?;
    }
    super::validity::verify(body.issued_at, body.expires_at, facts.now)?;
    Ok(CheckedCommitApproval {
        digest: ApprovalDigest::of(approval),
        body,
        by,
    })
}

/// Why this Machine may run a claim's commit now. Both are facts the Machine established
/// itself.
#[derive(Clone, Copy, Debug)]
pub enum CommitNow<'a> {
    /// The branch head this Machine read from GitHub just now.
    Head(CommitSha),
    /// A person's live approval this Machine checked, in place of the head.
    Approved(&'a CheckedCommitApproval),
}

/// Which token of a build this is: the one that asks for the build secrets, or the one
/// that reports the image.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildStage {
    /// Mint a Build Grant for `holder`, and seal the build secrets to `recipient`.
    Grant {
        holder: BuildGrantId,
        recipient: RecipientKey,
    },
    /// `image` was built, with its secrets sealed from `attest`.
    Built {
        image: ImageDigest,
        attest: AttestKey,
    },
}

/// What a pinned builder attests through a token's audience. Nothing in it is secret: a
/// Machine recomputes it from facts it holds and compares the audience.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BuildClaim {
    pub cluster: SealingPublicKey,
    pub rule: RuleId,
    pub generation: u64,
    pub service: ServiceName,
    pub policy: PolicyDigest,
    /// The commit the builder checked out.
    pub commit: CommitSha,
    pub authority: CommitAuthority,
    pub stage: BuildStage,
}

impl BuildClaim {
    /// `ployz-build:` and the lowercase hex SHA-256 of the claim in canonical JSON.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the claim has no canonical form, such as a
    /// generation above 2^53 - 1.
    pub fn audience(&self) -> Result<String, CanonicalError> {
        let mut audience = AUDIENCE_PREFIX.to_owned();
        for byte in Sha256::digest(canonical::to_canonical(self)?) {
            write!(audience, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Ok(audience)
    }
}

/// The digest a rule approves for `spec` in `namespace`: its [`PayloadDigest`] with the
/// image and pull policy left out. The image token fills the image, and a deploy binds
/// every built image with pull policy `never`, so a Saved spec and the spec a Machine
/// receives share one template.
///
/// # Errors
/// Returns a [`CanonicalError`] when the spec has no [`PayloadDigest`] in `namespace`.
pub fn template(
    spec: &RequestedServiceSpec,
    namespace: &Namespace,
) -> Result<PayloadDigest, CanonicalError> {
    let mut spec = spec.clone();
    spec.container.image = String::new();
    spec.container.pull_policy = crate::PullPolicy::Never;
    PayloadDigest::of(&spec, namespace)
}

/// What a creation under a rule carries in place of a deploy approval: the signed rule,
/// the pinned builder's image token, and the claim whose audience it names. Cloud relays
/// all of it; a Machine trusts the claim only once the token's audience is its hash.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuleProof {
    #[ts(type = "unknown")]
    pub rule: Signed<AutoDeployRule>,
    /// The raw GitHub Actions OIDC token whose audience is `claim`'s.
    pub token: GithubToken,
    pub claim: BuildClaim,
    /// A token that may read the repository's branch head, such as an installation
    /// token narrowed to `contents: read`. `None` reads a public repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reader: Option<GithubToken>,
    /// A person's approval of the claim's commit, which replaces the branch-head read.
    /// Sent only to a daemon advertising [`crate::COMMIT_APPROVAL_CAPABILITY`]; an
    /// older one refuses the body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown", optional)]
    pub approval: Option<Signed<CommitApproval>>,
}

/// What a Machine knows when it checks a rule against a deploy request: its own facts and
/// why the claim's commit may run now.
#[derive(Clone, Copy, Debug)]
pub struct RuleFacts<'a> {
    /// The Sealing public key this Machine holds.
    pub cluster: SealingPublicKey,
    /// The attest key this Machine derives from it.
    pub attest: AttestKey,
    pub machine: &'a MachineId,
    /// The Namespace of the Container the request creates or acts on.
    pub namespace: &'a Namespace,
    /// The Service the request creates or acts on.
    pub service: &'a ServiceName,
    pub commit: CommitNow<'a>,
}

impl AutoDeployRule {
    /// Check a step on an existing Container under this rule: the rule's Cluster,
    /// Machines, Environment and Service, a live generation, and `claim` naming this
    /// rule's build of the commit that may run now: the head, built as the head, or the
    /// commit a person approved, built as the head or under an approval. `claim` must
    /// already be authentic: its audience is a GitHub token's, or this Machine accepted
    /// it before.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first fact that does not hold.
    pub fn verify_step(
        &self,
        claim: &BuildClaim,
        facts: &RuleFacts<'_>,
    ) -> Result<(), AuthorityRefusal> {
        let policy = self
            .service(facts.service)
            .map(|service| PolicyDigest::of(&service.policy));
        // A reviewed build deploys only under a preview rule, and a build of a commit a
        // person approved only under a live approval of it: the head read alone never
        // admits one.
        let now = match (facts.commit, claim.authority) {
            (CommitNow::Head(head), CommitAuthority::Head) => ("a commit", claim.commit == head),
            (
                CommitNow::Approved(approval),
                CommitAuthority::Head | CommitAuthority::Approved(_),
            ) => (
                "an approval",
                approval.names(self, &claim.service, claim.commit),
            ),
            (_, CommitAuthority::Review(_))
            | (CommitNow::Head(_), CommitAuthority::Approved(_)) => {
                return Err(AuthorityRefusal::Mismatch {
                    field: "an authority",
                });
            }
        };
        first_mismatch([
            (
                field::CLUSTER,
                self.cluster == facts.cluster && claim.cluster == facts.cluster,
            ),
            ("a Machine", self.machines.contains(facts.machine)),
            (field::ENVIRONMENT, &self.namespace == facts.namespace),
            ("an ended rule", !self.ended),
            (
                "a Service",
                policy.is_some() && &claim.service == facts.service,
            ),
            ("a rule", claim.rule == self.rule),
            ("a generation", claim.generation == self.generation),
            (
                "a policy",
                policy.is_some_and(|policy| policy.ok() == Some(claim.policy)),
            ),
            now,
        ])
    }

    /// Check a new Container under this rule: everything [`Self::verify_step`] checks,
    /// then that `claim` reports an image built with secrets sealed from this Machine's
    /// attest key, that the spec runs exactly that image, and that the spec with its
    /// image left out is the template the rule approves. A commit a session approved runs
    /// no [`host_reach`](super::deploy::refuse_host_reach). Returns the image.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first fact that does not hold.
    pub fn verify_creation(
        &self,
        claim: &BuildClaim,
        facts: &RuleFacts<'_>,
        creation: Creation<'_>,
    ) -> Result<ImageDigest, AuthorityRefusal> {
        let mismatch = |field| AuthorityRefusal::Mismatch { field };
        if creation.service != facts.service || &creation.spec.name != facts.service {
            return Err(mismatch("a Service"));
        }
        self.verify_step(claim, facts)?;
        super::deploy::refuse_preview_parts(facts.namespace, creation.spec)?;
        let (image, attest) = match claim.stage {
            BuildStage::Built { image, attest } => (image, attest),
            BuildStage::Grant { .. } => return Err(mismatch("a stage")),
        };
        if attest != facts.attest {
            return Err(mismatch("the attest key"));
        }
        let running = &creation.spec.container.image;
        let digest = running
            .rsplit_once('@')
            .map_or(running.as_str(), |(_, digest)| digest);
        if !content_addressed(running) || ImageDigest::parse(digest) != Ok(image) {
            return Err(mismatch("an image"));
        }
        if creation.kind == ContainerKind::PreDeployHook && creation.spec.pre_deploy.is_none() {
            return Err(mismatch("a hook"));
        }
        let approved = self
            .service(facts.service)
            .map(|service| service.template)
            .ok_or_else(|| mismatch("a Service"))?;
        let template = template(&creation.spec.to_requested(), facts.namespace)
            .map_err(|_| mismatch("a payload"))?;
        if template != approved {
            return Err(mismatch("a payload"));
        }
        // A session opens only its own Environment's Sealed values and reaches nothing on
        // the host, as under a deploy approval.
        if let CommitNow::Approved(approval) = facts.commit {
            if !super::deploy::sealed_scope_allows(approval.by(), creation.spec) {
                return Err(mismatch("the signer"));
            }
            if matches!(approval.by(), Approver::Browser { .. }) {
                super::deploy::refuse_host_reach(creation.spec, facts.namespace)?;
            }
        }
        Ok(image)
    }
}

/// The claims of a GitHub Actions OIDC token that a rule checks, parsed after its signature
/// verified. GitHub writes IDs as decimal strings; they are numbers here. Claims a rule
/// does not read, such as `sub`, are ignored.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct GithubClaims {
    pub iss: String,
    pub aud: String,
    pub jti: String,
    #[serde(deserialize_with = "seconds")]
    pub iat: Timestamp,
    #[serde(deserialize_with = "seconds")]
    pub exp: Timestamp,
    #[serde(deserialize_with = "seconds")]
    pub nbf: Timestamp,
    #[serde(deserialize_with = "decimal")]
    pub repository_id: u64,
    #[serde(deserialize_with = "decimal")]
    pub repository_owner_id: u64,
    pub repository: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub ref_type: String,
    pub sha: CommitSha,
    pub event_name: String,
    pub workflow_ref: String,
    pub job_workflow_ref: String,
    pub job_workflow_sha: CommitSha,
    pub runner_environment: String,
    #[serde(deserialize_with = "decimal")]
    pub run_id: u64,
    #[serde(deserialize_with = "decimal")]
    pub run_attempt: u64,
    #[serde(deserialize_with = "decimal")]
    pub actor_id: u64,
}

fn seconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
    let seconds = u64::deserialize(deserializer)?;
    seconds
        .checked_mul(1000)
        .and_then(|millis| Timestamp::from_millis(millis).ok())
        .ok_or_else(|| serde::de::Error::custom("a token time is out of range"))
}

fn decimal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let text = String::deserialize(deserializer)?;
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(serde::de::Error::custom("a GitHub ID is a decimal string"));
    }
    text.parse()
        .map_err(|_| serde::de::Error::custom("a GitHub ID is out of range"))
}

fn services_by_name<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<RuleService>, D::Error> {
    canonical::sorted_by(deserializer, |service: &RuleService| &service.name)
}

#[cfg(test)]
#[path = "rule_tests.rs"]
mod tests;

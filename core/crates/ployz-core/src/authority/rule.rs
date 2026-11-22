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
    canonical::{self, CanonicalError, Timestamp, drop_nulls},
    deploy::{Creation, PayloadDigest, placement_signed},
    keys::{Approver, Purpose, Signed, SignerKey, Statement},
    open::open_approval,
    proposal::{ProposalRef, StepBody, StepKind},
    refusal::{AuthorityRefusal, MismatchField, first_mismatch},
    scope::Ask,
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
    BuiltImageDigest, "image digest", 32, "sha256:"
);
impl BuiltImageDigest {
    /// The content `image` names, as [`ImageRef::content`](crate::ImageRef::content) reads it.
    #[must_use]
    pub fn named_by(image: &str) -> Option<Self> {
        crate::ImageRef::new(image)
            .content()
            .and_then(|digest| Self::parse(digest.as_str()).ok())
    }
}
hex_newtype!(
    /// The X25519 key a builder makes for one build, which its build secrets are sealed to.
    RecipientKey, "recipient key", 32, ""
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
pub const BUILDER_SHA: &str = "39e1f488e492c0641e520777e481e8f5b266985b";

/// The `getployz/build` commits whose workflow installs a `ployz` that no Ployz release built,
/// a lab build without the checks a rule build relies on. A Server refuses a build they ran,
/// even under a rule a person signed for one.
const RETIRED_BUILDERS: [&str; 6] = [
    "eb8e7b1daab07ba132454b385029b2ba033d3d31",
    "86765b8df6f8e87640a557d70fededbb19d2ec7d",
    "a19b4fca3adea371c25ec29f181e8c7d21037d1e",
    "a703b95af1af871dd299f300649601f047e329ad",
    "bd0e279a2753a04d3964fcb498e8c11b6f8c1e3e",
    "609eb221a49af9cefaf86bc083f57c4afaae4b7c",
];

impl Builder {
    /// Whether this is a commit Ployz retired, whose builds no Server runs.
    #[must_use]
    pub fn retired(&self) -> bool {
        RETIRED_BUILDERS.contains(&self.sha.to_string().as_str())
    }

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
/// Signed by a Device, answered by a passkey's own prompt, or a Source step of a bundle
/// either signed; a browser key under a session grant cannot sign it, because it
/// authorizes every future push.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proposal: Option<ProposalRef>,
}

impl Statement for AutoDeployRule {
    const PURPOSE: Purpose = Purpose::Rule;
    const TAKES_ASSERTION: bool = true;
}

impl StepBody for AutoDeployRule {
    fn kind(&self) -> StepKind {
        StepKind::Source
    }
    fn place(&self) -> Option<ProposalRef> {
        self.proposal
    }
    fn signer(&self) -> SignerKey {
        self.signer
    }
    fn cluster(&self) -> SealingPublicKey {
        self.cluster
    }
    fn namespace(&self) -> Option<&Namespace> {
        Some(&self.namespace)
    }
    fn ask(&self) -> Ask<'_> {
        Ask::Rule
    }
}

impl AutoDeployRule {
    /// What the rule approves for `name`, or `None` when it names no such Service.
    #[must_use]
    pub fn service(&self, name: &ServiceName) -> Option<&RuleService> {
        self.services.iter().find(|service| service.name == *name)
    }

    /// Check a verified GitHub token's claims against this rule, in the order the body
    /// runs them. The audience is the caller's: it depends on the holder, recipient or
    /// image in hand.
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
        if self.builder.retired() {
            return Err(AuthorityRefusal::RetiredBuilder {
                sha: self.builder.sha.to_string(),
            });
        }
        first_mismatch([
            (
                MismatchField::ClaimRepositoryId,
                claims.repository_id == self.repository_id,
            ),
            (
                MismatchField::ClaimRepositoryOwnerId,
                claims.repository_owner_id == self.owner_id,
            ),
            (MismatchField::ClaimRefType, claims.ref_type == "branch"),
            (MismatchField::ClaimRef, claims.git_ref == git_ref),
            (
                MismatchField::ClaimEventName,
                self.events
                    .iter()
                    .any(|event| event.as_str() == claims.event_name),
            ),
            (
                MismatchField::ClaimWorkflowRef,
                claims.workflow_ref == format!("{}/{}@{git_ref}", self.repository, self.workflow),
            ),
            (
                MismatchField::ClaimJobWorkflowSha,
                claims.job_workflow_sha == self.builder.sha,
            ),
            (
                MismatchField::ClaimJobWorkflowRef,
                claims.job_workflow_ref == self.builder.r#ref,
            ),
            (
                MismatchField::ClaimRunnerEnvironment,
                claims.runner_environment == self.runner.as_str(),
            ),
        ])
    }
}

impl AutoDeployRule {
    /// What a Machine's record keeps of this rule.
    #[must_use]
    pub fn identity(&self) -> RuleIdentity<'_> {
        RuleIdentity {
            rule: self.rule,
            generation: self.generation,
            repository_id: self.source.repository_id,
            branch: &self.source.branch,
            namespace: &self.namespace,
        }
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

/// Open a rule for this Machine's Cluster `cluster`: signed by a Device it trusts,
/// answered by a trusted passkey's own prompt, or a Source step of a bundle either signed.
/// It names this Cluster before anything records a mark of it.
///
/// # Errors
/// Returns the refusal of [`open_approval`], and [`AuthorityRefusal::Mismatch`] naming
/// the Cluster for a rule of another.
pub fn verify_signature(
    rule: &Signed<AutoDeployRule>,
    trusted: &TrustedKeys,
    cluster: Option<SealingPublicKey>,
) -> Result<(AutoDeployRule, Approver), AuthorityRefusal> {
    let (body, by, cluster) = open_approval(rule, trusted, cluster)?;
    first_mismatch([(MismatchField::Cluster, body.cluster == cluster)])?;
    Ok((body, by))
}

/// Which authority a build claims for its commit. Written into every claim, so the
/// pinned builder's audience keeps its bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CommitAuthority {
    /// The commit is the branch head the Machine reads from GitHub itself.
    Head,
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
        image: BuiltImageDigest,
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
/// image left out. The image token fills the image, so a Saved spec and the spec a Machine
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
    PayloadDigest::of(&spec, namespace)
}

/// What a creation under a rule carries in place of a signed version: the signed rule,
/// the evidence that its build is authentic, and the claim that evidence names. Cloud
/// relays all of it; a Machine trusts the claim only once the evidence checks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuleProof {
    pub rule: Signed<AutoDeployRule>,
    pub evidence: RuleEvidence,
    pub claim: BuildClaim,
    /// A token that may read the repository's branch head, such as an installation
    /// token narrowed to `contents: read`. `None` reads a public repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reader: Option<GithubToken>,
}

/// Why a Machine may take a rule's claim as a build the rule covers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RuleEvidence {
    /// The pinned builder's raw GitHub Actions OIDC token, whose audience is the claim's.
    /// The Machine also reads the branch head, which must still be the claim's commit.
    Online(GithubToken),
    /// A verified build: what a Machine of this Cluster answered once that online check
    /// passed. It stands for any commit, the head or not, while the rule's signer is
    /// trusted and its generation is the newest a Machine has seen.
    Verified(VerifiedBuild),
}

hex_newtype!(
    /// HMAC-SHA256, under a key every Machine of the Cluster derives from its Sealing Key
    /// and nothing else can, over [`VerifiedBuild::input`]. Only a Machine whose online
    /// check of a rule's build passed makes one.
    VerifiedBuild, "verified build", 32, "", "64 lowercase hex digits"
);

impl VerifiedBuild {
    /// The bytes a verified build is a MAC of: a fixed prefix, the SHA-256 of the signed
    /// rule's body in hex, a newline, and the claim in canonical JSON. So one build of one
    /// commit of one Service, under one approval of one rule in one Cluster.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the claim has no canonical form.
    pub fn input(
        rule: &Signed<AutoDeployRule>,
        claim: &BuildClaim,
    ) -> Result<Vec<u8>, CanonicalError> {
        let mut input = crate::separation::VERIFIED_BUILD.to_vec();
        let mut body = String::with_capacity(64);
        for byte in Sha256::digest(rule.body.as_bytes()) {
            write!(body, "{byte:02x}").expect("writing to a String cannot fail");
        }
        input.extend(body.as_bytes());
        input.push(b'\n');
        input.extend(canonical::to_canonical(claim)?.as_bytes());
        Ok(input)
    }
}

/// What a Machine knows when it checks a rule against a deploy request: its own facts and,
/// for a build checked online, the branch head it read from GitHub just now.
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
    /// The branch head, read online; `None` for a verified build, which needs none.
    pub head: Option<CommitSha>,
}

impl AutoDeployRule {
    /// Check a new Container under this rule: the rule's Cluster, Environment and Service,
    /// a live generation, a builder Ployz still trusts, `claim` naming this rule's build of
    /// the head when `facts` read one, an image built with secrets sealed from this
    /// Machine's attest key, a spec that runs exactly that image, and that the spec with
    /// its image left out is the template the rule approves. A spec whose placement is
    /// signed, because it reads data a Server keeps or reaches the host, also needs this
    /// Machine among the rule's. `claim` must already be authentic: a GitHub token names
    /// its audience, or a verified build holds for it. Returns the image.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::RetiredBuilder`] for a retired builder, and
    /// [`AuthorityRefusal::Mismatch`] naming the first other fact that does not hold.
    pub fn verify_creation(
        &self,
        claim: &BuildClaim,
        facts: &RuleFacts<'_>,
        creation: Creation<'_>,
    ) -> Result<BuiltImageDigest, AuthorityRefusal> {
        let mismatch = |field| AuthorityRefusal::Mismatch { field };
        if creation.service != facts.service || &creation.spec.name != facts.service {
            return Err(mismatch(MismatchField::Service));
        }
        if self.source.builder.retired() {
            return Err(AuthorityRefusal::RetiredBuilder {
                sha: self.source.builder.sha.to_string(),
            });
        }
        let requested = creation.spec.to_requested();
        let policy = self
            .service(facts.service)
            .map(|service| PolicyDigest::of(&service.policy));
        first_mismatch([
            (
                MismatchField::Cluster,
                self.cluster == facts.cluster && claim.cluster == facts.cluster,
            ),
            (
                MismatchField::Machine,
                !placement_signed(&requested) || self.machines.contains(facts.machine),
            ),
            (
                MismatchField::Environment,
                &self.namespace == facts.namespace,
            ),
            (MismatchField::EndedRule, !self.ended),
            (
                MismatchField::Service,
                policy.is_some() && &claim.service == facts.service,
            ),
            (MismatchField::Rule, claim.rule == self.rule),
            (
                MismatchField::Generation,
                claim.generation == self.generation,
            ),
            (
                MismatchField::Policy,
                policy.is_some_and(|policy| policy.ok() == Some(claim.policy)),
            ),
            (
                MismatchField::Commit,
                facts.head.is_none_or(|head| claim.commit == head),
            ),
        ])?;
        let (image, attest) = match claim.stage {
            BuildStage::Built { image, attest } => (image, attest),
            BuildStage::Grant { .. } => return Err(mismatch(MismatchField::Stage)),
        };
        if attest != facts.attest {
            return Err(mismatch(MismatchField::AttestKey));
        }
        if BuiltImageDigest::named_by(&creation.spec.container.image) != Some(image) {
            return Err(mismatch(MismatchField::Image));
        }
        if creation.kind == ContainerKind::PreDeployHook && creation.spec.pre_deploy.is_none() {
            return Err(mismatch(MismatchField::Hook));
        }
        if !creation.spec.volume_graph().born_in(facts.namespace) {
            return Err(mismatch(MismatchField::VolumesEnvironment));
        }
        let approved = self
            .service(facts.service)
            .map(|service| service.template)
            .ok_or_else(|| mismatch(MismatchField::Service))?;
        let template =
            template(&requested, facts.namespace).map_err(|_| mismatch(MismatchField::Payload))?;
        if template != approved {
            return Err(mismatch(MismatchField::Payload));
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

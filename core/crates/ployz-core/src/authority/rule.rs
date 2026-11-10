//! The automatic deploy rule a person signs once, the build claim a pinned GitHub builder
//! names in the audience of each token it asks GitHub for, and the token claims a Machine
//! checks against the rule.
//!
//! A rule names the repository by ID, the branch, the Environment, the builder workflow
//! pinned by commit and, per Service, the settings and build policy it approves. A Machine
//! verifies a GitHub token's signature itself, then checks every claim here against the
//! signed rule. Nothing here verifies a JWT or touches a key, so it builds for the browser.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use super::{
    canonical::{self, CanonicalError, Timestamp, hex_bytes, write_hex},
    deploy::{Creation, PayloadDigest, content_addressed, drop_nulls},
    keys::{Approver, Purpose, Signed, SigningPublicKey, Statement},
    refusal::AuthorityRefusal,
    restart::SignerKey,
    trusted_key::TrustedKeys,
};
use crate::{
    BuildGrantId, ContainerKind, GithubToken, MachineId, Namespace, RequestedServiceSpec,
    SealedRecord, SealingPublicKey, ServiceName,
};

/// What prefixes every build audience, so a token minted for a Ployz build names no other
/// relying party's audience.
pub const AUDIENCE_PREFIX: &str = "ployz-build:";

macro_rules! hex_newtype {
    ($(#[$attribute:meta])* $name:ident, $label:literal, $len:literal, $prefix:literal) => {
        $(#[$attribute])*
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
        #[serde(try_from = "String", into = "String")]
        #[ts(type = "string")]
        pub struct $name([u8; $len]);

        impl $name {
            #[must_use]
            pub const fn from_bytes(bytes: [u8; $len]) -> Self {
                Self(bytes)
            }

            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            /// # Errors
            /// Returns a [`crate::ValueError`] unless `value` is the prefix, if any, and
            /// then lowercase hex of the right length.
            pub fn parse(value: &str) -> Result<Self, crate::ValueError> {
                value
                    .strip_prefix($prefix)
                    .and_then(hex_bytes)
                    .map(Self)
                    .ok_or_else(|| {
                        crate::ValueError::new(
                            $label,
                            value,
                            concat!($prefix, "lowercase hex of ", stringify!($len), " bytes"),
                        )
                    })
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str($prefix)?;
                write_hex(&self.0, formatter)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!(stringify!($name), "({})"), self)
            }
        }

        impl TryFrom<String> for $name {
            type Error = crate::ValueError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(&value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.to_string()
            }
        }
    };
}

hex_newtype!(
    /// One rule across all its generations: 128 random bits the approver's client makes,
    /// so Cloud cannot pick one that collides with another rule's.
    RuleId, "rule ID", 16, ""
);
hex_newtype!(
    /// A Git commit as GitHub names it: 40 lowercase hex digits.
    CommitSha, "commit", 20, ""
);
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
pub struct Builder {
    /// `job_workflow_ref` as GitHub writes it:
    /// `<owner>/<repository>/<path>@<ref>`.
    pub r#ref: String,
    pub sha: CommitSha,
}

/// The commit of `getployz/build` whose reusable workflow a rule approved from the CLI
/// pins by default.
pub const BUILDER_SHA: &str = "5f35eaa8e1372d0d86ff82788adadf308e5f3f97";

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
    #[serde(deserialize_with = "sorted_distinct")]
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
                    .unwrap_or_else(|| "Dockerfile".to_owned()),
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
/// Signed by a Device; a browser key under a session grant cannot sign it, because it
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
    #[serde(deserialize_with = "sorted_distinct")]
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
        let source = &self.source;
        let git_ref = format!("refs/heads/{}", source.branch);
        let checks = [
            (
                "repository_id",
                claims.repository_id == source.repository_id,
            ),
            (
                "repository_owner_id",
                claims.repository_owner_id == source.owner_id,
            ),
            ("ref_type", claims.ref_type == "branch"),
            ("ref", claims.git_ref == git_ref),
            (
                "event_name",
                source
                    .events
                    .iter()
                    .any(|event| event.as_str() == claims.event_name),
            ),
            (
                "workflow_ref",
                claims.workflow_ref
                    == format!("{}/{}@{git_ref}", source.repository, source.workflow),
            ),
            (
                "job_workflow_sha",
                claims.job_workflow_sha == source.builder.sha,
            ),
            (
                "job_workflow_ref",
                claims.job_workflow_ref == source.builder.r#ref,
            ),
            (
                "runner_environment",
                claims.runner_environment == source.runner.as_str(),
            ),
        ];
        match checks.into_iter().find(|(_, holds)| !holds) {
            Some((field, _)) => Err(AuthorityRefusal::Mismatch { field }),
            None => Ok(()),
        }
    }
}

/// Open a rule signed by a Device this Machine trusts, the one its body names. A browser
/// key is refused even under a valid session grant.
///
/// # Errors
/// Returns the envelope's refusal from [`Signed::approvers`], a malformed body, and
/// [`AuthorityRefusal::Mismatch`] naming the signer when no trusted Device signed as the
/// body says.
pub fn verify_signature(
    rule: &Signed<AutoDeployRule>,
    trusted: &TrustedKeys,
    now: Timestamp,
) -> Result<(AutoDeployRule, SigningPublicKey), AuthorityRefusal> {
    let approvers = rule.approvers(trusted, now)?;
    let body = rule.open()?;
    approvers
        .into_iter()
        .find_map(|approver| match approver {
            Approver::Device(key) if approver.signs_as(body.signer) => Some(key),
            Approver::Device(_) | Approver::Browser { .. } => None,
        })
        .map(|key| (body, key))
        .ok_or(AuthorityRefusal::Mismatch {
            field: "the signer",
        })
}

/// Which authority a build claims for its commit. Only `Head` exists so far: the commit is
/// the branch head the Machine reads from GitHub itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CommitAuthority {
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
}

/// What a Machine knows when it checks a rule against a deploy request: its own facts and
/// the branch head it read from GitHub just now.
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
    pub head: CommitSha,
}

impl AutoDeployRule {
    /// Check a step on an existing Container under this rule: the rule's Cluster,
    /// Machines, Environment and Service, a live generation, and `claim` naming this
    /// rule's build of the head. `claim` must already be authentic: its audience is a
    /// GitHub token's, or this Machine accepted it before.
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
        let checks = [
            (
                "a Cluster",
                self.cluster == facts.cluster && claim.cluster == facts.cluster,
            ),
            ("a Machine", self.machines.contains(facts.machine)),
            ("an Environment", &self.namespace == facts.namespace),
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
            ("a commit", claim.commit == facts.head),
        ];
        // Only a build of the head reaches a creation through a rule. D6's approved
        // builds deploy through a person's deploy approval instead.
        match claim.authority {
            CommitAuthority::Head => {}
        }
        match checks.into_iter().find(|(_, holds)| !holds) {
            Some((field, _)) => Err(AuthorityRefusal::Mismatch { field }),
            None => Ok(()),
        }
    }

    /// Check a new Container under this rule: everything [`Self::verify_step`] checks,
    /// then that `claim` reports an image built with secrets sealed from this Machine's
    /// attest key, that the spec runs exactly that image, and that the spec with its
    /// image left out is the template the rule approves. Returns the image.
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

fn sorted_distinct<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Ord,
{
    let items = Vec::<T>::deserialize(deserializer)?;
    if !items.is_sorted_by(|left, right| left < right) {
        return Err(serde::de::Error::custom(
            "a list must be sorted and distinct",
        ));
    }
    Ok(items)
}

fn services_by_name<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<RuleService>, D::Error> {
    let services = Vec::<RuleService>::deserialize(deserializer)?;
    if !services.is_sorted_by(|left, right| left.name < right.name) {
        return Err(serde::de::Error::custom(
            "Services must be sorted by name and distinct",
        ));
    }
    Ok(services)
}

#[cfg(test)]
#[path = "rule_tests.rs"]
mod tests;

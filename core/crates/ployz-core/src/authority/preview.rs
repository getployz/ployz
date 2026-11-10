//! Previews under a preview rule: the rule a person signs once for an Environment's pull
//! request previews, the review a person signs for one untrusted commit, and the checks a
//! Machine runs on a preview's build and Containers.
//!
//! A Machine derives each preview's Namespace itself, from the rule's ID and a preview key
//! it verified: the branch GitHub names in a trusted push's token, or the key a signed
//! review names. Only then does it hash the spec, with that Namespace and the Generated
//! Values it checked replaced by placeholders, against what the person signed. A
//! Cloud-chosen production Namespace or hostname anywhere in the spec stays literal and
//! fails the template. Nothing here verifies a JWT or touches a key.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use ts_rs::TS;
use uuid::Uuid;

use super::{
    canonical::{self, Timestamp},
    deploy::{Creation, PayloadDigest, content_addressed},
    keys::{Approver, Purpose, Signed, SigningPublicKey, Statement, signing_input},
    refusal::AuthorityRefusal,
    restart::SignerKey,
    rule::{
        AttestKey, BuildClaim, BuildPolicy, BuildStage, ClaimRule, CommitAuthority, CommitSha,
        GithubClaims, ImageDigest, PolicyDigest, ReviewDigest, RuleId, RuleIdentity, RuleSource,
        RuleStatement, SealedDigest,
    },
    session::{ActionFamily, GrantScope},
    trusted_key::{PasskeyAssertion, TrustedKeys},
};
use crate::{
    ContainerKind, DeviceCode, EnvValue, MachineId, Namespace, RequestedServiceSpec,
    SealingPublicKey, ServiceName, config::Generator,
};

/// What every occurrence of a preview's derived Namespace becomes before its spec is
/// hashed. Braces never appear in a Namespace or a hostname.
pub const NAMESPACE_PLACEHOLDER: &str = "{preview-namespace}";

/// Which preview of a rule this is: a branch of the repository, or a fork's pull request.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub enum PreviewKey {
    Branch(String),
    Fork(u64),
}

impl PreviewKey {
    /// # Errors
    /// Returns a [`crate::ValueError`] unless `value` is `branch:<name>` or
    /// `fork:<pull request number>`.
    pub fn parse(value: &str) -> Result<Self, crate::ValueError> {
        let invalid = || {
            crate::ValueError::new(
                "preview key",
                value,
                "branch:<name> or fork:<pull request number>",
            )
        };
        if let Some(branch) = value.strip_prefix("branch:") {
            return (!branch.is_empty())
                .then(|| Self::Branch(branch.to_owned()))
                .ok_or_else(invalid);
        }
        value
            .strip_prefix("fork:")
            .filter(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|number| number.parse().ok())
            .map(Self::Fork)
            .ok_or_else(invalid)
    }
}

impl fmt::Display for PreviewKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Branch(branch) => write!(formatter, "branch:{branch}"),
            Self::Fork(number) => write!(formatter, "fork:{number}"),
        }
    }
}

impl TryFrom<String> for PreviewKey {
    type Error = crate::ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<PreviewKey> for String {
    fn from(value: PreviewKey) -> Self {
        value.to_string()
    }
}

/// The Namespace a preview of `rule` runs in: `pv-`, the first 16 hex digits of the rule
/// ID, `-`, and the first 12 hex digits of SHA-256 of the preview key. The rule ID is
/// random from the approver's client, so Cloud cannot pick one that collides with another
/// rule's previews.
#[must_use]
pub fn preview_namespace(rule: RuleId, key: &PreviewKey) -> Namespace {
    let rule = rule.to_string();
    let key: String = Sha256::digest(key.to_string().as_bytes())
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Namespace::parse(format!("pv-{}-{key}", rule.get(..16).unwrap_or_default()))
        .expect("a preview Namespace is a DNS label")
}

/// Whether `namespace` has the reserved preview form, `pv-<16 hex>-<12 hex>`. Only a
/// creation under the preview rule it derives from may land in one.
#[must_use]
pub fn is_preview_namespace(namespace: &Namespace) -> bool {
    let hex = |part: &str, len| {
        part.len() == len
            && part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    namespace
        .as_str()
        .strip_prefix("pv-")
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(rule, key)| hex(rule, 16) && hex(key, 12))
}

/// A preview's generated domain prefix: `<label>-<namespace>`, the label cut so the
/// prefix stays one DNS label. Its hostname is `<prefix>.<domain>`, under the same
/// Cluster Domain wildcard as every generated domain, and holds the derived Namespace,
/// which the template puts back.
#[must_use]
pub fn preview_prefix(label: &str, namespace: &Namespace) -> String {
    let room = 63usize.saturating_sub(namespace.as_str().len() + 1);
    let label = label.get(..room).unwrap_or(label).trim_end_matches('-');
    format!("{label}-{namespace}")
}

/// One Service a preview rule deploys in each preview.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PreviewService {
    pub name: ServiceName,
    /// [`template`] of the preview's spec without its Setup Commands.
    pub template: PayloadDigest,
    /// [`template`] of the same spec with them, which only a preview's first creation of
    /// this Service may carry. `None` when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub setup: Option<PayloadDigest>,
    pub policy: BuildPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub secrets: Option<SealedDigest>,
    /// The parent's Sealed keys this Service may open in a preview.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    #[ts(optional, as = "Option<BTreeSet<String>>")]
    pub inherit: BTreeSet<String>,
    /// Its Generated Values by key, each a fresh random value per preview.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(optional, as = "Option<BTreeMap<String, Generator>>")]
    pub generated: BTreeMap<String, Generator>,
}

impl PreviewService {
    /// Whether a preview of this Service opens any secret: a parent value or a build
    /// secret. A session may not review such a preview (operator decision 6).
    #[must_use]
    pub fn holds_secrets(&self) -> bool {
        !self.inherit.is_empty() || self.secrets.is_some()
    }
}

/// A person's standing approval of previews of one Environment: pushes by trusted GitHub
/// accounts to the repository's other branches deploy with no review, into a Namespace
/// and hostnames each Machine derives. Signed by a Device, like an automatic deploy rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PreviewRule {
    pub cluster: SealingPublicKey,
    pub rule: RuleId,
    pub generation: u64,
    /// Sorted and distinct.
    #[serde(deserialize_with = "sorted_distinct")]
    pub machines: Vec<MachineId>,
    /// The Namespace of the Environment previews start from.
    pub parent: Namespace,
    /// That Environment's Cloud ID, the one its Sealed values are sealed for.
    #[ts(type = "string")]
    pub parent_id: Uuid,
    /// Previews' hostnames live under it.
    pub domain: String,
    /// The parent's source: its branch is where reviewed builds run.
    pub source: RuleSource,
    /// GitHub account IDs whose pushes deploy a preview with no review. Sorted and
    /// distinct.
    #[serde(deserialize_with = "sorted_distinct")]
    pub trusted: Vec<u64>,
    /// Sorted by name and distinct.
    #[serde(deserialize_with = "services_by_name")]
    pub services: Vec<PreviewService>,
    pub issued_at: Timestamp,
    pub ended: bool,
    pub signer: SignerKey,
}

impl Statement for PreviewRule {
    const PURPOSE: Purpose = Purpose::Rule;
}

impl RuleStatement for PreviewRule {
    fn signer(&self) -> SignerKey {
        self.signer
    }
}

impl ClaimRule for PreviewRule {
    fn identity(&self) -> RuleIdentity<'_> {
        RuleIdentity {
            rule: self.rule,
            generation: self.generation,
            repository_id: self.source.repository_id,
            branch: &self.source.branch,
            namespace: &self.parent,
        }
    }

    /// Every claim an automatic deploy rule checks, at whichever branch the token names:
    /// which branch may build a preview depends on the build's authority.
    fn verify_claims(&self, claims: &GithubClaims) -> Result<(), AuthorityRefusal> {
        if !claims.git_ref.starts_with("refs/heads/") {
            return Err(AuthorityRefusal::Mismatch { field: "ref" });
        }
        self.source.verify_claims(claims, &claims.git_ref)
    }
}

impl PreviewRule {
    #[must_use]
    pub fn service(&self, name: &ServiceName) -> Option<&PreviewService> {
        self.services.iter().find(|service| service.name == *name)
    }

    /// Which preview a build of `commit` under `authority` is, from the token's verified
    /// `claims`. A trusted push is a push by an account in `trusted` to a branch other
    /// than the parent's, of the commit GitHub ran for; its head is the caller's to read.
    /// A reviewed build is a dispatch on the parent's branch of the commit the review
    /// names.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first fact that does not hold.
    pub fn preview_key(
        &self,
        claims: &GithubClaims,
        commit: CommitSha,
        authority: CommitAuthority,
        review: Option<&CheckedReview>,
    ) -> Result<PreviewKey, AuthorityRefusal> {
        let mismatch = |field| Err(AuthorityRefusal::Mismatch { field });
        let parent = format!("refs/heads/{}", self.source.branch);
        match (authority, review) {
            (CommitAuthority::Head, None) => {
                let Some(branch) = claims.git_ref.strip_prefix("refs/heads/") else {
                    return mismatch("ref");
                };
                if claims.git_ref == parent {
                    return mismatch("a branch");
                }
                if claims.event_name != "push" {
                    return mismatch("event_name");
                }
                if !self.trusted.contains(&claims.actor_id) {
                    return mismatch("an account");
                }
                if commit != claims.sha {
                    return mismatch("a commit");
                }
                Ok(PreviewKey::Branch(branch.to_owned()))
            }
            (CommitAuthority::Review(digest), Some(review)) => {
                if digest != review.digest {
                    return mismatch("a review");
                }
                if claims.git_ref != parent {
                    return mismatch("ref");
                }
                if claims.event_name != "workflow_dispatch" {
                    return mismatch("event_name");
                }
                if commit != review.body.commit {
                    return mismatch("a commit");
                }
                Ok(review.body.key.clone())
            }
            (CommitAuthority::Head, Some(_)) | (CommitAuthority::Review(_), None) => {
                mismatch("a review")
            }
        }
    }
}

/// What a Machine knows when it checks a preview's step or creation: its own facts, the
/// preview it derived from verified claims, and the branch head it read just now.
#[derive(Clone, Copy, Debug)]
pub struct PreviewFacts<'a> {
    pub cluster: SealingPublicKey,
    pub attest: AttestKey,
    pub machine: &'a MachineId,
    /// The Namespace of the Container the request creates or acts on.
    pub namespace: &'a Namespace,
    pub service: &'a ServiceName,
    pub key: &'a PreviewKey,
    /// The head of the preview's branch, read from GitHub now. `None` for a reviewed
    /// build, whose authority is the review.
    pub head: Option<CommitSha>,
}

/// A preview creation [`PreviewRule::verify_creation`] admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreviewCreation {
    pub image: ImageDigest,
    /// The creation is a Hook Container that runs the Setup Commands, which only a
    /// preview's first deploy of the Service may do: the caller checks that no Service
    /// Container of it exists here yet.
    pub runs_setup: bool,
}

impl PreviewRule {
    /// Check a step on an existing Container of a preview: the rule's Cluster, Machines
    /// and Service, a live generation, the Namespace derived from the preview key, and
    /// `claim` naming this rule's build under its authority. `claim` must be authentic.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first fact that does not hold.
    pub fn verify_step(
        &self,
        claim: &BuildClaim,
        facts: &PreviewFacts<'_>,
        review: Option<&CheckedReview>,
    ) -> Result<(), AuthorityRefusal> {
        let policy = self
            .service(facts.service)
            .map(|service| PolicyDigest::of(&service.policy));
        let authority = match (claim.authority, review, facts.head) {
            (CommitAuthority::Head, None, Some(head)) => claim.commit == head,
            (CommitAuthority::Review(digest), Some(review), _) => {
                digest == review.digest
                    && claim.commit == review.body.commit
                    && &review.body.key == facts.key
            }
            _ => false,
        };
        let checks = [
            (
                "a Cluster",
                self.cluster == facts.cluster && claim.cluster == facts.cluster,
            ),
            ("a Machine", self.machines.contains(facts.machine)),
            ("an ended rule", !self.ended),
            (
                "a Namespace",
                facts.namespace == &preview_namespace(self.rule, facts.key),
            ),
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
            ("a commit", authority),
        ];
        match checks.into_iter().find(|(_, holds)| !holds) {
            Some((field, _)) => Err(AuthorityRefusal::Mismatch { field }),
            None => Ok(()),
        }
    }

    /// Check a new Container of a preview: everything [`Self::verify_step`] checks, the
    /// image the token names built with secrets sealed from this Machine's attest key, its
    /// Sealed values, and its spec's [`template`] against what the person signed: the
    /// rule's, or for a reviewed build the review's. Returns the image, and whether the
    /// creation runs the Setup Commands.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first fact that does not hold.
    pub fn verify_creation(
        &self,
        claim: &BuildClaim,
        facts: &PreviewFacts<'_>,
        creation: Creation<'_>,
        review: Option<&CheckedReview>,
    ) -> Result<PreviewCreation, AuthorityRefusal> {
        let mismatch = |field| AuthorityRefusal::Mismatch { field };
        if creation.service != facts.service || &creation.spec.name != facts.service {
            return Err(mismatch("a Service"));
        }
        self.verify_step(claim, facts, review)?;
        let service = self
            .service(facts.service)
            .ok_or_else(|| mismatch("a Service"))?;
        let BuildStage::Built { image, attest } = claim.stage else {
            return Err(mismatch("a stage"));
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
        let spec = creation.spec.to_requested();
        self.verify_sealed(&spec, service, review)?;
        let digest = template(&spec, facts.namespace, service)?;
        let setup = match review {
            Some(review) => {
                if review.digest_for(facts.service) != Some(digest) {
                    return Err(mismatch("a payload"));
                }
                false
            }
            None if digest == service.template => false,
            None if Some(digest) == service.setup => true,
            None => return Err(mismatch("a payload")),
        };
        Ok(PreviewCreation {
            image,
            runs_setup: setup && creation.kind == ContainerKind::PreDeployHook,
        })
    }

    /// Refuse a preview's Sealed value unless it is a parent value this Service inherits,
    /// or, under a review, one sealed for the reviewed Environment. A trusted preview has
    /// no values of its own, so its identity opens nothing.
    fn verify_sealed(
        &self,
        spec: &RequestedServiceSpec,
        service: &PreviewService,
        review: Option<&CheckedReview>,
    ) -> Result<(), AuthorityRefusal> {
        let own = review.is_some_and(|review| {
            spec.container
                .sealed_identity
                .as_ref()
                .is_some_and(|identity| identity.environment == review.body.environment)
        });
        let hook = spec
            .pre_deploy
            .iter()
            .flat_map(|hook| hook.environment.values());
        let allowed = spec
            .container
            .environment
            .values()
            .chain(hook)
            .flat_map(EnvValue::sealed_parts)
            .all(|value| match value.environment {
                Some(environment) => {
                    environment == self.parent_id && service.inherit.contains(&value.key)
                }
                None => own,
            });
        if allowed {
            Ok(())
        } else {
            Err(AuthorityRefusal::Mismatch {
                field: SEALED_VALUE,
            })
        }
    }
}

impl PreviewRule {
    /// Refuse a preview build's Sealed record unless each of its values is a parent value
    /// `service` inherits, then return its [`secrets_digest`] for the caller to compare
    /// with the rule's.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the Sealed value.
    pub fn build_secrets(
        &self,
        service: &PreviewService,
        record: &crate::SealedRecord,
    ) -> Result<SealedDigest, AuthorityRefusal> {
        let refused = || AuthorityRefusal::Mismatch {
            field: SEALED_VALUE,
        };
        let inherited = record
            .values()
            .values()
            .flat_map(EnvValue::sealed_parts)
            .all(|value| {
                value.environment == Some(self.parent_id) && service.inherit.contains(&value.key)
            });
        if !inherited {
            return Err(refused());
        }
        secrets_digest(record).map_err(|_| refused())
    }
}

/// The digest a preview rule approves for a preview build's Sealed record: SHA-256 of its
/// values in canonical JSON, without the identity, which names each preview's own
/// Environment.
///
/// # Errors
/// Returns a [`super::CanonicalError`] when the values have no canonical form.
pub fn secrets_digest(record: &crate::SealedRecord) -> Result<SealedDigest, super::CanonicalError> {
    let mut value = serde_json::to_value(record.values())
        .map_err(|error| super::CanonicalError::Parse(error.to_string()))?;
    super::deploy::drop_nulls(&mut value);
    Ok(SealedDigest::from_bytes(
        Sha256::digest(canonical::to_canonical(&value)?).into(),
    ))
}

/// The field a refused Sealed value names.
pub const SEALED_VALUE: &str = "a Sealed value";

/// The digest a preview rule or review approves for `spec`, a preview's spec in its
/// derived `namespace`: its [`PayloadDigest`] with the image, pull policy, Sealed
/// identity and [`crate::config::SERVICE_ID_LABEL`] left out, each Generated Value `service` names checked and replaced by its
/// generator, and every occurrence of `namespace` replaced by [`NAMESPACE_PLACEHOLDER`].
///
/// # Errors
/// Returns [`AuthorityRefusal::Mismatch`] naming a Generated Value whose value its
/// generator could not have made, and a payload with no canonical form.
pub fn template(
    spec: &RequestedServiceSpec,
    namespace: &Namespace,
    service: &PreviewService,
) -> Result<PayloadDigest, AuthorityRefusal> {
    let mut spec = spec.clone();
    spec.container.image = String::new();
    spec.container.pull_policy = crate::PullPolicy::Never;
    spec.container.sealed_identity = None;
    // Each preview's Services are its own copies with their own IDs, which only this
    // bookkeeping label names.
    let mut labels = spec.container.labels.as_map().clone();
    labels.remove(crate::config::SERVICE_ID_LABEL);
    spec.container.labels = crate::ContainerLabels::parse(labels)
        .map_err(|_| AuthorityRefusal::Mismatch { field: "a payload" })?;
    for (key, generator) in &service.generated {
        let generated =
            |environment: &mut BTreeMap<String, EnvValue>, required: bool| match environment
                .get_mut(key)
            {
                Some(value @ EnvValue::Text(_))
                    if value.text().is_some_and(|text| generator.could_make(text)) =>
                {
                    *value = EnvValue::Text(generator.to_string());
                    Ok(())
                }
                None if !required => Ok(()),
                _ => Err(AuthorityRefusal::Mismatch {
                    field: "a Generated Value",
                }),
            };
        generated(&mut spec.container.environment, true)?;
        if let Some(hook) = &mut spec.pre_deploy {
            generated(&mut hook.environment, false)?;
        }
    }
    let payload = || AuthorityRefusal::Mismatch { field: "a payload" };
    spec.mount_graph = spec
        .mount_graph
        .scope_to_namespace(namespace)
        .map_err(|_| payload())?;
    PayloadDigest::of_scoped(&spec, |value| placeholder(value, namespace.as_str()))
        .map_err(|_| payload())
}

fn placeholder(value: &mut Value, namespace: &str) {
    match value {
        Value::String(text) => {
            if text.contains(namespace) {
                *text = text.replace(namespace, NAMESPACE_PLACEHOLDER);
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| placeholder(item, namespace)),
        Value::Object(fields) => {
            let renamed: Vec<String> = fields
                .keys()
                .filter(|key| key.contains(namespace))
                .cloned()
                .collect();
            for key in renamed {
                if let Some(field) = fields.remove(&key) {
                    fields.insert(key.replace(namespace, NAMESPACE_PLACEHOLDER), field);
                }
            }
            fields
                .values_mut()
                .for_each(|field| placeholder(field, namespace));
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// What a preview's Container change carries in place of a deploy approval: the signed
/// preview rule, the pinned builder's image token and the claim whose audience it names,
/// and for a reviewed build the review. Cloud relays all of it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PreviewProof {
    #[ts(type = "unknown")]
    pub rule: Signed<PreviewRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review: Option<Review>,
    pub token: crate::GithubToken,
    pub claim: BuildClaim,
    /// A token that may read the preview branch's head. `None` reads a public repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reader: Option<crate::GithubToken>,
}

/// One Service of a review, and the [`template`] digest of its spec as the reviewer saw
/// it, which may differ from the rule's when the preview's settings were edited.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ReviewedService {
    pub name: ServiceName,
    pub digest: PayloadDigest,
}

/// A person's review of one commit for one preview, which lets a reviewed build of
/// exactly that commit get its build secrets and deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PreviewReview {
    pub cluster: SealingPublicKey,
    pub rule: RuleId,
    pub generation: u64,
    pub key: PreviewKey,
    pub commit: CommitSha,
    /// The preview's Cloud Environment ID, which its own Sealed values are sealed for.
    #[ts(type = "string")]
    pub environment: Uuid,
    /// Sorted by name and distinct.
    #[serde(deserialize_with = "reviewed_by_name")]
    pub services: Vec<ReviewedService>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
}

impl Statement for PreviewReview {
    const PURPOSE: Purpose = Purpose::Workflow;
}

/// A review as it travels: signed by a Device or by a browser key under a session grant,
/// or answered by a passkey's own prompt over its challenge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Review {
    Signed(Signed<PreviewReview>),
    Passkey {
        body: String,
        assertion: PasskeyAssertion,
    },
}

impl Review {
    fn body(&self) -> &str {
        match self {
            Self::Signed(signed) => &signed.body,
            Self::Passkey { body, .. } => body,
        }
    }

    /// Its body, read without checking who signed it.
    #[must_use]
    pub fn opened(&self) -> Option<PreviewReview> {
        canonical::from_canonical(self.body()).ok()
    }

    /// SHA-256 of the review's signing input: what a build's audience names, and the
    /// challenge a passkey answers.
    #[must_use]
    pub fn digest(&self) -> ReviewDigest {
        review_digest(self.body())
    }
}

/// The digest of a review whose canonical body is `body`.
#[must_use]
pub fn review_digest(body: &str) -> ReviewDigest {
    ReviewDigest::from_bytes(Sha256::digest(signing_input(Purpose::Workflow, body)).into())
}

/// What a person's client fills in when it drafts a review: the Cluster its Servers
/// report, and the preview, commit and Environment it reviews.
#[derive(Clone, Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ReviewTerms {
    pub cluster: SealingPublicKey,
    pub key: PreviewKey,
    pub commit: CommitSha,
    #[ts(type = "string")]
    pub environment: Uuid,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
}

/// A review a person's client drafted: the canonical body to sign as it is, whether a
/// reviewed Service opens a secret, and one line per Service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ReviewDraft {
    pub body: String,
    /// Only a passkey's own prompt or a Device may sign it (operator decision 6).
    pub secrets: bool,
    pub lines: Vec<String>,
}

/// Why a client can't draft a review.
#[derive(Debug, thiserror::Error)]
pub enum ReviewDraftError {
    #[error("The preview rule is for another cluster. Approve it again.")]
    Cluster,
    #[error("The preview rule is off. Turn it on again to review previews.")]
    Ended,
    #[error("This preview isn't in the namespace the preview rule gives it.")]
    Namespace,
    #[error("This review names the environment previews start from, not the preview.")]
    Parent,
    #[error("The preview lists {0} twice.")]
    Duplicate(ServiceName),
    #[error("The preview rule doesn't cover {0}. Approve the preview rule again.")]
    Uncovered(ServiceName),
    #[error("{0} can't be reviewed: {1}")]
    Template(ServiceName, AuthorityRefusal),
    #[error(transparent)]
    Canonical(#[from] super::CanonicalError),
}

impl PreviewReview {
    /// The review a person's client signs for `target`, the preview's specs as a Deploy of
    /// it lowers them in `namespace`, under `rule` and `terms`. Each Service's digest is
    /// [`template`], which every Server takes again at creation, so the client signs no
    /// digest Cloud computed.
    ///
    /// # Errors
    /// Refuses a rule for another Cluster or one that is off, a `namespace` the rule does
    /// not derive for `terms.key`, a Service the rule does not cover, and a spec with no
    /// template.
    pub fn draft(
        rule: &PreviewRule,
        namespace: &Namespace,
        target: &[RequestedServiceSpec],
        terms: ReviewTerms,
    ) -> Result<ReviewDraft, ReviewDraftError> {
        if rule.cluster != terms.cluster {
            return Err(ReviewDraftError::Cluster);
        }
        if rule.ended {
            return Err(ReviewDraftError::Ended);
        }
        if *namespace != preview_namespace(rule.rule, &terms.key) {
            return Err(ReviewDraftError::Namespace);
        }
        if terms.environment == rule.parent_id {
            return Err(ReviewDraftError::Parent);
        }
        let mut specs: Vec<&RequestedServiceSpec> = target.iter().collect();
        specs.sort_by(|left, right| left.name.cmp(&right.name));
        if let Some(twice) = specs
            .iter()
            .zip(specs.iter().skip(1))
            .find_map(|(left, right)| (left.name == right.name).then_some(&left.name))
        {
            return Err(ReviewDraftError::Duplicate(twice.clone()));
        }
        let mut services = Vec::with_capacity(specs.len());
        let mut lines = Vec::with_capacity(specs.len());
        let mut secrets = false;
        for spec in specs {
            let covered = rule
                .service(&spec.name)
                .ok_or_else(|| ReviewDraftError::Uncovered(spec.name.clone()))?;
            let digest = template(spec, namespace, covered)
                .map_err(|refusal| ReviewDraftError::Template(spec.name.clone(), refusal))?;
            secrets |= covered.holds_secrets();
            lines.push(review_line(spec, covered, terms.commit, digest));
            services.push(ReviewedService {
                name: spec.name.clone(),
                digest,
            });
        }
        let body = Self {
            cluster: terms.cluster,
            rule: rule.rule,
            generation: rule.generation,
            key: terms.key,
            commit: terms.commit,
            environment: terms.environment,
            services,
            issued_at: terms.issued_at,
            expires_at: terms.expires_at,
        };
        Ok(ReviewDraft {
            body: canonical::to_canonical(&body)?,
            secrets,
            lines,
        })
    }
}

/// `Review web at 0123456789ab`, then what it reads from the parent and the preview, how
/// it departs from the preview rule, and what it asks of the host.
fn review_line(
    spec: &RequestedServiceSpec,
    service: &PreviewService,
    commit: CommitSha,
    digest: PayloadDigest,
) -> String {
    let mut commit = commit.to_string();
    commit.truncate(12);
    let mut notes: Vec<String> = Vec::new();
    if !service.inherit.is_empty() {
        let keys: Vec<&str> = service.inherit.iter().map(String::as_str).collect();
        notes.push(format!("reads {} from the parent", keys.join(", ")));
    }
    let hook = spec
        .pre_deploy
        .iter()
        .flat_map(|hook| hook.environment.values());
    let own: BTreeSet<&str> = spec
        .container
        .environment
        .values()
        .chain(hook)
        .flat_map(EnvValue::sealed_parts)
        .filter(|value| value.environment.is_none())
        .map(|value| value.key.as_str())
        .collect();
    if !own.is_empty() {
        let keys: Vec<&str> = own.into_iter().collect();
        notes.push(format!("opens its own {}", keys.join(", ")));
    }
    if Some(digest) == service.setup {
        notes.push("runs setup commands".to_owned());
    } else if digest != service.template {
        notes.push("settings differ from the preview rule".to_owned());
    }
    if service.secrets.is_some() {
        notes.push("builds with secrets".to_owned());
    }
    notes.extend(super::deploy::host_sensitive(spec));
    let mut line = format!("Review {} at {commit}", spec.name);
    if !notes.is_empty() {
        line.push_str(&format!(" ({})", notes.join(", ")));
    }
    line
}

/// [`verify_review`] for a build this Machine already admitted under `review`: every
/// check but its window, which held when the Machine first admitted the build. So a copy
/// of a reviewed preview still starts after its review expired, while a revoked key, a
/// session that couldn't sign it, or another rule or generation still fail.
///
/// # Errors
/// As [`verify_review`], but never [`AuthorityRefusal::Expired`].
pub fn verify_accepted_review(
    review: &Review,
    rule: &PreviewRule,
    trusted: &TrustedKeys,
) -> Result<CheckedReview, AuthorityRefusal> {
    let body: PreviewReview =
        canonical::from_canonical(review.body()).map_err(|error| AuthorityRefusal::Malformed {
            purpose: Purpose::Workflow,
            problem: error.to_string(),
        })?;
    verify_review(review, rule, trusted, body.issued_at)
}

/// Who a Machine found signed a review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewSigner {
    Device(SigningPublicKey),
    /// A passkey's own prompt.
    Passkey(DeviceCode),
    /// A browser key under a passkey's session grant.
    Session(SigningPublicKey),
}

impl fmt::Display for ReviewSigner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(key) => write!(formatter, "device {}", key.code()),
            Self::Passkey(code) => write!(formatter, "passkey {code}"),
            Self::Session(key) => write!(formatter, "browser key {} under a session", key.code()),
        }
    }
}

/// A review a Machine checked against a preview rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedReview {
    pub body: PreviewReview,
    pub digest: ReviewDigest,
    pub signer: ReviewSigner,
}

impl CheckedReview {
    #[must_use]
    pub fn digest_for(&self, service: &ServiceName) -> Option<PayloadDigest> {
        self.body
            .services
            .iter()
            .find(|reviewed| reviewed.name == *service)
            .map(|reviewed| reviewed.digest)
    }
}

/// Check `review` is a live review of `rule`'s current generation, signed by a trusted
/// Device, a trusted passkey's own prompt, or a browser key whose session grant covers
/// deploys in the preview's derived Namespace and was lent for the reviewed Environment.
/// A session may not review a Service that inherits parent values or has build secrets
/// (operator decision 6).
///
/// # Errors
/// Returns the envelope's refusal, a malformed body, [`AuthorityRefusal::Expired`] outside
/// its window, and [`AuthorityRefusal::Mismatch`] naming the first fact that does not hold.
pub fn verify_review(
    review: &Review,
    rule: &PreviewRule,
    trusted: &TrustedKeys,
    now: Timestamp,
) -> Result<CheckedReview, AuthorityRefusal> {
    let mismatch = |field| AuthorityRefusal::Mismatch { field };
    let body: PreviewReview =
        canonical::from_canonical(review.body()).map_err(|error| AuthorityRefusal::Malformed {
            purpose: Purpose::Workflow,
            problem: error.to_string(),
        })?;
    let digest = review.digest();
    let namespace = preview_namespace(rule.rule, &body.key);
    let signer = match review {
        Review::Passkey { assertion, .. } => {
            let passkey =
                trusted.passkey_approval(Purpose::Workflow, assertion, digest.as_bytes())?;
            ReviewSigner::Passkey(passkey.passkey.code())
        }
        Review::Signed(signed) => {
            let approvers = signed.approvers(trusted, now)?;
            let mut signer = None;
            for approver in approvers {
                match approver {
                    Approver::Device(key) => {
                        signer = Some(ReviewSigner::Device(key));
                        break;
                    }
                    Approver::Browser { key, grant } => {
                        grant.body.covers(&GrantScope {
                            cluster: rule.cluster,
                            environment: &namespace,
                            machines: &rule.machines,
                            family: ActionFamily::Deploy,
                            issued_at: body.issued_at,
                            expires_at: body.expires_at,
                        })?;
                        if grant.body.environment_id != body.environment {
                            return Err(mismatch("an Environment"));
                        }
                        signer = Some(ReviewSigner::Session(key));
                    }
                    // A passkey reviews by its own prompt, `Review::Passkey`, never inside an envelope.
                    Approver::Passkey(_) => {}
                }
            }
            signer.ok_or(mismatch("the signer"))?
        }
    };
    let checks = [
        ("a Cluster", body.cluster == rule.cluster),
        ("a rule", body.rule == rule.rule),
        ("a generation", body.generation == rule.generation),
        ("an ended rule", !rule.ended),
        // The parent's values open in a preview only by the rule's `inherit`, never as
        // the preview's own.
        ("an Environment", body.environment != rule.parent_id),
        (
            "a Service",
            body.services
                .iter()
                .all(|reviewed| rule.service(&reviewed.name).is_some()),
        ),
    ];
    if let Some((field, _)) = checks.into_iter().find(|(_, holds)| !holds) {
        return Err(mismatch(field));
    }
    super::validity::verify(body.issued_at, body.expires_at, now)?;
    if matches!(signer, ReviewSigner::Session(_))
        && body
            .services
            .iter()
            .filter_map(|reviewed| rule.service(&reviewed.name))
            .any(PreviewService::holds_secrets)
    {
        return Err(mismatch("the signer"));
    }
    Ok(CheckedReview {
        body,
        digest,
        signer,
    })
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
) -> Result<Vec<PreviewService>, D::Error> {
    let services = Vec::<PreviewService>::deserialize(deserializer)?;
    if !services.is_sorted_by(|left, right| left.name < right.name) {
        return Err(serde::de::Error::custom(
            "Services must be sorted by name and distinct",
        ));
    }
    Ok(services)
}

fn reviewed_by_name<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ReviewedService>, D::Error> {
    let services = Vec::<ReviewedService>::deserialize(deserializer)?;
    if !services.is_sorted_by(|left, right| left.name < right.name) {
        return Err(serde::de::Error::custom(
            "Services must be sorted by name and distinct",
        ));
    }
    Ok(services)
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;

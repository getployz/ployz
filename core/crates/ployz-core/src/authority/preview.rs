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

/// A preview's Ingress hostname: `<label>.<namespace>.<domain>`.
#[must_use]
pub fn preview_hostname(label: &str, namespace: &Namespace, domain: &str) -> String {
    format!("{label}.{namespace}.{domain}")
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
    /// No Service Container of this Service exists in the Namespace on this Machine, so
    /// a creation may still run the Setup Commands.
    pub first: bool,
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
    /// rule's, or for a reviewed build the review's. Returns the image.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first fact that does not hold.
    pub fn verify_creation(
        &self,
        claim: &BuildClaim,
        facts: &PreviewFacts<'_>,
        creation: Creation<'_>,
        review: Option<&CheckedReview>,
    ) -> Result<ImageDigest, AuthorityRefusal> {
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
        match review {
            Some(review) => {
                if review.digest_for(facts.service) != Some(digest) {
                    return Err(mismatch("a payload"));
                }
            }
            None if digest == service.template => {}
            None if Some(digest) == service.setup => {
                if !facts.first {
                    return Err(mismatch("a Setup Command"));
                }
            }
            None => return Err(mismatch("a payload")),
        }
        Ok(image)
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

/// The field a refused Sealed value names.
pub const SEALED_VALUE: &str = "a Sealed value";

/// The digest a preview rule or review approves for `spec`, a preview's spec in its
/// derived `namespace`: its [`PayloadDigest`] with the image, pull policy and Sealed
/// identity left out, each Generated Value `service` names checked and replaced by its
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
    #[ts(type = "unknown")]
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

//! The versions a person's passkey signs in one prompt: one step of one bundle per
//! Service, drafted where the person signs, from specs and tag pins Cloud relays.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    Against, Approved, ApprovedChange, AutoDeployRule, Before, BirthError, Births, Held,
    HeldContainer, Mounts, PlannedVolumes, RuleBasis, RuleDraft, RuleDraftError, RuleTerms,
    ServiceVersion, SignerKey, TrustedGrant, TrustedKeys, TrustedPasskey, Unknown, birth,
    canonical::{CanonicalError, Timestamp, to_canonical},
    deploy::{content_addressed, placement_signed},
    keys::Signed,
    proposal::{ProposalApproval, ProposalRef, ProposalStep, StepDigest, StepKind},
    rule::{Builder, RuleId},
    rule_draft::RuleSourceView,
};
use crate::{
    DeviceKey, MachineId, Namespace, OperationId, RequestedServiceSpec, SealingPublicKey,
    ServiceName,
};

/// What a person is asked to sign: the specs of `services` in `namespace`, each tag image
/// pinned by `pins`, under `operation`, open from `issued_at` to `expires_at`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionsAsked {
    pub cluster: SealingPublicKey,
    pub namespace: Namespace,
    /// The plan's Volumes, which the draft mints births for.
    #[serde(default)]
    pub volumes: PlannedVolumes,
    pub services: Vec<RequestedServiceSpec>,
    /// The image each tag runs as, by the whole tag reference. Cloud looks them up.
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    pub operation: OperationId,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    /// The signer's own random bytes, which the births' nonces come from.
    pub seed: [u8; 16],
    /// What each mount is compared with.
    pub against: Relayed,
    /// What a Source step drafts the Environment's automatic deploy rule from, when the
    /// signer's tap may sign one.
    #[serde(default)]
    pub source: Option<SourceAsked>,
}

/// What a Source step drafts its rule from, as the Store relays it for the revision the
/// tap goes with: the rule the Environment keeps, ended or not, where each Service builds
/// from, the settings a push deploys, and every Server of the Cluster.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAsked {
    pub rule: Option<Signed<AutoDeployRule>>,
    pub sources: Vec<RuleSourceView>,
    /// Lowered as a push deploys them, each built image left a placeholder.
    pub target: Vec<RequestedServiceSpec>,
    pub machines: Vec<MachineId>,
    /// The repository owner's GitHub ID.
    pub owner_id: u64,
    /// When absent, the kept rule's builder, or Ployz's own release for a new rule or when
    /// the person turns the rule on.
    #[serde(default)]
    pub builder: Option<Builder>,
    /// The branch the person turns Deploy every push on for. Only the switch names one: a
    /// rule they ended starts again, and with several branches it follows this one.
    #[serde(default)]
    pub on: Option<PushBranch>,
}

/// A branch a rule follows: the repository's GitHub ID and the branch's name.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushBranch {
    pub repository_id: u64,
    pub branch: String,
}

impl SourceAsked {
    /// The rule the tap signs as its step at `place`, or none when the kept rule still
    /// approves every Service a push deploys on every Server it may place one on. A rule
    /// the person ended stays ended: only turning it on again signs one.
    fn draft(
        self,
        (cluster, namespace, volumes): (SealingPublicKey, &Namespace, &PlannedVolumes),
        (issued_at, seed): (Timestamp, &[u8; 16]),
        place: ProposalRef,
    ) -> Result<Option<RuleDraft>, RuleDraftError> {
        let mut basis = RuleBasis {
            namespace: namespace.clone(),
            rule: self.rule,
            sources: self.sources,
            target: self.target,
            volumes: volumes.clone(),
            minted: Births::new(),
        };
        let kept = basis.kept();
        // Turning the rule on pins Ployz's own release, so a builder Ployz stopped trusting
        // is replaced there; a Save or Deploy keeps the builder someone approved.
        let builder = (self.builder)
            .or_else(|| match self.on {
                Some(_) => None,
                None => kept.as_ref().map(|kept| kept.source.builder.clone()),
            })
            .unwrap_or_else(Builder::pinned);
        let terms = RuleTerms {
            cluster,
            machines: self.machines,
            owner_id: self.owner_id,
            builder,
            platform: super::rule_draft::renewed_platform(kept.as_ref()),
            // The births the Version steps mint come from the same seed, so a Volume both
            // mount is born once.
            new_rule: RuleId::from_bytes(*seed),
            issued_at,
            signer: SignerKey::Passkey(super::OwnPrompt::Passkey),
        };
        let Some(mut rule) = source_rule(&mut basis, terms, self.on.as_ref())? else {
            return Ok(None);
        };
        rule.proposal = Some(place);
        Ok(Some(rule.drafted(&basis)?))
    }
}

/// The rule a tap signs as its Source step for what `basis` reads, under `terms`, or none
/// when the kept rule still approves every Service a push deploys on every Server it may
/// place one on. A rule the person ended stays ended unless `on` turns it on again.
/// `terms.new_rule` names the rule when none is kept, and seeds the births it mints.
///
/// # Errors
/// Whatever [`AutoDeployRule::draft`] refuses.
pub fn source_rule(
    basis: &mut RuleBasis,
    terms: RuleTerms,
    on: Option<&PushBranch>,
) -> Result<Option<AutoDeployRule>, RuleDraftError> {
    let Some(names) = eligible(basis, on) else {
        return Ok(None);
    };
    let kept = basis.kept();
    let rule = AutoDeployRule::draft(basis, &names, terms)?;
    Ok((!kept.is_some_and(|kept| approves(&kept, &rule, &basis.target))).then_some(rule))
}

/// What a Save needs to keep pushes deploying, read from the Store's rule view alone,
/// before any Server or passkey is reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SourceNeed {
    /// No Source step: the rule was ended, no Service's push deploys from one branch, or
    /// the kept rule approves what a push deploys wherever it may place it.
    None,
    /// The kept rule approves the settings, and only the Servers it names may be out of
    /// date: a placed Service may land on a Server it doesn't name.
    Servers,
    /// A new rule, or a generation for settings the kept rule doesn't approve.
    Rule,
}

/// What a Save of what `basis` reads needs for its Source step, as
/// [`VersionsAsked::draft`] decides it, from the view alone. Every Server the kept rule
/// names stands in for the Cluster's, which only a Server reports.
#[must_use]
pub fn source_need(basis: &RuleBasis) -> SourceNeed {
    let Some(names) = eligible(basis, None) else {
        return SourceNeed::None;
    };
    let Some(kept) = basis.kept() else {
        return SourceNeed::Rule;
    };
    let terms = RuleTerms {
        cluster: kept.cluster,
        machines: kept.machines.clone(),
        owner_id: kept.source.owner_id,
        builder: kept.source.builder.clone(),
        platform: super::rule_draft::renewed_platform(Some(&kept)),
        new_rule: kept.rule,
        issued_at: kept.issued_at,
        signer: SignerKey::Passkey(super::OwnPrompt::Passkey),
    };
    let mut basis = basis.clone();
    match AutoDeployRule::draft(&mut basis, &names, terms) {
        Ok(next) if approves(&kept, &next, &basis.target) => match placed(&next, &basis.target) {
            true => SourceNeed::Servers,
            false => SourceNeed::None,
        },
        _ => SourceNeed::Rule,
    }
}

/// The Services a Source step covers: those whose push deploys, on the one branch they
/// build from, or on `on` when the person turns the rule on for it. Without `on`, none
/// when the person ended the rule, which stays ended until they turn it on again, or when
/// they build from several branches, where the person picks one.
fn eligible(basis: &RuleBasis, on: Option<&PushBranch>) -> Option<Vec<ServiceName>> {
    if let Some(on) = on {
        let names: Vec<ServiceName> = (basis.sources.iter())
            .filter(|source| {
                source.on_push
                    && source.repository_id == on.repository_id
                    && source.branch.as_deref() == Some(on.branch.as_str())
            })
            .map(|source| source.service.clone())
            .collect();
        return (!names.is_empty()).then_some(names);
    }
    if basis.kept().is_some_and(|kept| kept.ended) {
        return None;
    }
    let names: Vec<ServiceName> = (basis.branch_services().into_iter())
        .filter(|name| {
            (basis.sources.iter()).any(|source| source.service == *name && source.branch.is_some())
        })
        .collect();
    let branches: BTreeSet<_> = (basis.sources.iter())
        .filter(|source| names.contains(&source.service))
        .map(|source| (source.repository_id, source.branch.as_deref()))
        .collect();
    (!names.is_empty() && branches.len() == 1).then_some(names)
}

/// Whether `kept` approves what `next` would: on, with the same rule, branch, builder and
/// Services, and every Server `next` names unless no Service it covers is placed by its spec.
fn approves(kept: &AutoDeployRule, next: &AutoDeployRule, target: &[RequestedServiceSpec]) -> bool {
    !kept.ended
        && kept.rule == next.rule
        && kept.namespace == next.namespace
        && kept.source.repository_id == next.source.repository_id
        && kept.source.branch == next.source.branch
        && kept.source.builder == next.source.builder
        && kept.services == next.services
        && (next
            .machines
            .iter()
            .all(|machine| kept.machines.contains(machine))
            || !placed(next, target))
}

/// Whether a spec places a Service `rule` covers, so the Servers it names matter.
fn placed(rule: &AutoDeployRule, target: &[RequestedServiceSpec]) -> bool {
    (rule.services.iter())
        .filter_map(|service| target.iter().find(|spec| spec.name == service.name))
        .any(placement_signed)
}

/// What Ployz Cloud relays to compare each mount with: each Service's last approved
/// version, the keys one enforcing Server trusts, and the mounts of the Containers its
/// Servers list.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relayed {
    /// By the name each spec runs under. A Service left out has no version to compare with.
    #[serde(default)]
    pub parents: BTreeMap<ServiceName, Parent>,
    pub trusted: Trust,
    pub containers: Vec<HeldContainer>,
    /// Each Server whose Containers weren't listed, by name.
    #[serde(default)]
    pub unanswered: Vec<String>,
}

/// A Service's newest parentless signed version, as the Config Store keeps it.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "parent", rename_all = "snake_case")]
pub enum Parent {
    Signed {
        statement: Signed<ServiceVersion>,
        spec: Box<RequestedServiceSpec>,
    },
    /// No Deployment ran one. `deployed` when the Service runs anyway.
    Missing { deployed: bool },
}

/// The keys one Server trusts, as its Inspect reports them.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Trust {
    #[serde(default)]
    pub devices: Vec<DeviceKey>,
    #[serde(default, deserialize_with = "super::trusted_key::readable_passkeys")]
    pub passkeys: Vec<TrustedPasskey>,
    #[serde(default, deserialize_with = "super::scope::readable_grants")]
    pub grants: Vec<TrustedGrant>,
}

impl Relayed {
    /// Each Service's version in `namespace` checked against the relayed keys for
    /// `cluster`, and the relayed Containers' mounts.
    fn evidence(
        self,
        cluster: SealingPublicKey,
        namespace: &Namespace,
    ) -> (BTreeMap<ServiceName, Before>, Held) {
        let Trust {
            devices,
            passkeys,
            grants,
        } = self.trusted;
        let trusted = TrustedKeys::new(devices, passkeys, grants);
        let before = (self.parents.into_iter())
            .map(|(service, parent)| {
                let before = match parent {
                    Parent::Missing { deployed: false } => Before::New,
                    Parent::Missing { deployed: true } => Before::Unknown(Unknown::NoRecord),
                    Parent::Signed { statement, spec } => Approved::verify(
                        &statement,
                        *spec,
                        (&trusted, cluster),
                        (namespace, &service),
                    )
                    .map_or(Before::Unknown(Unknown::Unchecked), Before::Approved),
                };
                (service, before)
            })
            .collect();
        let held = Held::relayed(self.containers, self.unanswered);
        (before, held)
    }
}

/// The bundle a passkey answers, each step body a Server is handed with it, and what the
/// person reads before answering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct VersionsDraft {
    pub bundle: String,
    pub steps: Vec<VersionStep>,
    pub lines: Vec<String>,
    /// The lines that use data the person must say yes to, each after its Service. The
    /// passkey's answer is the yes.
    pub confirm: Vec<String>,
    /// The automatic deploy rule its Source step signs, with what the person reads of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<RuleDraft>,
}

/// One Service's step, with what the Store records beside it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct VersionStep {
    pub service: ServiceName,
    pub body: String,
    pub births: Births,
    /// The image the Servers run, by digest.
    pub image: String,
    /// The tag the Saved State names, when `image` is its pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tag: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DraftError {
    #[error("No service to sign. Nothing signed.")]
    Empty,
    #[error("{0} is listed twice. Nothing signed.")]
    Twice(ServiceName),
    #[error("{0} has no digest pinned for its image. Nothing signed.")]
    Unpinned(ServiceName),
    #[error("The digest pinned for {0} names another image. Nothing signed.")]
    OtherImage(ServiceName),
    #[error(transparent)]
    Birth(#[from] BirthError),
    #[error("A version has no canonical form. Nothing signed.")]
    Canonical(#[from] CanonicalError),
    #[error(transparent)]
    Rule(#[from] RuleDraftError),
}

impl VersionsAsked {
    /// One first version of each Service, which forgets nothing, signed at `issued_at` as
    /// one step of a bundle a passkey answers, in the order the lines show them, then the
    /// automatic deploy rule's Source step when `source` asks for one. None when only a
    /// Source was asked for and the kept rule still approves it.
    ///
    /// # Errors
    /// Refuses before any bundle: no Service and no Source, one listed twice, a tag with no
    /// pin, a pin that isn't the tag itself `@sha256:` and 64 hex digits, a Volume from
    /// another Environment, a rule that can't be drafted, or a body with no canonical form.
    pub fn draft(self) -> Result<Option<VersionsDraft>, DraftError> {
        if self.services.is_empty() && self.source.is_none() {
            return Err(DraftError::Empty);
        }
        let mut pins = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for spec in &self.services {
            if !seen.insert(&spec.name) {
                return Err(DraftError::Twice(spec.name.clone()));
            }
            let image = &spec.container.image;
            if content_addressed(image) {
                continue;
            }
            let pin = self
                .pins
                .get(image)
                .ok_or_else(|| DraftError::Unpinned(spec.name.clone()))?;
            let names_tag = pin
                .strip_prefix(image.as_str())
                .is_some_and(|digest| digest.starts_with('@'));
            if !names_tag || !content_addressed(pin) {
                return Err(DraftError::OtherImage(spec.name.clone()));
            }
            pins.insert(spec.name.clone(), pin.clone());
        }
        let Self {
            mut services,
            against,
            source,
            ..
        } = self;
        let namespace = &self.namespace;
        let minted = birth::mint(
            &mut services,
            (namespace, &self.volumes),
            self.issued_at,
            |name| birth::nonce(&self.seed, name),
        )?;
        let (before, held) = against.evidence(self.cluster, namespace);
        let mounts = Mounts {
            namespace,
            volumes: &self.volumes,
            minted: &minted,
            now: self.issued_at,
            against: Some(Against {
                before: &before,
                held: Some(&held),
            }),
        };
        let approved = ApprovedChange::of(&services, namespace, &pins, &mounts).map_err(
            |error| match error {
                super::deploy::ChangeError::Digest(error) => DraftError::Canonical(error),
                super::deploy::ChangeError::Mounts(error) => DraftError::Birth(error),
            },
        )?;
        let signer = SignerKey::Passkey(super::OwnPrompt::Passkey);
        let mut steps = Vec::new();
        let mut listed = Vec::new();
        for (index, change) in (0_u32..).zip(&approved) {
            let body = to_canonical(&ServiceVersion {
                cluster: self.cluster,
                namespace: namespace.clone(),
                service: change.service.clone(),
                digest: change.digest,
                signed_at: self.issued_at,
                signer,
                proposal: Some(ProposalRef {
                    operation: self.operation,
                    index,
                }),
                forget_older: false,
                parent: None,
            })?;
            listed.push(ProposalStep {
                index,
                kind: StepKind::Version,
                digest: StepDigest::of(&body),
            });
            let births = services
                .iter()
                .find(|spec| spec.name == change.service)
                .map(|spec| birth::carried(spec, &minted))
                .unwrap_or_default();
            steps.push(VersionStep {
                service: change.service.clone(),
                body,
                births,
                image: change.image.clone(),
                tag: change.tag.clone(),
            });
        }
        let source = match source {
            Some(asked) => asked.draft(
                (self.cluster, namespace, &self.volumes),
                (self.issued_at, &self.seed),
                ProposalRef {
                    operation: self.operation,
                    index: u32::try_from(listed.len()).unwrap_or(u32::MAX),
                },
            )?,
            None => None,
        };
        if let Some(rule) = &source {
            listed.push(ProposalStep {
                index: u32::try_from(listed.len()).unwrap_or(u32::MAX),
                kind: StepKind::Source,
                digest: StepDigest::of(&rule.body),
            });
        }
        if listed.is_empty() {
            return Ok(None);
        }
        let bundle = to_canonical(&ProposalApproval {
            cluster: self.cluster,
            environment: namespace.clone(),
            operation: self.operation,
            steps: listed,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            signer,
        })?;
        let mut confirm: Vec<String> = (approved.iter())
            .flat_map(|change| {
                let service = change.service.as_str();
                (change.confirm.iter()).map(move |line| format!("{service}: {line}"))
            })
            .collect();
        if let Some(rule) = &source {
            let summary = &rule.summary;
            confirm.extend((summary.services.iter()).flat_map(|service| {
                let name = service.name.as_str();
                (service.confirm.iter()).map(move |line| format!("{name}: {line}"))
            }));
            if !summary.pinned {
                confirm.push(format!(
                    "builder {}, not Ployz's own release",
                    summary.builder
                ));
            }
        }
        // Cloud chose each pin, so the person reads the whole signed reference, never a
        // short prefix of it.
        let lines = approved
            .into_iter()
            .flat_map(|change| {
                ApprovedChange {
                    tag: None,
                    ..change
                }
                .lines()
            })
            .collect();
        Ok(Some(VersionsDraft {
            bundle,
            steps,
            lines,
            confirm,
            source,
        }))
    }
}

#[cfg(test)]
#[path = "version_draft_tests.rs"]
mod tests;

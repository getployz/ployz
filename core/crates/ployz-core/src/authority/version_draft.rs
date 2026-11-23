//! The versions a person's passkey signs in one prompt: one step of one bundle per
//! Service, drafted where the person signs, from specs and tag pins Cloud relays.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    Against, Approved, ApprovedChange, Before, BirthError, Births, Held, Mounts, PlannedVolumes,
    ServiceVersion, SignerKey, TrustedGrant, TrustedKeys, TrustedPasskey, Unknown, birth,
    canonical::{CanonicalError, Timestamp, to_canonical},
    deploy::content_addressed,
    keys::Signed,
    proposal::{ProposalApproval, ProposalRef, ProposalStep, StepDigest, StepKind},
};
use crate::{
    ContainerObservation, DeviceKey, Namespace, OperationId, RequestedServiceSpec,
    SealingPublicKey, ServiceName,
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
}

/// What Ployz Cloud relays to compare each mount with: each Service's last approved
/// version, the keys one enforcing Server trusts, and the Containers its Servers list.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relayed {
    /// By the name each spec runs under. A Service left out has no version to compare with.
    #[serde(default)]
    pub parents: BTreeMap<ServiceName, Parent>,
    pub trusted: Trust,
    pub containers: Vec<ContainerObservation>,
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
    /// Each Service's version checked against the relayed keys for `cluster`, and the
    /// Containers of `namespace`.
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
        let held = Held::of(namespace, &self.containers, self.unanswered);
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
}

impl VersionsAsked {
    /// One first version of each Service, which forgets nothing, signed at `issued_at` as
    /// one step of a bundle a passkey answers, in the order the lines show them.
    ///
    /// # Errors
    /// Refuses before any bundle: no Service, one listed twice, a tag with no pin, a pin that isn't the tag
    /// itself `@sha256:` and 64 hex digits, a Volume from another Environment, or a body
    /// with no canonical form.
    pub fn draft(self) -> Result<VersionsDraft, DraftError> {
        if self.services.is_empty() {
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
        let bundle = to_canonical(&ProposalApproval {
            cluster: self.cluster,
            environment: namespace.clone(),
            operation: self.operation,
            steps: listed,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            signer,
        })?;
        let confirm = (approved.iter())
            .flat_map(|change| {
                let service = change.service.as_str();
                (change.confirm.iter()).map(move |line| format!("{service}: {line}"))
            })
            .collect();
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
        Ok(VersionsDraft {
            bundle,
            steps,
            lines,
            confirm,
        })
    }
}

#[cfg(test)]
#[path = "version_draft_tests.rs"]
mod tests;

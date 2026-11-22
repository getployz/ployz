//! The versions a person's passkey signs in one prompt: one step of one bundle per
//! Service, drafted where the person signs, from specs and tag pins Cloud relays.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    ApprovedChange, BirthError, Births, Mounts, PlannedVolumes, ServiceVersion, SignerKey, birth,
    canonical::{CanonicalError, Timestamp, to_canonical},
    deploy::content_addressed,
    proposal::{ProposalApproval, ProposalRef, ProposalStep, StepDigest, StepKind},
};
use crate::{Namespace, OperationId, RequestedServiceSpec, SealingPublicKey, ServiceName};

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
}

/// The bundle a passkey answers, each step body a Server is handed with it, and what the
/// person reads before answering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct VersionsDraft {
    pub bundle: String,
    pub steps: Vec<VersionStep>,
    pub lines: Vec<String>,
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
    pub fn draft(mut self) -> Result<VersionsDraft, DraftError> {
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
        let namespace = &self.namespace;
        let minted = birth::mint(
            &mut self.services,
            (namespace, &self.volumes),
            self.issued_at,
            |name| birth::nonce(&self.seed, name),
        )?;
        let mounts = Mounts {
            namespace,
            volumes: &self.volumes,
            minted: &minted,
            now: self.issued_at,
            against: None,
        };
        let approved =
            ApprovedChange::of(&self.services, namespace, &pins, &mounts).map_err(|error| {
                match error {
                    super::deploy::ChangeError::Digest(error) => DraftError::Canonical(error),
                    super::deploy::ChangeError::Mounts(error) => DraftError::Birth(error),
                }
            })?;
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
            let births = self
                .services
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
        })
    }
}

#[cfg(test)]
#[path = "version_draft_tests.rs"]
mod tests;

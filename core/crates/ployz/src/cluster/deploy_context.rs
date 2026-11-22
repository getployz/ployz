//! What a Deploy hands the Client it runs through: the log it writes under, the pull
//! credentials of its Services, and the signatures every Server gets with a Container
//! change.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use ployz_core::{
    ContainerLocator, ContainerWant, DeploymentLogId, NewContainer, RegistryAuth, RunTarget,
    ServiceName,
    authority::{
        BuiltImageDigest, PayloadDigest, RuleEvidence, RuleProof, ServiceVersion, Signed,
        VerifiedBuild, version::MAX_PRESENTED,
    },
};
use ployz_store::{Ask as _, ConfigStore, OrganizationId, VersionsQuery, View};

use crate::cloud_account::Credential;
use crate::failure::Failure;

/// The Deploy this client executes, shared by its clones as their Deploy's own.
#[derive(Clone, Default)]
pub(crate) struct DeployContext {
    /// The Deploy's log; a replacement joining a named Deploy keeps that Deploy's label.
    pub(crate) id: Option<DeploymentLogId>,
    /// Pull credentials by Service.
    pub(crate) registry_auth: BTreeMap<ServiceName, RegistryAuth>,
    /// What every Container change this client sends carries.
    approvals: Arc<Approvals>,
}

/// The signatures a client hands each Server with a Container change.
#[derive(Clone, Debug, Default)]
struct Approvals {
    /// A rule's proof for each Service it built, when a rule deploys in place of a
    /// person's signature.
    proofs: RuleProofs,
    versions: Versions,
}

/// Where a new creation's signed versions come from.
#[derive(Clone, Debug, Default)]
enum Versions {
    #[default]
    None,
    /// A Deploy's: the versions of each Service it creates, by the digest each signs. A
    /// copy carries those of its own spec's digest.
    ByService(BTreeMap<ServiceName, Vec<(PayloadDigest, Signed<ServiceVersion>)>>),
    /// A drain's: each moved copy runs its own spec, so its versions are read from the
    /// Config Store when the drain reaches it.
    Kept(KeptVersions),
}

/// The Config Store that keeps an Organization's signed versions: one in this process, or
/// Cloud's, read as this CLI's credential. Cloud can withhold a version but not forge one:
/// each Server checks every signature against the keys it trusts.
#[derive(Clone)]
pub(crate) enum KeptVersions {
    Local {
        store: Arc<ConfigStore>,
        organization: OrganizationId,
    },
    Cloud(Arc<Credential>),
}

impl std::fmt::Debug for KeptVersions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local { organization, .. } => formatter
                .debug_struct("KeptVersions::Local")
                .field("organization", organization)
                .finish_non_exhaustive(),
            Self::Cloud(credential) => formatter
                .debug_struct("KeptVersions::Cloud")
                .field("cloud", &credential.cloud())
                .finish_non_exhaustive(),
        }
    }
}

impl KeptVersions {
    #[must_use]
    pub(crate) fn new(store: Arc<ConfigStore>, organization: OrganizationId) -> Self {
        Self::Local {
            store,
            organization,
        }
    }

    /// Every version kept for the spec `new` asks for, by the digest its Server checks,
    /// and the newest verified build of its image when a rule built it. A Store that
    /// doesn't answer gives none, and a Server that needs one refuses.
    async fn of(&self, new: &NewContainer) -> Carried {
        let Ok(digest) = PayloadDigest::of(&new.resolved_spec.to_requested(), &new.slot.namespace)
        else {
            return Carried::none();
        };
        let query = VersionsQuery {
            namespace: new.slot.namespace.clone(),
            service: new.slot.service.clone(),
            digest,
            image: image_of(&new.resolved_spec.container.image),
        };
        let read = match self {
            Self::Local {
                store,
                organization,
            } => {
                let organization = organization.clone();
                crate::sdk::store_call(store, move |store| store.versions(&organization, &query))
                    .await
                    .map_err(Failure::from)
            }
            Self::Cloud(credential) => crate::cloud_account::config_store::<View>(
                credential,
                crate::cloud_account::Via::Cli,
                "read",
                &query.to_query(),
                None,
            )
            .await
            .map_err(Failure::from)
            .and_then(|view| VersionsQuery::view(view).map_err(Failure::from)),
        };
        match read {
            Ok(view) => Carried {
                rule: view.rule,
                versions: view.versions,
            },
            Err(error) => {
                tracing::warn!(%error, service = %new.slot.service, "could not read a copy's signed versions");
                Carried::none()
            }
        }
    }
}

/// The content digest `image` names, when it names one.
fn image_of(image: &str) -> Option<BuiltImageDigest> {
    let digest = image.rsplit_once('@').map_or(image, |(_, digest)| digest);
    BuiltImageDigest::parse(digest).ok()
}

/// The proof each Service an automatic deploy rule built carries to every Server, in
/// place of a person's signed version, and the verified builds the Servers issued for
/// them, which every clone of the Deploy's client shares.
#[derive(Clone, Debug, Default)]
pub struct RuleProofs {
    pub rules: BTreeMap<ServiceName, RuleProof>,
    pub issued: Arc<Mutex<BTreeMap<ServiceName, VerifiedBuild>>>,
}

impl RuleProofs {
    /// The verified build a Server issued for each Service, with the claim it covers.
    #[must_use]
    pub fn issued(&self) -> BTreeMap<ServiceName, ployz_store::VerifiedClaim> {
        let issued = self
            .issued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        issued
            .iter()
            .filter_map(|(service, verified)| {
                let proof = self.rules.get(service)?;
                Some((
                    service.clone(),
                    ployz_store::VerifiedClaim {
                        claim: proof.claim.clone(),
                        verified: *verified,
                    },
                ))
            })
            .collect()
    }

    /// The proof a change of `want` carries: its Service's, or for a step on a Container
    /// named only by ID, any of the Deploy's, since such a step needs only the rule's
    /// Environment. Once a Server issued a verified build for that Service, the proof
    /// carries it in place of the token, which may have expired since: a pre-deploy hook
    /// can outlast it.
    fn carried(&self, want: &ContainerWant) -> Option<Box<RuleProof>> {
        let service = match want {
            ContainerWant::Running {
                container: RunTarget::New(new),
                ..
            } => Some(&new.slot.service),
            ContainerWant::Held { container, .. }
            | ContainerWant::Out { container, .. }
            | ContainerWant::Stopped { container, .. }
            | ContainerWant::Gone { container, .. }
            | ContainerWant::Running {
                container: RunTarget::Existing(container),
                ..
            } => match container {
                ContainerLocator::Key(slot) => Some(&slot.service),
                ContainerLocator::Id(_) => None,
            },
        };
        let (service, proof) = service
            .and_then(|service| self.rules.get_key_value(service))
            .or_else(|| self.rules.iter().next())?;
        let mut proof = proof.clone();
        let issued = self
            .issued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(verified) = issued.get(service) {
            proof.evidence = RuleEvidence::Verified(*verified);
        }
        Some(Box::new(proof))
    }
}

/// What a Container change carries to the Server: the signed versions and the proofs for
/// `want`.
pub(super) struct Carried {
    pub(super) rule: Option<Box<RuleProof>>,
    pub(super) versions: Vec<Signed<ServiceVersion>>,
}

impl Carried {
    pub(super) fn none() -> Self {
        Self {
            rule: None,
            versions: Vec::new(),
        }
    }
}

impl DeployContext {
    pub(super) fn set_proofs(&mut self, proofs: RuleProofs) {
        Arc::make_mut(&mut self.approvals).proofs = proofs;
    }

    /// Keep the verified build a Server issued for a creation of `service`, when the
    /// Deploy carried that Service's proof.
    pub(super) fn note_issued(&self, service: &ServiceName, verified: VerifiedBuild) {
        let proofs = &self.approvals.proofs;
        if proofs.rules.contains_key(service) {
            proofs
                .issued
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(service.clone(), verified);
        }
    }

    /// Hand each Service's creations the versions signed for their spec, newest last.
    pub(super) fn set_versions(&mut self, versions: Vec<Signed<ServiceVersion>>) {
        let mut by_service: BTreeMap<ServiceName, Vec<_>> = BTreeMap::new();
        for version in versions {
            if let Ok(body) = version.open() {
                by_service
                    .entry(body.service)
                    .or_default()
                    .push((body.digest, version));
            }
        }
        Arc::make_mut(&mut self.approvals).versions = Versions::ByService(by_service);
    }

    /// Read each new creation's versions from `kept` by the digest of its own spec.
    pub(super) fn set_kept(&mut self, kept: KeptVersions) {
        Arc::make_mut(&mut self.approvals).versions = Versions::Kept(kept);
    }

    /// What a change of `want` carries. A new creation carries its spec's versions; a
    /// step on an existing Container carries none.
    pub(super) async fn carried(&self, want: &ContainerWant) -> Carried {
        let versions = match (want, &self.approvals.versions) {
            (
                ContainerWant::Running {
                    container: RunTarget::New(new),
                    ..
                },
                Versions::ByService(by_service),
            ) => {
                let digest =
                    PayloadDigest::of(&new.resolved_spec.to_requested(), &new.slot.namespace).ok();
                let signed: Vec<_> = by_service
                    .get(&new.slot.service)
                    .into_iter()
                    .flatten()
                    .filter(|(signs, _)| Some(*signs) == digest)
                    .map(|(_, version)| version.clone())
                    .collect();
                let skip = signed.len().saturating_sub(MAX_PRESENTED);
                signed.into_iter().skip(skip).collect()
            }
            (
                ContainerWant::Running {
                    container: RunTarget::New(new),
                    ..
                },
                Versions::Kept(kept),
            ) => return kept.of(new).await,
            (
                ContainerWant::Running { .. }
                | ContainerWant::Held { .. }
                | ContainerWant::Out { .. }
                | ContainerWant::Stopped { .. }
                | ContainerWant::Gone { .. },
                _,
            ) => Vec::new(),
        };
        Carried {
            rule: self.approvals.proofs.carried(want),
            versions,
        }
    }
}

#[cfg(test)]
#[path = "deploy_context_tests.rs"]
mod tests;

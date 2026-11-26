//! The signatures a Deploy or a drain hands every Server with a Container change.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use ployz_core::{
    ContainerLocator, ContainerWant, FreeReceipt, Namespace, NewContainer, ResolvedServiceSpec,
    RunTarget, ServiceName,
    authority::{
        BuiltImageDigest, KeptRule, PayloadDigest, RuleEvidence, ServiceVersion, Signed,
        VerifiedBuild, version::MAX_PRESENTED,
    },
};
use ployz_store::{ConfigStore, OrganizationId, Trusted, VersionsQuery};

use crate::cloud::account::Credential;
use crate::cluster::Carried;
use crate::failure::Failure;

/// The signatures a client hands each Server with a Container change.
#[derive(Clone, Debug, Default)]
pub(crate) struct Approvals {
    /// A rule's proof for each Service it built, when a rule deploys in place of a
    /// person's signature.
    proofs: RuleProofs,
    versions: Versions,
    /// The Free receipt for each Service a Free Build built, which an unsigned creation of
    /// its image carries.
    free: BTreeMap<ServiceName, FreeReceipt>,
}

/// A signed version a Deploy hands its Servers, by the digest it signs and the one it
/// names as its parent.
#[derive(Clone, Debug)]
struct Kept {
    digest: PayloadDigest,
    parent: Option<PayloadDigest>,
    version: Signed<ServiceVersion>,
}

/// Where a new creation's signed versions come from.
#[derive(Clone, Debug, Default)]
enum Versions {
    #[default]
    None,
    /// A Deploy's: the versions of each Service it creates, by the digest each signs. A
    /// copy carries those of its own spec's digest and the parentless ones they edit.
    ByService(BTreeMap<ServiceName, Vec<Kept>>),
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
    /// the newest verified build of its image when a rule built it, and the Free receipt
    /// of its image when a free Build built it. A Store that doesn't answer gives none,
    /// and a Server that needs one refuses.
    async fn of(&self, new: &NewContainer) -> Carried {
        self.read(&new.slot.namespace, &new.resolved_spec).await
    }

    async fn read(&self, namespace: &Namespace, spec: &ResolvedServiceSpec) -> Carried {
        let Some(query) = kept_query(namespace, spec) else {
            return Carried::default();
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
            Self::Cloud(credential) => {
                crate::cloud::account::StoreClient::Cloud(credential, crate::cloud::http::Via::Cli)
                    .read(&query, &Trusted::default())
                    .await
                    .map_err(Failure::from)
            }
        };
        match read {
            Ok(view) => Carried {
                rule: view.rule,
                versions: view.versions,
                free: view.free.filter(|receipt| runs(receipt, spec)),
            },
            Err(error) => {
                tracing::warn!(%error, service = %spec.name, "could not read a copy's signed versions");
                Carried::default()
            }
        }
    }
}

/// What the Config Store keeps for `spec` is read by: the digest a Server checks, and the
/// image a rule may have built. None for a spec with no digest.
pub(crate) fn kept_query(
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
) -> Option<VersionsQuery> {
    Some(VersionsQuery {
        namespace: namespace.clone(),
        service: spec.name.clone(),
        digest: PayloadDigest::of(&spec.to_requested(), namespace).ok()?,
        image: BuiltImageDigest::named_by(&spec.container.image),
    })
}

/// The proof each Service an automatic deploy rule built carries to every Server, in
/// place of a person's signed version, and the verified builds the Servers issued for
/// them, which every clone of the Deploy's client shares.
#[derive(Clone, Debug, Default)]
pub(crate) struct RuleProofs {
    /// Each Service's proof, with the login its Server pulls with under a registry rule.
    rules: BTreeMap<ServiceName, KeptRule>,
    issued: Arc<Mutex<BTreeMap<ServiceName, VerifiedBuild>>>,
}

impl RuleProofs {
    pub(crate) fn insert(&mut self, service: ServiceName, rule: KeptRule) {
        self.rules.insert(service, rule);
    }

    pub(crate) fn contains(&self, service: &ServiceName) -> bool {
        self.rules.contains_key(service)
    }

    /// Each Service a rule's proof covers.
    pub(crate) fn services(&self) -> impl Iterator<Item = &ServiceName> {
        self.rules.keys()
    }

    /// Each Service a registry rule's proof covers, which only a Server that checks one
    /// admits.
    pub(crate) fn registry_services(&self) -> impl Iterator<Item = &ServiceName> {
        self.rules
            .iter()
            .filter(|(_, kept)| kept.proof.evidence == RuleEvidence::Registry)
            .map(|(service, _)| service)
    }

    /// The verified build a Server issued for each Service, with the claim it covers.
    #[must_use]
    pub(crate) fn issued(&self) -> BTreeMap<ServiceName, ployz_store::VerifiedClaim> {
        let issued = self
            .issued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        issued
            .iter()
            .filter_map(|(service, verified)| {
                let kept = self.rules.get(service)?;
                Some((
                    service.clone(),
                    ployz_store::VerifiedClaim {
                        claim: kept.proof.claim.clone()?,
                        verified: *verified,
                    },
                ))
            })
            .collect()
    }

    /// The proof a change of `want` carries: its Service's, none for a Service no rule
    /// covers, or for a step on a Container named only by ID, any of the Deploy's, since such a step needs only the rule's
    /// Environment. Once a Server issued a verified build for that Service, the proof
    /// carries it in place of the token, which may have expired since: a pre-deploy hook
    /// can outlast it.
    fn carried(&self, want: &ContainerWant) -> Option<KeptRule> {
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
        let (service, kept) = match service {
            Some(service) => self.rules.get_key_value(service)?,
            None => self.rules.iter().next()?,
        };
        let mut kept = kept.clone();
        let issued = self
            .issued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(verified) = issued.get(service) {
            kept.proof.evidence = RuleEvidence::Verified(*verified);
        }
        Some(kept)
    }
}

impl Approvals {
    /// Hand each Service's creations the versions signed for their spec, each with the
    /// parentless versions it names as its parent, in the order given, and each change the
    /// rule's proof for its Service, in place of a person's signed version.
    pub(crate) fn signed(versions: Vec<Signed<ServiceVersion>>, proofs: RuleProofs) -> Self {
        let mut by_service: BTreeMap<ServiceName, Vec<_>> = BTreeMap::new();
        for version in versions {
            if let Ok(body) = version.open() {
                by_service.entry(body.service).or_default().push(Kept {
                    digest: body.digest,
                    parent: body.parent,
                    version,
                });
            }
        }
        Self {
            proofs,
            versions: Versions::ByService(by_service),
            free: BTreeMap::new(),
        }
    }

    /// Hand each creation of a Service a Free Build built the Free receipt in `free` for
    /// the image it runs.
    #[must_use]
    pub(crate) fn with_free(self, free: BTreeMap<ServiceName, FreeReceipt>) -> Self {
        Self { free, ..self }
    }

    /// Read each new creation's versions from `kept` by the digest of its own spec.
    pub(crate) fn kept(kept: KeptVersions) -> Self {
        Self {
            proofs: RuleProofs::default(),
            versions: Versions::Kept(kept),
            free: BTreeMap::new(),
        }
    }

    /// Keep the verified build a Server issued for a creation of `service`, when the
    /// Deploy carried that Service's proof.
    pub(super) fn note_issued(&self, service: &ServiceName, verified: VerifiedBuild) {
        let proofs = &self.proofs;
        if proofs.rules.contains_key(service) {
            proofs
                .issued
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(service.clone(), verified);
        }
    }

    /// What a change of `want` carries. A new creation carries its spec's versions; a
    /// step on an existing Container carries none.
    pub(super) async fn carried(&self, want: &ContainerWant) -> Carried {
        let versions = match (want, &self.versions) {
            (
                ContainerWant::Running {
                    container: RunTarget::New(new),
                    ..
                },
                Versions::ByService(by_service),
            ) => {
                let digest =
                    PayloadDigest::of(&new.resolved_spec.to_requested(), &new.slot.namespace).ok();
                let kept = by_service
                    .get(&new.slot.service)
                    .map_or(&[][..], Vec::as_slice);
                let parents: Vec<PayloadDigest> = kept
                    .iter()
                    .filter(|kept| Some(kept.digest) == digest)
                    .filter_map(|kept| kept.parent)
                    .collect();
                // A Server takes an edit only beside the parentless version it edits.
                kept.iter()
                    .filter(|kept| {
                        Some(kept.digest) == digest
                            || (kept.parent.is_none() && parents.contains(&kept.digest))
                    })
                    .take(MAX_PRESENTED)
                    .map(|kept| kept.version.clone())
                    .collect()
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
            rule: self.proofs.carried(want),
            versions,
            free: self.free_receipt(want),
        }
    }

    /// The Free receipt a new creation carries: its Service's, when it runs the image that
    /// receipt names.
    fn free_receipt(&self, want: &ContainerWant) -> Option<FreeReceipt> {
        let ContainerWant::Running {
            container: RunTarget::New(new),
            ..
        } = want
        else {
            return None;
        };
        self.free
            .get(&new.slot.service)
            .filter(|receipt| runs(receipt, &new.resolved_spec))
            .cloned()
    }

    /// Whether a new creation of `spec` in `namespace` carries a Free receipt, so only a
    /// Server that takes one may run it.
    pub(crate) async fn carries_free(
        &self,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
    ) -> bool {
        match &self.versions {
            Versions::Kept(kept) => kept.read(namespace, spec).await.free.is_some(),
            Versions::None | Versions::ByService(_) => self
                .free
                .get(&spec.name)
                .is_some_and(|receipt| runs(receipt, spec)),
        }
    }
}

/// Whether `spec` runs the image `receipt` names.
fn runs(receipt: &FreeReceipt, spec: &ResolvedServiceSpec) -> bool {
    spec.container
        .image
        .rsplit_once('@')
        .is_some_and(|(_, image)| image == receipt.image.as_str())
}

#[cfg(test)]
#[path = "approvals_tests.rs"]
mod tests;

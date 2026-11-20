//! What a Deploy hands the Client it runs through: the log it writes under, the pull
//! credentials of its Services, and the signatures every Server gets with a Container
//! change.

use std::{collections::BTreeMap, sync::Arc};

use ployz_core::{
    ContainerLocator, ContainerWant, DeploymentLogId, NewContainer, RegistryAuth, RunTarget,
    ServiceName,
    authority::{PayloadDigest, RuleProof, ServiceVersion, Signed, version::MAX_PRESENTED},
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

    /// Every version kept for the spec `new` asks for, by the digest its Server checks.
    /// A Store that doesn't answer gives none, and a Server that needs one refuses.
    async fn of(&self, new: &NewContainer) -> Vec<Signed<ServiceVersion>> {
        let Ok(digest) = PayloadDigest::of(&new.resolved_spec.to_requested(), &new.slot.namespace)
        else {
            return Vec::new();
        };
        let query = VersionsQuery {
            namespace: new.slot.namespace.clone(),
            service: new.slot.service.clone(),
            digest,
        };
        let read = match self {
            Self::Local {
                store,
                organization,
            } => {
                let organization = organization.clone();
                crate::sdk::store_call(store, move |store| {
                    store.versions_for(&organization, (&query.namespace, &query.service), digest)
                })
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
            .and_then(|view| VersionsQuery::view(view).map_err(Failure::from))
            .map(|view| view.versions),
        };
        read.unwrap_or_else(|error| {
            tracing::warn!(%error, service = %new.slot.service, "could not read a copy's signed versions");
            Vec::new()
        })
    }
}

/// The proof each Service an automatic deploy rule built carries to every Server, in
/// place of a person's signed version.
#[derive(Clone, Debug, Default)]
pub struct RuleProofs {
    pub rules: BTreeMap<ServiceName, RuleProof>,
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
            ) => kept.of(new).await,
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
            rule: proof_for(&self.approvals.proofs.rules, want),
            versions,
        }
    }
}

/// The proof in `proofs` a change of `want` carries: its Service's, or for a step on a
/// Container named only by ID, any of the Deploy's, since such a step needs only the
/// rule's Environment.
fn proof_for(
    proofs: &BTreeMap<ServiceName, RuleProof>,
    want: &ContainerWant,
) -> Option<Box<RuleProof>> {
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
    service
        .and_then(|service| proofs.get(service))
        .or_else(|| proofs.values().next())
        .cloned()
        .map(Box::new)
}

#[cfg(test)]
#[path = "deploy_context_tests.rs"]
mod tests;

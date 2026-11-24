//! Names that ran signed, and the Service Containers a proxy may answer for them.
//!
//! A Qualified Service whose Container a Machine once admitted signed stays signed: no
//! Container admitted any other way answers its name again, even after every signed
//! Container is gone. Each Machine decides that from what it has seen itself.

use std::collections::BTreeSet;

use crate::{Admission, ContainerObservation, QualifiedService, ServiceContainer};

/// The `cluster` key prefix of a name that ran signed. One row per Qualified Service,
/// inserted once and never updated or deleted, so replicas only ever add rows.
pub const SIGNED_SERVICE_KEY_PREFIX: &str = "signed-service:";

/// The `cluster` key that keeps `service` signed.
#[must_use]
pub fn signed_service_key(service: &QualifiedService) -> String {
    format!("{SIGNED_SERVICE_KEY_PREFIX}{service}")
}

/// The Qualified Services one Machine has seen run signed. It only grows.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KeptNames(BTreeSet<QualifiedService>);

impl KeptNames {
    /// Keep every name `other` keeps.
    pub fn keep(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    /// Keep each name a `cluster` key in `keys` holds. Keys of other rows, and a name
    /// that doesn't parse, are skipped.
    pub fn keep_keys<'k>(&mut self, keys: impl IntoIterator<Item = &'k str>) {
        self.0.extend(
            keys.into_iter()
                .filter_map(|key| key.strip_prefix(SIGNED_SERVICE_KEY_PREFIX))
                .filter_map(|name| QualifiedService::parse(name).ok()),
        );
    }

    /// Keep the name of every Service Container in `observations` admitted signed.
    pub fn keep_signed<'o>(
        &mut self,
        observations: impl IntoIterator<Item = &'o ContainerObservation>,
    ) {
        self.0.extend(
            observations
                .into_iter()
                .filter(|observation| {
                    observation.kind == crate::ContainerKind::ServiceContainer
                        && observation.signed()
                })
                .map(ContainerObservation::identity),
        );
    }

    #[must_use]
    pub fn contains(&self, service: &QualifiedService) -> bool {
        self.0.contains(service)
    }

    /// The names held for one pass over `containers`: these, and the name of each Service
    /// Container in `containers` its Machine vouched for, while it is in view. A Local
    /// copy holds its name only this way, so the name frees once it is gone.
    #[must_use]
    pub fn in_view(&self, containers: &[ServiceContainer]) -> Self {
        let mut held = self.clone();
        held.0.extend(
            containers
                .iter()
                .map(ServiceContainer::as_observation)
                .filter(|observation| observation.vouched())
                .map(ContainerObservation::identity),
        );
        held
    }

    /// Whether a proxy may route `observation`: its Machine vouched for it, or its name is
    /// not held.
    #[must_use]
    pub fn admits(&self, observation: &ContainerObservation) -> bool {
        observation.vouched() || !self.contains(&observation.identity())
    }
}

/// The Service Containers a proxy may route: all of `containers`, less each one its
/// Machine didn't vouch for whose name `held` holds. A Container whose Machine doesn't
/// report its admission counts as not vouched for.
#[must_use]
pub fn signed_view(containers: &[ServiceContainer], held: &KeptNames) -> Vec<ServiceContainer> {
    containers
        .iter()
        .filter(|container| held.admits(container.as_observation()))
        .cloned()
        .collect()
}

impl crate::ContainerObservationParts {
    /// Whether its Machine reports it admitted signed, which keeps its name.
    #[must_use]
    pub fn signed(&self) -> bool {
        self.admission == Some(Admission::Signed)
    }

    /// Whether a request admitted `wanted` may adopt it: its Machine vouched for it at
    /// least as much. One whose Machine doesn't report counts as admitted free.
    #[must_use]
    pub fn admits_as(&self, wanted: Admission) -> bool {
        self.admission.unwrap_or(Admission::Free) >= wanted
    }

    /// Whether its Machine reports it admitted signed or from its own socket, so it routes
    /// under a held name and owns a Hostname ahead of a copy admitted free.
    #[must_use]
    pub fn vouched(&self) -> bool {
        self.admission >= Some(Admission::Local)
    }
}

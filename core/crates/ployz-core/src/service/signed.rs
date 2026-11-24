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
}

/// The Service Containers a proxy may route: all of `containers`, less each one not
/// admitted signed whose name `kept` holds. A Container whose Machine doesn't report its
/// admission counts as not signed.
#[must_use]
pub fn signed_view(containers: &[ServiceContainer], kept: &KeptNames) -> Vec<ServiceContainer> {
    containers
        .iter()
        .filter(|container| {
            let observation = container.as_observation();
            observation.signed() || !kept.contains(&observation.identity())
        })
        .cloned()
        .collect()
}

impl crate::ContainerObservationParts {
    /// Whether its Machine reports it admitted signed.
    #[must_use]
    pub fn signed(&self) -> bool {
        self.admission == Some(Admission::Signed)
    }
}

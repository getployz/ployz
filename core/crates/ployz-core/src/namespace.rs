//! Observer-derived Namespaces. There is no standalone Namespace record.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{ContainerObservation, DockerVolumeId, NAMESPACE_LABEL, Namespace, QualifiedService};

/// One observer-derived Namespace. It exists while this observer sees an owned
/// Service or a managed volume.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NamespaceObservation {
    pub name: Namespace,
    #[serde(default)]
    pub services: Vec<QualifiedService>,
    #[serde(default)]
    pub volumes: Vec<DockerVolumeId>,
}

/// Namespace ownership from Docker Volume labels. Missing or invalid labels are
/// not a Namespace; the volume name is never consulted.
#[must_use]
pub fn owned_volume_namespace(labels: &BTreeMap<String, String>) -> Option<Namespace> {
    Namespace::parse(labels.get(NAMESPACE_LABEL)?).ok()
}

/// Derive Namespaces from owned Services and managed volumes.
///
/// Unlabeled volumes and invalid Namespace labels are skipped. The result is
/// whatever this observer supplied; it is not Cluster completeness.
#[must_use]
pub fn derive_namespaces<'a>(
    containers: impl IntoIterator<Item = &'a ContainerObservation>,
    volumes: impl IntoIterator<Item = (&'a DockerVolumeId, &'a BTreeMap<String, String>)>,
) -> Vec<NamespaceObservation> {
    let mut namespaces =
        BTreeMap::<Namespace, (BTreeSet<QualifiedService>, BTreeSet<DockerVolumeId>)>::new();
    for observation in containers {
        namespaces
            .entry(observation.namespace.clone())
            .or_default()
            .0
            .insert(observation.identity());
    }
    for (id, labels) in volumes {
        let Some(name) = owned_volume_namespace(labels) else {
            continue;
        };
        namespaces.entry(name).or_default().1.insert(id.clone());
    }
    namespaces
        .into_iter()
        .map(|(name, (services, volumes))| NamespaceObservation {
            name,
            services: services.into_iter().collect(),
            volumes: volumes.into_iter().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::{
        ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
        DockerVolumeName, MANAGED_LABEL, MachineId, ResolvedServiceSpec, ServiceId, ServiceName,
    };

    #[test]
    fn namespaces_come_from_owned_services_and_managed_volumes() {
        let shop = observation("shop", "web");
        let staging = observation("shop-staging", "web");
        let shop_data = volume("shop_data", Some("shop"));
        let staging_data = volume("shop-staging_data", Some("shop-staging"));
        let namespaces = derive_namespaces(
            [&shop, &staging],
            [as_volume(&shop_data), as_volume(&staging_data)],
        );
        assert_eq!(names(&namespaces), ["shop", "shop-staging"]);
        let [shop_namespace, staging_namespace] = namespaces.as_slice() else {
            panic!("expected two Namespaces, got {namespaces:?}");
        };
        assert_eq!(
            shop_namespace.services,
            [QualifiedService::parse("shop/web").unwrap()]
        );
        assert_eq!(
            shop_namespace
                .volumes
                .first()
                .expect("shop owns a volume")
                .name
                .as_str(),
            "shop_data"
        );
        assert_eq!(
            staging_namespace.services,
            [QualifiedService::parse("shop-staging/web").unwrap()]
        );
    }

    #[test]
    fn a_namespace_holding_only_volumes_still_lists() {
        let data = volume("shop_data", Some("shop"));
        let namespaces = derive_namespaces([] as [&ContainerObservation; 0], [as_volume(&data)]);
        assert_eq!(names(&namespaces), ["shop"]);
        let shop = namespaces.first().expect("shop still lists");
        assert!(shop.services.is_empty());
        assert_eq!(shop.volumes.len(), 1);
    }

    #[test]
    fn a_namespace_disappears_when_this_observer_sees_no_owned_service_or_volume() {
        let leftover = observation("other", "web");
        let orphan = volume("orphan", None);
        let namespaces = derive_namespaces([&leftover], [as_volume(&orphan)]);
        assert_eq!(names(&namespaces), ["other"]);
        assert!(!names(&namespaces).contains(&"shop"));
    }

    #[test]
    fn unlabeled_and_invalid_volume_labels_are_not_assigned_to_a_namespace() {
        let guessed = volume("shop_data", None);
        let invalid = (
            DockerVolumeId {
                machine_id: machine_id(),
                name: DockerVolumeName::parse("broken").unwrap(),
            },
            BTreeMap::from([
                (NAMESPACE_LABEL.to_owned(), "Not_DNS".to_owned()),
                (MANAGED_LABEL.to_owned(), String::new()),
            ]),
        );
        let managed_only = (
            DockerVolumeId {
                machine_id: machine_id(),
                name: DockerVolumeName::parse("managed").unwrap(),
            },
            BTreeMap::from([(MANAGED_LABEL.to_owned(), String::new())]),
        );
        let namespaces = derive_namespaces(
            [] as [&ContainerObservation; 0],
            [
                as_volume(&guessed),
                as_volume(&invalid),
                as_volume(&managed_only),
            ],
        );
        assert!(namespaces.is_empty(), "{namespaces:?}");
    }

    #[test]
    fn reserved_namespace_still_lists() {
        let ingress = observation("ployz-system", "ingress");
        let namespaces = derive_namespaces([&ingress], std::iter::empty());
        assert_eq!(names(&namespaces), ["ployz-system"]);
    }

    fn names(namespaces: &[NamespaceObservation]) -> Vec<&str> {
        namespaces
            .iter()
            .map(|namespace| namespace.name.as_str())
            .collect()
    }

    fn observation(namespace: &str, service: &str) -> ContainerObservation {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let service_name = ServiceName::parse(service).unwrap();
        let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "nginx", "pull_policy": "missing" }
        }))
        .unwrap();
        ContainerObservation::try_from(crate::ContainerObservationParts {
            container_id: ContainerId::parse("c".repeat(64)).unwrap(),
            display_name: format!("{service}-c"),
            created_at_unix_nanos: 0,
            machine_id: machine_id(),
            namespace: Namespace::parse(namespace).unwrap(),
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Created,
            effective_healthcheck: None,
            resolved_spec,
            address: None,
            labels: BTreeMap::new(),
        })
        .unwrap()
    }

    fn as_volume(
        volume: &(DockerVolumeId, BTreeMap<String, String>),
    ) -> (&DockerVolumeId, &BTreeMap<String, String>) {
        (&volume.0, &volume.1)
    }

    fn volume(name: &str, namespace: Option<&str>) -> (DockerVolumeId, BTreeMap<String, String>) {
        let id = DockerVolumeId {
            machine_id: machine_id(),
            name: DockerVolumeName::parse(name).unwrap(),
        };
        let labels = namespace
            .map(|namespace| {
                BTreeMap::from([
                    (NAMESPACE_LABEL.to_owned(), namespace.to_owned()),
                    (MANAGED_LABEL.to_owned(), String::new()),
                ])
            })
            .unwrap_or_default();
        (id, labels)
    }

    fn machine_id() -> MachineId {
        MachineId::parse("1".repeat(32)).unwrap()
    }
}

//! Observer-derived Projects. There is no standalone Project record.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ContainerObservation, DockerVolumeId, PROJECT_NAME_LABEL, ProjectName, QualifiedService,
};

/// One observer-derived Project. It exists while this observer sees an owned
/// Service or a managed volume.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectObservation {
    pub name: ProjectName,
    #[serde(default)]
    pub services: Vec<QualifiedService>,
    #[serde(default)]
    pub volumes: Vec<DockerVolumeId>,
}

/// Project ownership from Docker Volume labels. Missing or invalid labels are
/// not a Project; the volume name is never consulted.
#[must_use]
pub fn owned_volume_project(labels: &BTreeMap<String, String>) -> Option<ProjectName> {
    ProjectName::parse(labels.get(PROJECT_NAME_LABEL)?).ok()
}

/// Derive Projects from owned Services and managed volumes.
///
/// Unlabeled volumes and invalid Project labels are skipped. The result is
/// whatever this observer supplied; it is not Cluster completeness.
#[must_use]
pub fn derive_projects<'a>(
    containers: impl IntoIterator<Item = &'a ContainerObservation>,
    volumes: impl IntoIterator<Item = (&'a DockerVolumeId, &'a BTreeMap<String, String>)>,
) -> Vec<ProjectObservation> {
    let mut projects =
        BTreeMap::<ProjectName, (BTreeSet<QualifiedService>, BTreeSet<DockerVolumeId>)>::new();
    for observation in containers {
        projects
            .entry(observation.project_name.clone())
            .or_default()
            .0
            .insert(observation.identity());
    }
    for (id, labels) in volumes {
        let Some(name) = owned_volume_project(labels) else {
            continue;
        };
        projects.entry(name).or_default().1.insert(id.clone());
    }
    projects
        .into_iter()
        .map(|(name, (services, volumes))| ProjectObservation {
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
    fn projects_come_from_owned_services_and_managed_volumes() {
        let shop = observation("shop", "web");
        let staging = observation("shop-staging", "web");
        let shop_data = volume("shop_data", Some("shop"));
        let staging_data = volume("shop-staging_data", Some("shop-staging"));
        let projects = derive_projects(
            [&shop, &staging],
            [as_volume(&shop_data), as_volume(&staging_data)],
        );
        assert_eq!(names(&projects), ["shop", "shop-staging"]);
        let [shop_project, staging_project] = projects.as_slice() else {
            panic!("expected two Projects, got {projects:?}");
        };
        assert_eq!(
            shop_project.services,
            [QualifiedService::parse("shop/web").unwrap()]
        );
        assert_eq!(
            shop_project
                .volumes
                .first()
                .expect("shop owns a volume")
                .name
                .as_str(),
            "shop_data"
        );
        assert_eq!(
            staging_project.services,
            [QualifiedService::parse("shop-staging/web").unwrap()]
        );
    }

    #[test]
    fn a_project_holding_only_volumes_still_lists() {
        let data = volume("shop_data", Some("shop"));
        let projects = derive_projects([] as [&ContainerObservation; 0], [as_volume(&data)]);
        assert_eq!(names(&projects), ["shop"]);
        let shop = projects.first().expect("shop still lists");
        assert!(shop.services.is_empty());
        assert_eq!(shop.volumes.len(), 1);
    }

    #[test]
    fn a_project_disappears_when_this_observer_sees_no_owned_service_or_volume() {
        let leftover = observation("other", "web");
        let orphan = volume("orphan", None);
        let projects = derive_projects([&leftover], [as_volume(&orphan)]);
        assert_eq!(names(&projects), ["other"]);
        assert!(!names(&projects).contains(&"shop"));
    }

    #[test]
    fn unlabeled_and_invalid_volume_labels_are_not_assigned_to_a_project() {
        let guessed = volume("shop_data", None);
        let invalid = (
            DockerVolumeId {
                machine_id: machine_id(),
                name: DockerVolumeName::parse("broken").unwrap(),
            },
            BTreeMap::from([
                (PROJECT_NAME_LABEL.to_owned(), "Not_DNS".to_owned()),
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
        let projects = derive_projects(
            [] as [&ContainerObservation; 0],
            [
                as_volume(&guessed),
                as_volume(&invalid),
                as_volume(&managed_only),
            ],
        );
        assert!(projects.is_empty(), "{projects:?}");
    }

    #[test]
    fn reserved_project_still_lists() {
        let ingress = observation("ployz-system", "ingress");
        let projects = derive_projects([&ingress], std::iter::empty());
        assert_eq!(names(&projects), ["ployz-system"]);
    }

    fn names(projects: &[ProjectObservation]) -> Vec<&str> {
        projects
            .iter()
            .map(|project| project.name.as_str())
            .collect()
    }

    fn observation(project: &str, service: &str) -> ContainerObservation {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let service_name = ServiceName::parse(service).unwrap();
        let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "nginx", "pull_policy": "missing" }
        }))
        .unwrap();
        ContainerObservation {
            container_id: ContainerId::parse("c".repeat(64)).unwrap(),
            display_name: format!("{service}-c"),
            created_at_unix_nanos: 0,
            machine_id: machine_id(),
            project_name: ProjectName::parse(project).unwrap(),
            service_id,
            service_name,
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Created,
            effective_healthcheck: None,
            resolved_spec,
            address: None,
            labels: BTreeMap::new(),
        }
    }

    fn as_volume(
        volume: &(DockerVolumeId, BTreeMap<String, String>),
    ) -> (&DockerVolumeId, &BTreeMap<String, String>) {
        (&volume.0, &volume.1)
    }

    fn volume(name: &str, project: Option<&str>) -> (DockerVolumeId, BTreeMap<String, String>) {
        let id = DockerVolumeId {
            machine_id: machine_id(),
            name: DockerVolumeName::parse(name).unwrap(),
        };
        let labels = project
            .map(|project| {
                BTreeMap::from([
                    (PROJECT_NAME_LABEL.to_owned(), project.to_owned()),
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

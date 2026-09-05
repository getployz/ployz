//! Serving Containers and Machine-local occupancy of one Serving Shape.

use std::collections::BTreeMap;

use crate::{
    ContainerAddress, ContainerObservation, ContainerRuntimeObservation, QualifiedService,
    ServiceContainer, ServingShape,
};

/// Proof that a Service Container is healthy, addressed, and of a selected Serving Shape.
///
/// Only [`serving_containers`] constructs this type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ServingContainer<'serving> {
    container: &'serving ServiceContainer,
    address: ContainerAddress,
}

impl<'serving> ServingContainer<'serving> {
    /// Container Address proven when this Serving Container was built.
    #[must_use]
    pub fn address(self) -> ContainerAddress {
        self.address
    }

    /// Borrow the proven Service Container.
    #[must_use]
    pub fn as_container(self) -> &'serving ServiceContainer {
        self.container
    }

    /// Borrow the mixed observation this Serving Container was proven from.
    #[must_use]
    pub fn as_observation(self) -> &'serving ContainerObservation {
        self.container.as_observation()
    }
}

/// Serving Containers for this observer: healthy, addressed, and of the newest
/// observed Serving Shape per Qualified Service.
#[must_use]
pub fn serving_containers<'serving>(
    containers: impl IntoIterator<Item = &'serving ServiceContainer>,
) -> Vec<ServingContainer<'serving>> {
    let mut by_identity = BTreeMap::<QualifiedService, Vec<&'serving ServiceContainer>>::new();
    for container in containers {
        by_identity
            .entry(container.as_observation().identity())
            .or_default()
            .push(container);
    }
    let mut serving = Vec::new();
    for members in by_identity.into_values() {
        let Some(newest) = members.iter().max_by_key(|container| {
            let observation = container.as_observation();
            (
                observation.created_at_unix_nanos,
                observation.container_id.as_str(),
            )
        }) else {
            continue;
        };
        let selected = newest.as_observation().resolved_spec.serving_shape();
        for container in members {
            let Some(address) = container.traffic_address() else {
                continue;
            };
            if container.as_observation().resolved_spec.serving_shape() == selected {
                serving.push(ServingContainer { container, address });
            }
        }
    }
    serving
}

/// Machine-local occupancy of one wanted Serving Shape.
#[derive(Debug)]
pub enum SlotOccupancy {
    /// A local Container carries this shape. Reuse it.
    Current(Box<ContainerObservation>),
    /// Only other shapes of this Service exist locally. They stay running.
    OtherShape,
    /// No Container of this Service exists locally.
    Empty,
}

impl SlotOccupancy {
    /// Classify pre-filtered same-Service observations against one shape.
    #[must_use]
    pub fn classify(
        existing: impl IntoIterator<Item = ContainerObservation>,
        wanted: ServingShape,
    ) -> Self {
        let mut current = None;
        let mut saw_other = false;
        for observation in existing {
            if observation.resolved_spec.serving_shape() == wanted {
                if matches!(
                    observation.runtime,
                    ContainerRuntimeObservation::Running { .. }
                ) {
                    return Self::Current(Box::new(observation));
                }
                current = Some(observation);
            } else {
                saw_other = true;
            }
        }
        match (current, saw_other) {
            (Some(current), _) => Self::Current(Box::new(current)),
            (None, true) => Self::OtherShape,
            (None, false) => Self::Empty,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{SlotOccupancy, serving_containers};
    use crate::{
        ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
        ContainerRuntimeObservation, HealthObservation, MachineId, ProjectName,
        ResolvedServiceSpec, ServiceId, ServiceName, service_containers,
    };

    #[test]
    fn serving_containers_are_healthy_addressed_service_containers() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let healthy = serving_observation(
            '1',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            Some([10, 210, 1, 2]),
        );
        let not_configured = serving_observation(
            '2',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::NotConfigured,
            },
            Some([10, 210, 1, 3]),
        );
        let hook = serving_observation(
            '3',
            &service_id,
            ContainerKind::PreDeployHook,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            Some([10, 210, 1, 4]),
        );
        let starting = serving_observation(
            '4',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Starting,
            },
            Some([10, 210, 1, 5]),
        );
        let unhealthy = serving_observation(
            '5',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Unhealthy,
            },
            Some([10, 210, 1, 6]),
        );
        let no_address = serving_observation(
            '6',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            None,
        );
        let exited = serving_observation(
            '7',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Exited { code: 0 },
            Some([10, 210, 1, 7]),
        );

        let containers = service_containers([
            healthy.clone(),
            not_configured.clone(),
            hook,
            starting,
            unhealthy,
            no_address,
            exited,
        ]);
        let serving = serving_containers(&containers);

        assert_eq!(
            serving
                .into_iter()
                .map(super::ServingContainer::as_observation)
                .collect::<Vec<_>>(),
            vec![&healthy, &not_configured]
        );
    }

    #[test]
    fn serving_containers_isolate_older_shapes_once_a_newer_shape_is_observed() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let mut v3 = serving_observation(
            '1',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            Some([10, 210, 1, 2]),
        );
        v3.try_update(|parts| {
            parts.created_at_unix_nanos = 1;
            parts.resolved_spec.container.image = "api:3".into();
        })
        .unwrap();
        let mut v4 = serving_observation(
            '2',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            Some([10, 210, 1, 3]),
        );
        v4.try_update(|parts| {
            parts.created_at_unix_nanos = 2;
            parts.resolved_spec.container.image = "api:4".into();
        })
        .unwrap();
        let mut unready_v4 = serving_observation(
            '3',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Starting,
            },
            Some([10, 210, 1, 4]),
        );
        unready_v4
            .try_update(|parts| {
                parts.created_at_unix_nanos = 3;
                parts.resolved_spec.container.image = "api:4".into();
            })
            .unwrap();

        let mixed = service_containers([v3.clone(), v4.clone()]);
        let serving = serving_containers(&mixed);
        assert_eq!(
            serving
                .into_iter()
                .map(super::ServingContainer::as_observation)
                .collect::<Vec<_>>(),
            vec![&v4]
        );

        let unready_newest = service_containers([v3.clone(), unready_v4]);
        assert!(serving_containers(&unready_newest).is_empty());
    }

    #[test]
    fn slot_occupancy_does_not_treat_another_shape_as_current() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let mut v3 = observation(
            '1',
            &service_id,
            "api",
            ContainerKind::ServiceContainer,
            "api:3",
        );
        v3.try_update(|parts| {
            parts.runtime = ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            }
        })
        .unwrap();
        let wanted = {
            let mut spec = v3.resolved_spec.clone();
            spec.container.image = "api:4".into();
            spec.serving_shape()
        };
        assert!(matches!(
            SlotOccupancy::classify([v3], wanted),
            SlotOccupancy::OtherShape
        ));
    }

    #[test]
    fn slot_occupancy_reuses_a_running_wanted_shape() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let mut v4 = observation(
            '1',
            &service_id,
            "api",
            ContainerKind::ServiceContainer,
            "api:4",
        );
        v4.try_update(|parts| {
            parts.runtime = ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            }
        })
        .unwrap();
        let wanted = v4.resolved_spec.serving_shape();
        assert!(matches!(
            SlotOccupancy::classify([v4], wanted),
            SlotOccupancy::Current(_)
        ));
    }

    #[test]
    fn slot_occupancy_is_empty_when_no_local_container_exists() {
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": "a".repeat(32),
            "name": "api",
            "mode": { "mode": "global" },
            "container": { "image": "api:4", "pull_policy": "missing" }
        }))
        .unwrap();
        let wanted = spec.serving_shape();
        assert!(matches!(
            SlotOccupancy::classify([], wanted),
            SlotOccupancy::Empty
        ));
    }

    fn serving_observation(
        id: char,
        service_id: &ServiceId,
        kind: ContainerKind,
        runtime: ContainerRuntimeObservation,
        address: Option<[u8; 4]>,
    ) -> ContainerObservation {
        let mut observation = observation(id, service_id, "api", kind, "api");
        observation
            .try_update(|parts| parts.runtime = runtime)
            .unwrap();
        observation
            .try_update(|parts| {
                parts.address = address.map(|octets| ContainerAddress(octets.into()))
            })
            .unwrap();
        observation
    }

    fn observation(
        id: char,
        service_id: &ServiceId,
        name: &str,
        kind: ContainerKind,
        image: &str,
    ) -> ContainerObservation {
        let service_name = ServiceName::parse(name).unwrap();
        let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": image, "pull_policy": "missing" }
        }))
        .unwrap();
        ContainerObservation::try_from(crate::ContainerObservationParts {
            container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: format!("{name}-{id}"),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
            project_name: ProjectName::parse("app").unwrap(),
            kind,
            runtime: ContainerRuntimeObservation::Created,
            effective_healthcheck: None,
            resolved_spec,
            address: None,
            labels: BTreeMap::new(),
        })
        .unwrap()
    }
}

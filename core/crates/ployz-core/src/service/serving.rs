//! Serving Containers and Machine-local occupancy of one Serving Shape.

use std::collections::BTreeMap;

use crate::{
    ContainerAddress, ContainerObservation, ContainerRuntimeObservation, CopyRotation, MachineId,
    QualifiedService, ServiceContainer, ServingShape,
};

/// Proof that a Service Container is In, may serve, and is addressed.
///
/// Only [`serving_containers`] constructs this type.
#[derive(Clone, Debug, PartialEq)]
pub struct ServingContainer {
    container: ServiceContainer,
    address: ContainerAddress,
}

impl ServingContainer {
    fn prove(container: &ServiceContainer) -> Option<Self> {
        let observation = container.as_observation();
        if *observation.rotation() != CopyRotation::In
            || observation.identity() == QualifiedService::system_ingress()
        {
            return None;
        }
        let address = container.traffic_address()?;
        Some(Self {
            container: container.clone(),
            address,
        })
    }

    /// Container Address proven when this Serving Container was built.
    #[must_use]
    pub fn address(&self) -> ContainerAddress {
        self.address
    }

    /// The Service Container that [`serving_containers`] proved may serve.
    #[must_use]
    pub fn as_container(&self) -> &ServiceContainer {
        &self.container
    }

    /// Borrow the mixed observation this Serving Container was proven from.
    #[must_use]
    pub fn as_observation(&self) -> &ContainerObservation {
        self.container.as_observation()
    }
}

/// Serving Containers: copies that are In, may serve, and are addressed. A copy
/// of the Ingress Proxy Service never is one.
#[must_use]
pub fn serving_containers<'serving>(
    containers: impl IntoIterator<Item = &'serving ServiceContainer>,
) -> Vec<ServingContainer> {
    containers
        .into_iter()
        .filter_map(ServingContainer::prove)
        .collect()
}

/// The copies one proxy routes, per Qualified Service.
pub type ProxyRoutes = BTreeMap<QualifiedService, Vec<ServingContainer>>;

/// What a proxy routes next, for every Service it observes a copy of.
///
/// A Service routes its Serving Containers on Machines that are not Down. With
/// none, it keeps its `current` route while a copy that route lists is out of
/// view or some copy is In or Pending, so lag, Down owners and health blips
/// never empty it. It empties once every copy in view is Held or Out. A Service
/// missing from `current` has never been routed by this proxy, as after a
/// restart, and routes its Serving Containers on any Machine instead. A Service
/// with no observed copy has no route, and the Ingress Proxy Service never has one.
#[must_use]
pub fn proxy_routes(
    containers: &[ServiceContainer],
    is_down: impl Fn(&MachineId) -> bool,
    current: &ProxyRoutes,
) -> ProxyRoutes {
    let mut by_service = BTreeMap::<QualifiedService, Vec<&ServiceContainer>>::new();
    for container in containers {
        let identity = container.as_observation().identity();
        if identity != QualifiedService::system_ingress() {
            by_service.entry(identity).or_default().push(container);
        }
    }
    by_service
        .into_iter()
        .map(|(service, siblings)| {
            let route = next_route(&siblings, &is_down, current.get(&service));
            (service, route)
        })
        .collect()
}

fn next_route(
    siblings: &[&ServiceContainer],
    is_down: impl Fn(&MachineId) -> bool,
    current: Option<&Vec<ServingContainer>>,
) -> Vec<ServingContainer> {
    let (candidates, on_down): (Vec<_>, Vec<_>) = serving_containers(siblings.iter().copied())
        .into_iter()
        .partition(|serving| !is_down(&serving.as_observation().machine_id));
    if !candidates.is_empty() {
        return candidates;
    }
    let Some(current) = current else {
        return on_down;
    };
    let entering = siblings.iter().any(|sibling| {
        matches!(
            sibling.as_observation().rotation(),
            CopyRotation::In | CopyRotation::Pending
        )
    });
    let routed_out_of_view = current.iter().any(|routed| {
        !siblings.iter().any(|sibling| {
            sibling.as_observation().container_id == routed.as_observation().container_id
        })
    });
    if entering || routed_out_of_view {
        current.clone()
    } else {
        Vec::new()
    }
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
    use serde_json::json;

    use super::{SlotOccupancy, proxy_routes, serving_containers};
    use crate::service::tests::observation;
    use crate::{
        ContainerAddress, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
        CopyRotation, DeploymentLogId, HealthObservation, Namespace, ProxyRoutes,
        ResolvedServiceSpec, ServiceContainer, ServiceId, ServiceName, service_containers,
    };

    fn serving_ids(containers: &[ServiceContainer]) -> Vec<&ContainerObservation> {
        let serving = serving_containers(containers);
        containers
            .iter()
            .filter(|container| {
                serving
                    .iter()
                    .any(|serving| serving.as_container() == *container)
            })
            .map(ServiceContainer::as_observation)
            .collect()
    }

    #[test]
    fn serving_containers_are_addressed_in_copies_that_may_serve() {
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
        let failing = serving_observation(
            '8',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Failing,
            },
            Some([10, 210, 1, 8]),
        );

        let containers = service_containers([
            healthy.clone(),
            not_configured.clone(),
            failing.clone(),
            hook,
            starting,
            unhealthy,
            no_address,
            exited,
        ]);

        assert_eq!(
            serving_ids(&containers),
            vec![&healthy, &not_configured, &failing]
        );
    }

    #[test]
    fn only_an_in_copy_serves() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let run = DeploymentLogId::parse("run").unwrap();
        for (rotation, serves) in [
            (CopyRotation::Pending, false),
            (CopyRotation::In, true),
            (CopyRotation::Held(run), false),
            (CopyRotation::Out, false),
        ] {
            let mut copy = serving_observation(
                '1',
                &service_id,
                ContainerKind::ServiceContainer,
                HEALTHY,
                Some([10, 210, 1, 2]),
            );
            copy.try_update(|parts| parts.rotation = Some(rotation.clone()))
                .unwrap();
            let containers = service_containers([copy]);
            assert_eq!(
                serving_containers(&containers).len(),
                usize::from(serves),
                "{rotation:?}"
            );
        }
    }

    #[test]
    fn serving_containers_route_every_in_copy_whatever_its_shape() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let mut v3 = serving_observation(
            '1',
            &service_id,
            ContainerKind::ServiceContainer,
            HEALTHY,
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
            HEALTHY,
            Some([10, 210, 1, 3]),
        );
        v4.try_update(|parts| {
            parts.created_at_unix_nanos = 2;
            parts.resolved_spec.container.image = "api:4".into();
        })
        .unwrap();

        let containers = service_containers([v3.clone(), v4.clone()]);

        assert_eq!(serving_ids(&containers), vec![&v3, &v4]);
    }

    #[test]
    fn serving_containers_keeps_trunk_and_head_rows_of_one_config_service() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": service_id,
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 2 },
            "container": {
                "image": "api",
                "pull_policy": "missing",
                "config_mounts": [{ "config_name": "settings", "target": "/etc/settings.conf" }]
            },
            "configs": [{ "name": "settings", "content": b"token=config-secret".to_vec() }]
        }))
        .unwrap();
        let mut trunk_row = serving_observation(
            '1',
            &service_id,
            ContainerKind::ServiceContainer,
            HEALTHY,
            Some([10, 210, 1, 2]),
        );
        trunk_row
            .try_update(|parts| {
                parts.created_at_unix_nanos = 1;
                parts.resolved_spec = spec.clone();
            })
            .unwrap();
        let mut head_row = serving_observation(
            '2',
            &service_id,
            ContainerKind::ServiceContainer,
            HEALTHY,
            Some([10, 210, 1, 3]),
        );
        head_row
            .try_update(|parts| {
                parts.created_at_unix_nanos = 2;
                parts.resolved_spec = spec;
                parts.resolved_spec.mount_graph.redact_config_content();
            })
            .unwrap();

        let containers = service_containers([trunk_row.clone(), head_row.clone()]);

        assert_eq!(serving_ids(&containers), vec![&trunk_row, &head_row]);
    }

    #[test]
    fn proxy_copies_never_routed_always_removable() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let mut proxy = serving_observation(
            '1',
            &service_id,
            ContainerKind::ServiceContainer,
            HEALTHY,
            Some([10, 210, 1, 2]),
        );
        proxy
            .try_update(|parts| {
                parts.namespace = Namespace::system();
                parts.resolved_spec.name = ServiceName::parse("ingress").unwrap();
            })
            .unwrap();
        assert_eq!(proxy.identity(), crate::QualifiedService::system_ingress());
        let containers = service_containers([proxy]);

        assert!(serving_containers(&containers).is_empty());
        assert!(proxy_routes(&containers, |_| false, &ProxyRoutes::new()).is_empty());
    }

    const HEALTHY: ContainerRuntimeObservation = ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    };

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
                parts.address = address.map(|octets| ContainerAddress(octets.into()));
                parts.rotation = Some(CopyRotation::In);
            })
            .unwrap();
        observation
    }
}

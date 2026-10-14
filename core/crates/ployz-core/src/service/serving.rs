//! Serving Containers and Machine-local occupancy of one Serving Shape.

use std::collections::BTreeMap;

use crate::{
    ContainerAddress, ContainerObservation, ContainerRuntimeObservation, DEPLOYMENT_ID_LABEL,
    DeploymentLogId, QualifiedService, ServiceContainer, ServingPointer, ServingShape,
};

/// Proof that a Service Container may serve, is addressed, and is of a selected Serving Shape.
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

/// Serving Containers for this observer: may serve, addressed, and selected per
/// Qualified Service by its Serving Pointer, or by the newest traffic-eligible
/// Serving Shape when `pointer_for` has none.
///
/// With a pointer, the pointed Containers serve alone while every pointed id is
/// visible and one of them may serve. Otherwise the previous deploy's Containers
/// serve beside them, so an identity routes nothing only when none of those may
/// serve.
#[must_use]
pub fn serving_containers<'serving, 'pointer>(
    containers: impl IntoIterator<Item = &'serving ServiceContainer>,
    pointer_for: impl Fn(&QualifiedService) -> Option<&'pointer ServingPointer>,
) -> Vec<ServingContainer<'serving>> {
    let mut by_identity = BTreeMap::<QualifiedService, Vec<&'serving ServiceContainer>>::new();
    for container in containers {
        by_identity
            .entry(container.as_observation().identity())
            .or_default()
            .push(container);
    }
    let mut serving = Vec::new();
    for (identity, members) in by_identity {
        let selected = match pointer_for(&identity) {
            Some(pointer) => pointed_or_fallback(members, pointer),
            None => newest_shape(members),
        };
        serving.extend(selected.into_iter().filter_map(|container| {
            let address = container.traffic_address()?;
            Some(ServingContainer { container, address })
        }));
    }
    serving
}

fn newest_shape(members: Vec<&ServiceContainer>) -> Vec<&ServiceContainer> {
    let Some(newest) = members
        .iter()
        .filter(|container| container.traffic_address().is_some())
        .max_by_key(|container| {
            let observation = container.as_observation();
            (
                observation.created_at_unix_nanos,
                observation.container_id.as_str(),
            )
        })
    else {
        return Vec::new();
    };
    let selected = newest
        .as_observation()
        .resolved_spec
        .published_serving_shape();
    members
        .into_iter()
        .filter(|container| {
            container
                .as_observation()
                .resolved_spec
                .published_serving_shape()
                == selected
        })
        .collect()
}

fn pointed_or_fallback<'serving>(
    members: Vec<&'serving ServiceContainer>,
    pointer: &ServingPointer,
) -> Vec<&'serving ServiceContainer> {
    let all_visible = pointer.containers.iter().all(|id| {
        members
            .iter()
            .any(|container| container.as_observation().container_id == *id)
    });
    let (pointed, others): (Vec<_>, Vec<_>) = members.into_iter().partition(|container| {
        pointer
            .containers
            .contains(&container.as_observation().container_id)
    });
    let any_pointed_routable = pointed
        .iter()
        .any(|container| container.traffic_address().is_some());
    if all_visible && any_pointed_routable {
        return pointed;
    }
    // The previous deploy is its labelled copies plus the retiring ids that
    // carry no deploy label; a retiring copy labelled with an older deploy is
    // never revived.
    let previous = pointer.previous.as_ref().map(DeploymentLogId::as_str);
    let fallback = others.into_iter().filter(|container| {
        previous.is_some_and(|previous| {
            let observation = container.as_observation();
            match observation.labels.get(DEPLOYMENT_ID_LABEL) {
                Some(deploy) => deploy == previous,
                None => pointer.retiring.contains(&observation.container_id),
            }
        })
    });
    pointed.into_iter().chain(fallback).collect()
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

    use super::{SlotOccupancy, serving_containers};
    use crate::service::tests::observation;
    use crate::{
        ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
        ContainerRuntimeObservation, DEPLOYMENT_ID_LABEL, DeploymentLogId, HealthObservation,
        ResolvedServiceSpec, ServiceContainer, ServiceId, ServingPointer, service_containers,
    };

    #[test]
    fn serving_containers_are_addressed_service_containers_that_may_serve() {
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
        let serving = serving_containers(&containers, |_| None);

        assert_eq!(
            serving
                .into_iter()
                .map(super::ServingContainer::as_observation)
                .collect::<Vec<_>>(),
            vec![&healthy, &not_configured, &failing]
        );
    }

    #[test]
    fn serving_containers_cut_over_only_when_a_newer_shape_can_take_traffic() {
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
        let serving = serving_containers(&mixed, |_| None);
        assert_eq!(
            serving
                .into_iter()
                .map(super::ServingContainer::as_observation)
                .collect::<Vec<_>>(),
            vec![&v4]
        );

        for runtime in [
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Starting,
            },
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Unhealthy,
            },
            ContainerRuntimeObservation::Exited { code: 0 },
        ] {
            unready_v4
                .try_update(|parts| parts.runtime = runtime)
                .unwrap();
            let unready_newest = service_containers([v3.clone(), unready_v4.clone()]);
            assert_eq!(
                serving_containers(&unready_newest, |_| None)
                    .into_iter()
                    .map(super::ServingContainer::as_observation)
                    .collect::<Vec<_>>(),
                vec![&v3]
            );
        }
        unready_v4
            .try_update(|parts| {
                parts.runtime = ContainerRuntimeObservation::Running {
                    health: HealthObservation::Healthy,
                };
                parts.address = None;
            })
            .unwrap();
        let unaddressed_newest = service_containers([v3.clone(), unready_v4]);
        assert_eq!(
            serving_containers(&unaddressed_newest, |_| None)
                .into_iter()
                .map(super::ServingContainer::as_observation)
                .collect::<Vec<_>>(),
            vec![&v3]
        );
    }

    #[test]
    fn serving_containers_keep_a_newer_shape_that_passed_once_and_is_failing_now() {
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
        let mut failing_v4 = serving_observation(
            '2',
            &service_id,
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Failing,
            },
            Some([10, 210, 1, 3]),
        );
        failing_v4
            .try_update(|parts| {
                parts.created_at_unix_nanos = 2;
                parts.resolved_spec.container.image = "api:4".into();
            })
            .unwrap();

        let containers = service_containers([v3, failing_v4.clone()]);
        assert_eq!(
            serving_containers(&containers, |_| None)
                .into_iter()
                .map(super::ServingContainer::as_observation)
                .collect::<Vec<_>>(),
            vec![&failing_v4]
        );
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
        let healthy = ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        };
        let mut trunk_row = serving_observation(
            '1',
            &service_id,
            ContainerKind::ServiceContainer,
            healthy.clone(),
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
            healthy,
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

        assert_eq!(
            serving_containers(&containers, |_| None)
                .into_iter()
                .map(super::ServingContainer::as_observation)
                .collect::<Vec<_>>(),
            vec![&trunk_row, &head_row]
        );
    }

    const HEALTHY: ContainerRuntimeObservation = ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    };

    struct VisibilityCase {
        name: &'static str,
        rows: Vec<(char, u8, ContainerRuntimeObservation)>,
        pointer: bool,
        routed: &'static [char],
    }

    fn visibility_cases() -> Vec<VisibilityCase> {
        let exited = ContainerRuntimeObservation::Exited { code: 0 };
        let booting = (
            'c',
            2,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Starting,
            },
        );
        let old = || {
            vec![
                ('1', 1, HEALTHY),
                ('2', 1, HEALTHY),
                ('3', 1, exited.clone()),
            ]
        };
        let mut cases = vec![
            VisibilityCase {
                name: "pointed all present",
                rows: [old(), vec![('a', 2, HEALTHY), ('b', 2, HEALTHY)]].concat(),
                pointer: true,
                routed: &['a', 'b'],
            },
            VisibilityCase {
                name: "one pointed row missing",
                rows: [old(), vec![('a', 2, HEALTHY)]].concat(),
                pointer: true,
                routed: &['1', '2', 'a'],
            },
            VisibilityCase {
                name: "pointed Container crashed but present",
                rows: [old(), vec![('a', 2, HEALTHY), ('b', 2, exited.clone())]].concat(),
                pointer: true,
                routed: &['a'],
            },
            VisibilityCase {
                name: "pointed row removed after old retire",
                rows: vec![('a', 2, HEALTHY)],
                pointer: true,
                routed: &['a'],
            },
            VisibilityCase {
                name: "previous deploy fully gone",
                rows: vec![('a', 2, HEALTHY), ('b', 2, HEALTHY)],
                pointer: true,
                routed: &['a', 'b'],
            },
            VisibilityCase {
                name: "no pointer",
                rows: [old(), vec![('a', 2, HEALTHY), ('b', 2, HEALTHY)]].concat(),
                pointer: false,
                routed: &['a', 'b'],
            },
        ];
        for case in &mut cases {
            case.rows.push(booting.clone());
        }
        cases
    }

    fn visibility_row(
        id: char,
        deploy: u8,
        runtime: ContainerRuntimeObservation,
    ) -> ContainerObservation {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let octet = u8::try_from(id.to_digit(16).unwrap()).unwrap();
        let mut row = serving_observation(
            id,
            &service_id,
            ContainerKind::ServiceContainer,
            runtime,
            Some([10, 210, 1, octet]),
        );
        row.try_update(|parts| {
            parts.created_at_unix_nanos = i64::from(deploy);
            parts.resolved_spec.container.image = format!("api:{deploy}");
            parts
                .labels
                .insert(DEPLOYMENT_ID_LABEL.into(), format!("d{deploy}"));
        })
        .unwrap();
        row
    }

    fn visibility_pointer(retiring: &[char]) -> ServingPointer {
        pointer_of("d2", &['a', 'b'], Some("d1"), retiring)
    }

    fn visibility_containers(case: &VisibilityCase) -> Vec<ServiceContainer> {
        service_containers(
            case.rows
                .iter()
                .map(|(id, deploy, runtime)| visibility_row(*id, *deploy, runtime.clone())),
        )
    }

    fn id_char(container: &ServiceContainer) -> char {
        container
            .as_observation()
            .container_id
            .as_str()
            .chars()
            .next()
            .unwrap()
    }

    fn routed_ids(case: &VisibilityCase, pointer: &ServingPointer) -> Vec<char> {
        let containers = visibility_containers(case);
        let pointer = case.pointer.then_some(pointer);
        let mut routed: Vec<char> = serving_containers(&containers, |_| pointer)
            .into_iter()
            .map(|serving| id_char(serving.as_container()))
            .collect();
        routed.sort_unstable();
        routed
    }

    #[test]
    fn serving_containers_follow_the_union_rule_per_visibility_case() {
        let pointer = visibility_pointer(&[]);
        for case in visibility_cases() {
            assert_eq!(routed_ids(&case, &pointer), case.routed, "{}", case.name);
        }
    }

    #[test]
    fn serving_containers_never_route_an_unaddressed_container() {
        let pointer = visibility_pointer(&[]);
        for case in visibility_cases() {
            let unaddressed: Vec<char> = visibility_containers(&case)
                .iter()
                .filter(|container| container.traffic_address().is_none())
                .map(id_char)
                .collect();
            assert!(!unaddressed.is_empty(), "{}", case.name);
            for id in routed_ids(&case, &pointer) {
                assert!(!unaddressed.contains(&id), "{}: routed {id}", case.name);
            }
        }
    }

    #[test]
    fn an_unlabelled_old_copy_serves_through_retiring_until_the_pointed_copy_is_visible() {
        let mut unlabelled = visibility_row('1', 1, HEALTHY);
        unlabelled
            .try_update(|parts| {
                parts.labels.remove(DEPLOYMENT_ID_LABEL);
            })
            .unwrap();
        let containers = service_containers([unlabelled]);
        let pointer = ServingPointer {
            deploy: DeploymentLogId::parse("d2").unwrap(),
            containers: vec![ContainerId::parse("b".repeat(64)).unwrap()],
            previous: Some(DeploymentLogId::parse("pin-d2").unwrap()),
            retiring: vec![ContainerId::parse("1".repeat(64)).unwrap()],
        };

        let routed: Vec<char> = serving_containers(&containers, |_| Some(&pointer))
            .into_iter()
            .map(|serving| id_char(serving.as_container()))
            .collect();

        assert_eq!(routed, ['1']);
    }

    #[test]
    fn serving_containers_ignore_the_retiring_list_while_a_pointed_container_routes() {
        let without = visibility_pointer(&[]);
        let with = visibility_pointer(&['1', '2', '3']);
        for case in visibility_cases() {
            assert_eq!(
                routed_ids(&case, &without),
                routed_ids(&case, &with),
                "{}",
                case.name
            );
        }
    }

    const STOPPING: ContainerRuntimeObservation = ContainerRuntimeObservation::Running {
        health: HealthObservation::Stopping,
    };

    fn pointer_routes(
        rows: &[(char, u8, ContainerRuntimeObservation)],
        pointer: &ServingPointer,
    ) -> Vec<char> {
        let containers = service_containers(
            rows.iter()
                .map(|(id, deploy, runtime)| visibility_row(*id, *deploy, runtime.clone())),
        );
        let mut routed: Vec<char> = serving_containers(&containers, |_| Some(pointer))
            .into_iter()
            .map(|serving| id_char(serving.as_container()))
            .collect();
        routed.sort_unstable();
        routed
    }

    fn pointer_of(
        deploy: &str,
        containers: &[char],
        previous: Option<&str>,
        retiring: &[char],
    ) -> ServingPointer {
        let ids = |ids: &[char]| {
            ids.iter()
                .map(|id| ContainerId::parse(id.to_string().repeat(64)).unwrap())
                .collect()
        };
        ServingPointer {
            deploy: DeploymentLogId::parse(deploy).unwrap(),
            containers: ids(containers),
            previous: previous.map(|previous| DeploymentLogId::parse(previous).unwrap()),
            retiring: ids(retiring),
        }
    }

    #[test]
    fn a_rollback_to_a_stopping_container_routes_the_running_one_it_retires() {
        let rows = [('a', 1, STOPPING), ('b', 2, HEALTHY)];
        assert_eq!(
            pointer_routes(&rows, &pointer_of("d3", &['a'], Some("d2"), &['b'])),
            vec!['b']
        );
    }

    #[test]
    fn a_new_pointer_never_revives_a_retiring_container_older_than_previous() {
        let rows = [('a', 1, HEALTHY), ('b', 2, HEALTHY)];
        assert_eq!(
            pointer_routes(&rows, &pointer_of("d3", &['c'], Some("d2"), &['a', 'b'])),
            vec!['b']
        );
    }

    #[test]
    fn routable_pointed_containers_exclude_retiring_and_previous_ones() {
        let rows = [
            ('a', 2, HEALTHY),
            ('b', 2, STOPPING),
            ('1', 1, HEALTHY),
            ('0', 0, HEALTHY),
        ];
        assert_eq!(
            pointer_routes(
                &rows,
                &pointer_of("d2", &['a', 'b'], Some("d1"), &['0', '1'])
            ),
            vec!['a']
        );
    }

    #[test]
    fn nothing_routable_routes_nothing() {
        let rows = [
            ('a', 3, STOPPING),
            ('b', 2, STOPPING),
            ('1', 1, ContainerRuntimeObservation::Exited { code: 0 }),
            ('c', 4, HEALTHY),
        ];
        assert_eq!(
            pointer_routes(&rows, &pointer_of("d3", &['a'], Some("d1"), &['b'])),
            Vec::<char>::new()
        );
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
}

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Container, ContainerObservation, ContainerRef, HookContainer, PartialResult, QualifiedService,
    ServiceContainer, ServiceId, ServiceSelector,
};

/// One observer-derived grouping. Every container keeps its own historical spec.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServiceObservation {
    pub identity: QualifiedService,
    pub service_id: ServiceId,
    #[serde(default)]
    pub containers: Vec<ServiceContainer>,
    #[serde(default)]
    pub hook_containers: Vec<HookContainer>,
}

impl ServiceObservation {
    /// True when this Service's logical name equals `selector`.
    #[must_use]
    pub fn has_name(&self, selector: &str) -> bool {
        self.identity.name.as_str() == selector
    }

    /// True when any member carries this opaque Service ID.
    #[must_use]
    pub fn has_service_id(&self, service_id: &ServiceId) -> bool {
        self.members()
            .any(|container| container.as_observation().service_id == *service_id)
    }

    /// Every role-proven member of this Service.
    pub fn members(&self) -> impl Iterator<Item = ContainerRef<'_>> {
        self.containers
            .iter()
            .map(ContainerRef::Service)
            .chain(self.hook_containers.iter().map(ContainerRef::Hook))
    }

    /// Service Containers for Start; both roles for Stop and Remove.
    pub fn containers_for(
        &self,
        action: ContainerAction,
    ) -> impl Iterator<Item = ContainerRef<'_>> {
        let hooks = match action {
            ContainerAction::Start => &[][..],
            ContainerAction::Stop | ContainerAction::Remove => self.hook_containers.as_slice(),
        };
        self.containers
            .iter()
            .map(ContainerRef::Service)
            .chain(hooks.iter().map(ContainerRef::Hook))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerAction {
    Start,
    Stop,
    Remove,
}

/// Entry-relative Live Observations: the container PartialResult.
/// Service grouping is derived on demand; there is no completeness flag.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LiveServices<E> {
    pub containers: PartialResult<Vec<ContainerObservation>, E>,
}

impl<E> LiveServices<E> {
    /// Observer-derived Service groupings from successful container observations.
    #[must_use]
    pub fn services(&self) -> Vec<ServiceObservation> {
        derive_services(
            self.containers
                .successes
                .iter()
                .flat_map(|success| success.value.iter().cloned()),
        )
    }
}

#[must_use]
pub fn derive_live_services<E>(
    containers: PartialResult<Vec<ContainerObservation>, E>,
) -> LiveServices<E> {
    LiveServices { containers }
}

#[must_use]
pub fn derive_services(
    containers: impl IntoIterator<Item = ContainerObservation>,
) -> Vec<ServiceObservation> {
    let mut services = BTreeMap::<QualifiedService, ServiceObservation>::new();
    for observation in containers {
        let identity = observation.identity();
        let service = services
            .entry(identity.clone())
            .or_insert_with(|| ServiceObservation {
                identity,
                service_id: observation.service_id,
                containers: Vec::new(),
                hook_containers: Vec::new(),
            });
        match Container::from(observation) {
            Container::Service(container) => service.containers.push(container),
            Container::Hook(container) => service.hook_containers.push(container),
        }
    }
    for service in services.values_mut() {
        if let Some(newest) = service.members().max_by_key(|container| {
            let observation = container.as_observation();
            (
                observation.created_at_unix_nanos,
                observation.container_id.as_str(),
            )
        }) {
            service.service_id = newest.as_observation().service_id;
        }
    }
    services.into_values().collect()
}

/// Role-proven Service Containers from mixed wire observations. Hook Containers are dropped.
#[must_use]
pub fn service_containers(
    observations: impl IntoIterator<Item = ContainerObservation>,
) -> Vec<ServiceContainer> {
    observations
        .into_iter()
        .filter_map(|observation| match Container::from(observation) {
            Container::Service(container) => Some(container),
            Container::Hook(_) => None,
        })
        .collect()
}

/// Serving Containers: Service Containers that are healthy and have a Container Address.
#[must_use]
pub fn serving_replicas<'a>(
    containers: impl IntoIterator<Item = &'a ServiceContainer>,
) -> Vec<&'a ServiceContainer> {
    containers
        .into_iter()
        .filter(|container| {
            let observation = container.as_observation();
            observation.runtime.is_healthy() && observation.address.is_some()
        })
        .collect()
}

#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "error")]
pub enum ServiceSelectorError {
    #[error("Service \"{selector}\" was not found")]
    NotFound { selector: ServiceSelector },
    #[error(
        "Service Name \"{selector}\" matches multiple Services: {}",
        slash_list(.matches)
    )]
    NameAmbiguity {
        selector: ServiceSelector,
        matches: Vec<QualifiedService>,
    },
}

fn slash_list(matches: &[QualifiedService]) -> String {
    matches
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Resolve a Service by exact Service ID, then Qualified Service, then unique Service Name.
///
/// # Errors
///
/// Returns [`ServiceSelectorError::NotFound`] when no Service matches, or
/// [`ServiceSelectorError::NameAmbiguity`] when a short name matches more than one
/// Qualified Service.
pub fn select_service<'a>(
    services: &'a [ServiceObservation],
    selector: &ServiceSelector,
) -> Result<&'a ServiceObservation, ServiceSelectorError> {
    if let Ok(service_id) = ServiceId::parse(selector.as_str())
        && let Some(service) = services
            .iter()
            .find(|service| service.has_service_id(&service_id))
    {
        return Ok(service);
    }
    if let Ok(identity) = QualifiedService::parse(selector.as_str()) {
        return services
            .iter()
            .find(|service| service.identity == identity)
            .ok_or_else(|| ServiceSelectorError::NotFound {
                selector: selector.clone(),
            });
    }
    let mut matches = services
        .iter()
        .filter(|service| service.has_name(selector.as_str()))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.identity.cmp(&right.identity));
    match matches.as_slice() {
        [] => Err(ServiceSelectorError::NotFound {
            selector: selector.clone(),
        }),
        [service] => Ok(service),
        _ => Err(ServiceSelectorError::NameAmbiguity {
            selector: selector.clone(),
            matches: matches
                .into_iter()
                .map(|service| service.identity.clone())
                .collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use crate::{
        ContainerAddress, ContainerId, ContainerKind, ContainerObservation, ContainerRef,
        ContainerRuntimeObservation, HealthObservation, MachineFailure, MachineId, MachineSuccess,
        PartialResult, ProjectName, QualifiedService, ResolvedServiceSpec, RpcError, RpcErrorCode,
        ServiceContainer, ServiceId, ServiceName, ServiceSelector,
    };

    #[test]
    fn serving_replicas_are_healthy_addressed_service_containers() {
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

        let containers = super::service_containers([
            healthy.clone(),
            not_configured.clone(),
            hook,
            starting,
            unhealthy,
            no_address,
            exited,
        ]);
        let serving = super::serving_replicas(&containers);

        assert_eq!(
            serving
                .into_iter()
                .map(ServiceContainer::as_observation)
                .collect::<Vec<_>>(),
            vec![&healthy, &not_configured]
        );
    }

    #[test]
    fn mixed_wire_observations_convert_to_service_containers_without_hooks() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let regular = observation(
            '1',
            &service_id,
            "api",
            ContainerKind::ServiceContainer,
            "v1",
        );
        let hook = observation(
            '2',
            &service_id,
            "api",
            ContainerKind::PreDeployHook,
            "hook",
        );

        let containers = super::service_containers([regular.clone(), hook]);

        assert_eq!(
            containers
                .iter()
                .map(ServiceContainer::as_observation)
                .collect::<Vec<_>>(),
            vec![&regular]
        );
    }

    #[test]
    fn services_are_derived_in_one_pass_without_losing_history_or_hooks() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let regular = observation(
            '1',
            &service_id,
            "api",
            ContainerKind::ServiceContainer,
            "v1",
        );
        let changed = observation(
            '2',
            &service_id,
            "api",
            ContainerKind::ServiceContainer,
            "v2",
        );
        let hook = observation(
            '3',
            &service_id,
            "api",
            ContainerKind::PreDeployHook,
            "hook",
        );
        let hook_only_id = ServiceId::parse("b".repeat(32)).unwrap();
        let hook_only = observation(
            '4',
            &hook_only_id,
            "worker",
            ContainerKind::PreDeployHook,
            "hook-only",
        );

        let services = super::derive_services([regular, changed, hook, hook_only]);

        assert_eq!(services.len(), 2);
        let service = services
            .iter()
            .find(|service| service.service_id == service_id)
            .unwrap();
        assert_eq!(service.containers.len(), 2);
        assert_eq!(service.hook_containers.len(), 1);
        assert_ne!(
            service
                .containers
                .first()
                .unwrap()
                .as_observation()
                .resolved_spec,
            service
                .containers
                .get(1)
                .unwrap()
                .as_observation()
                .resolved_spec
        );
        let hook_only = services
            .iter()
            .find(|service| service.service_id == hook_only_id)
            .unwrap();
        assert!(hook_only.containers.is_empty());
        assert_eq!(hook_only.hook_containers.len(), 1);
    }

    #[test]
    fn service_selectors_resolve_qualified_names_and_unique_short_names() {
        let staging_id = ServiceId::parse("a".repeat(32)).unwrap();
        let prod_id = ServiceId::parse("b".repeat(32)).unwrap();
        let collision_id = ServiceId::parse("c".repeat(32)).unwrap();
        let unique_id = ServiceId::parse("d".repeat(32)).unwrap();
        let leftover_id = ServiceId::parse("e".repeat(32)).unwrap();
        let services = super::derive_services([
            in_project(
                observation(
                    '1',
                    &staging_id,
                    "web",
                    ContainerKind::ServiceContainer,
                    "v1",
                ),
                "shop-staging",
            ),
            in_project(
                observation('2', &prod_id, "web", ContainerKind::ServiceContainer, "v2"),
                "shop-prod",
            ),
            observation(
                '3',
                &collision_id,
                staging_id.as_str(),
                ContainerKind::ServiceContainer,
                "collision",
            ),
            observation(
                '4',
                &unique_id,
                "unique",
                ContainerKind::ServiceContainer,
                "unique",
            ),
            in_project(
                observation(
                    '5',
                    &leftover_id,
                    "web",
                    ContainerKind::ServiceContainer,
                    "old",
                ),
                "shop-staging",
            ),
        ]);

        assert_eq!(services.len(), 4);
        let staging_web = QualifiedService::parse("shop-staging/web").unwrap();
        assert_eq!(
            super::select_service(
                &services,
                &ServiceSelector::parse("shop-staging/web").unwrap()
            )
            .unwrap()
            .identity,
            staging_web
        );
        assert_eq!(
            super::select_service(&services, &ServiceSelector::from(&staging_id))
                .unwrap()
                .identity,
            staging_web
        );
        assert_eq!(
            super::select_service(&services, &ServiceSelector::from(&leftover_id))
                .unwrap()
                .identity,
            staging_web
        );
        assert_eq!(
            super::select_service(&services, &ServiceSelector::parse("unique").unwrap())
                .unwrap()
                .service_id,
            unique_id
        );
        assert_eq!(
            super::select_service(&services, &ServiceSelector::from(&staging_id))
                .unwrap()
                .service_id,
            leftover_id
        );
        assert!(matches!(
            super::select_service(&services, &ServiceSelector::parse("missing").unwrap()),
            Err(super::ServiceSelectorError::NotFound { selector })
                if selector.as_str() == "missing"
        ));
        assert!(matches!(
            super::select_service(&services, &ServiceSelector::parse("shop-prod/missing").unwrap()),
            Err(super::ServiceSelectorError::NotFound { selector })
                if selector.as_str() == "shop-prod/missing"
        ));
        let error = super::select_service(&services, &ServiceSelector::parse("web").unwrap())
            .expect_err("short name web is ambiguous");
        assert_eq!(
            error.to_string(),
            "Service Name \"web\" matches multiple Services: shop-prod/web, shop-staging/web"
        );
        assert!(matches!(
            error,
            super::ServiceSelectorError::NameAmbiguity { selector, matches }
                if selector.as_str() == "web"
                    && matches
                        == [
                            QualifiedService::parse("shop-prod/web").unwrap(),
                            QualifiedService::parse("shop-staging/web").unwrap(),
                        ]
        ));
        assert!(ServiceSelector::parse("").is_err());
        assert!(ServiceSelector::parse("SHOP/web").is_err());
        assert!(ServiceSelector::parse("shop/web/extra").is_err());
    }

    #[test]
    fn partial_live_results_keep_every_machine_outcome_and_only_derive_successes() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let success_id = MachineId::parse("1".repeat(32)).unwrap();
        let failed_id = MachineId::parse("2".repeat(32)).unwrap();
        let omitted_id = MachineId::parse("3".repeat(32)).unwrap();
        let partial = PartialResult {
            successes: vec![MachineSuccess {
                machine_id: success_id,
                value: vec![observation(
                    '1',
                    &service_id,
                    "api",
                    ContainerKind::ServiceContainer,
                    "v1",
                )],
            }],
            failures: vec![MachineFailure {
                machine_id: failed_id,
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "offline".into(),
                    details: serde_json::Value::Null,
                },
            }],
            omissions: vec![omitted_id],
        };

        let live = super::derive_live_services(partial);
        let services = live.services();

        assert_eq!(services.len(), 1);
        assert_eq!(
            services
                .first()
                .unwrap()
                .containers
                .first()
                .unwrap()
                .as_observation()
                .machine_id,
            success_id
        );
        assert_eq!(
            live.containers.failures.first().unwrap().machine_id,
            failed_id
        );
        assert_eq!(live.containers.omissions, vec![omitted_id]);
    }

    #[test]
    fn mutating_containers_updates_derived_services() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let first = observation(
            '1',
            &service_id,
            "api",
            ContainerKind::ServiceContainer,
            "v1",
        );
        let second = observation(
            '2',
            &service_id,
            "api",
            ContainerKind::ServiceContainer,
            "v2",
        );
        let mut live = super::derive_live_services::<()>(PartialResult {
            successes: vec![MachineSuccess {
                machine_id: first.machine_id,
                value: vec![first.clone()],
            }],
            failures: Vec::new(),
            omissions: Vec::new(),
        });

        assert_eq!(live.services().len(), 1);
        assert_eq!(live.services().first().unwrap().containers.len(), 1);

        live.containers
            .successes
            .first_mut()
            .unwrap()
            .value
            .push(second.clone());

        let services = live.services();
        assert_eq!(services.len(), 1);
        assert_eq!(services.first().unwrap().containers.len(), 2);
        assert_eq!(
            services
                .first()
                .unwrap()
                .containers
                .get(1)
                .unwrap()
                .as_observation()
                .container_id,
            second.container_id
        );

        live.containers.successes.clear();
        assert!(live.services().is_empty());
    }

    #[test]
    fn live_services_json_has_no_completeness_flag_when_every_target_succeeds() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let live = super::derive_live_services::<()>(PartialResult {
            successes: vec![MachineSuccess {
                machine_id: MachineId::parse("1".repeat(32)).unwrap(),
                value: vec![observation(
                    '1',
                    &service_id,
                    "api",
                    ContainerKind::ServiceContainer,
                    "v1",
                )],
            }],
            failures: Vec::new(),
            omissions: Vec::new(),
        });

        assert!(live.containers.all_targets_succeeded());
        let json = serde_json::to_value(&live).unwrap();
        assert_eq!(json.get("complete"), None);
        assert_eq!(json.get("services"), None);
    }

    #[test]
    fn derived_service_observations_reject_cross_role_collections() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let regular = observation(
            '1',
            &service_id,
            "api",
            ContainerKind::ServiceContainer,
            "v1",
        );
        let hook = observation(
            '2',
            &service_id,
            "api",
            ContainerKind::PreDeployHook,
            "hook",
        );

        assert!(
            serde_json::from_value::<super::ServiceObservation>(json!({
                "service_id": service_id,
                "containers": [hook],
                "hook_containers": []
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<super::ServiceObservation>(json!({
                "service_id": service_id,
                "containers": [],
                "hook_containers": [regular]
            }))
            .is_err()
        );

        let derived = super::derive_services([regular, hook]).pop().unwrap();
        let inspected = serde_json::to_value(&derived).unwrap();
        assert_eq!(inspected.get("service_id"), Some(&json!(service_id)));
        assert_eq!(inspected.get("identity"), Some(&json!("app/api")));
        assert_eq!(
            inspected.pointer("/containers/0/kind"),
            Some(&json!("service_container"))
        );
        assert_eq!(
            inspected.pointer("/hook_containers/0/kind"),
            Some(&json!("pre_deploy_hook"))
        );
        for pointer in [
            "/containers/0/container_id",
            "/containers/0/display_name",
            "/containers/0/machine_id",
            "/containers/0/service_id",
            "/containers/0/service_name",
            "/containers/0/runtime",
            "/containers/0/resolved_spec",
            "/hook_containers/0/container_id",
            "/hook_containers/0/kind",
            "/hook_containers/0/resolved_spec",
        ] {
            assert!(
                inspected.pointer(pointer).is_some(),
                "inspect JSON missing {pointer}"
            );
        }
        assert_eq!(
            serde_json::from_value::<super::ServiceObservation>(inspected).unwrap(),
            derived
        );
    }

    #[test]
    fn start_excludes_hooks_while_stop_and_remove_include_them() {
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let service = super::derive_services([
            observation(
                '1',
                &service_id,
                "api",
                ContainerKind::ServiceContainer,
                "v1",
            ),
            observation(
                '2',
                &service_id,
                "api",
                ContainerKind::PreDeployHook,
                "hook",
            ),
        ])
        .pop()
        .unwrap();

        let start = service
            .containers_for(super::ContainerAction::Start)
            .collect::<Vec<_>>();
        assert_eq!(
            start,
            service
                .containers
                .iter()
                .map(ContainerRef::Service)
                .collect::<Vec<_>>()
        );
        let stop = service
            .containers_for(super::ContainerAction::Stop)
            .collect::<Vec<_>>();
        let both = service.members().collect::<Vec<_>>();
        assert_eq!(stop, both);
        assert!(matches!(stop.first(), Some(ContainerRef::Service(_))));
        assert!(matches!(stop.get(1), Some(ContainerRef::Hook(_))));
        assert_eq!(
            service
                .containers_for(super::ContainerAction::Remove)
                .collect::<Vec<_>>(),
            both
        );

        let changed = PartialResult {
            successes: vec![MachineSuccess {
                machine_id: MachineId::parse("1".repeat(32)).unwrap(),
                value: ContainerId::parse("1".repeat(64)).unwrap(),
            }],
            failures: vec![MachineFailure {
                machine_id: MachineId::parse("2".repeat(32)).unwrap(),
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "partitioned".into(),
                    details: serde_json::Value::Null,
                },
            }],
            omissions: Vec::new(),
        };
        assert_eq!(changed.successes.len(), 1);
        assert_eq!(changed.failures.len(), 1);
        assert!(!changed.all_targets_succeeded());
    }

    fn serving_observation(
        id: char,
        service_id: &ServiceId,
        kind: ContainerKind,
        runtime: ContainerRuntimeObservation,
        address: Option<[u8; 4]>,
    ) -> ContainerObservation {
        let mut observation = observation(id, service_id, "api", kind, "api");
        observation.runtime = runtime;
        observation.address = address.map(|octets| ContainerAddress(octets.into()));
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
        ContainerObservation {
            container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: format!("{name}-{id}"),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
            project_name: ProjectName::parse("app").unwrap(),
            service_id: *service_id,
            service_name,
            kind,
            runtime: ContainerRuntimeObservation::Created,
            effective_healthcheck: None,
            resolved_spec,
            address: None,
            labels: BTreeMap::new(),
        }
    }

    fn in_project(mut observation: ContainerObservation, project: &str) -> ContainerObservation {
        observation.project_name = ProjectName::parse(project).unwrap();
        observation
    }
}

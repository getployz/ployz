use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Container, ContainerObservation, ContainerRef, HookContainer, PartialResult, ServiceContainer,
    ServiceId, ServiceName, ServiceSelector,
};

/// One observer-derived grouping. Every container keeps its own historical spec.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServiceObservation {
    pub service_id: ServiceId,
    #[serde(default)]
    pub containers: Vec<ServiceContainer>,
    #[serde(default)]
    pub hook_containers: Vec<HookContainer>,
}

impl ServiceObservation {
    /// Service Name carried by any member, if the observation is non-empty.
    #[must_use]
    pub fn service_name(&self) -> Option<&ServiceName> {
        self.members()
            .next()
            .map(|container| &container.as_observation().service_name)
    }

    /// True when any member carries this Service Name.
    #[must_use]
    pub fn has_name(&self, selector: &str) -> bool {
        self.members()
            .any(|container| container.as_observation().service_name.as_str() == selector)
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

/// Entry-relative Live Observations and the Machine outcomes that produced them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LiveServices<E> {
    pub services: Vec<ServiceObservation>,
    pub containers: PartialResult<Vec<ContainerObservation>, E>,
    /// Live Observation is never a proof of global completeness.
    pub complete: bool,
}

#[must_use]
pub fn derive_live_services<E>(
    containers: PartialResult<Vec<ContainerObservation>, E>,
) -> LiveServices<E> {
    let services = derive_services(
        containers
            .successes
            .iter()
            .flat_map(|success| success.value.iter().cloned()),
    );
    LiveServices {
        services,
        containers,
        complete: false,
    }
}

#[must_use]
pub fn derive_services(
    containers: impl IntoIterator<Item = ContainerObservation>,
) -> Vec<ServiceObservation> {
    let mut services = BTreeMap::<ServiceId, ServiceObservation>::new();
    for observation in containers {
        let service =
            services
                .entry(observation.service_id)
                .or_insert_with(|| ServiceObservation {
                    service_id: observation.service_id,
                    containers: Vec::new(),
                    hook_containers: Vec::new(),
                });
        match Container::from(observation) {
            Container::Service(container) => service.containers.push(container),
            Container::Hook(container) => service.hook_containers.push(container),
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

#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "error")]
pub enum ServiceSelectorError {
    #[error("Service \"{selector}\" was not found")]
    NotFound { selector: ServiceSelector },
    #[error("Service Name \"{selector}\" matches multiple Service IDs: {service_ids:?}")]
    NameAmbiguity {
        selector: ServiceSelector,
        service_ids: Vec<ServiceId>,
    },
}

/// Resolve a Service by exact Service ID, then Service Name.
///
/// # Errors
///
/// Returns [`ServiceSelectorError::NotFound`] when no Service matches, or
/// [`ServiceSelectorError::NameAmbiguity`] when a name matches more than one Service ID.
pub fn select_service<'a>(
    services: &'a [ServiceObservation],
    selector: &ServiceSelector,
) -> Result<&'a ServiceObservation, ServiceSelectorError> {
    // TODO(UT-103): same-named Service IDs remain ambiguous; never select or repair a winner.
    if let Some(service) = services
        .iter()
        .find(|service| service.service_id.as_str() == selector.as_str())
    {
        return Ok(service);
    }
    let matches = services
        .iter()
        .filter(|service| service.has_name(selector.as_str()))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(ServiceSelectorError::NotFound {
            selector: selector.clone(),
        }),
        [service] => Ok(service),
        _ => Err(ServiceSelectorError::NameAmbiguity {
            selector: selector.clone(),
            service_ids: matches
                .into_iter()
                .map(|service| service.service_id)
                .collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use crate::{
        ContainerId, ContainerKind, ContainerObservation, ContainerRef,
        ContainerRuntimeObservation, MachineFailure, MachineId, MachineSuccess, PartialResult,
        ResolvedServiceSpec, RpcError, RpcErrorCode, ServiceContainer, ServiceId, ServiceName,
        ServiceSelector,
    };

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
    fn service_selectors_prioritize_ids_and_report_every_ambiguous_name_match() {
        let first_id = ServiceId::parse("a".repeat(32)).unwrap();
        let second_id = ServiceId::parse("b".repeat(32)).unwrap();
        let collision_id = ServiceId::parse("c".repeat(32)).unwrap();
        let unique_id = ServiceId::parse("d".repeat(32)).unwrap();
        let services = super::derive_services([
            observation(
                '1',
                &first_id,
                "shared",
                ContainerKind::ServiceContainer,
                "v1",
            ),
            observation(
                '2',
                &second_id,
                "shared",
                ContainerKind::ServiceContainer,
                "v2",
            ),
            observation(
                '3',
                &collision_id,
                first_id.as_str(),
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
        ]);

        assert_eq!(
            super::select_service(&services, &ServiceSelector::from(&first_id))
                .unwrap()
                .service_id,
            first_id
        );
        assert!(matches!(
            super::select_service(&services, &ServiceSelector::parse("missing").unwrap()),
            Err(super::ServiceSelectorError::NotFound { selector })
                if selector.as_str() == "missing"
        ));
        assert_eq!(
            super::select_service(&services, &ServiceSelector::parse("unique").unwrap())
                .unwrap()
                .service_id,
            unique_id
        );
        assert!(matches!(
            super::select_service(&services, &ServiceSelector::parse("shared").unwrap()),
            Err(super::ServiceSelectorError::NameAmbiguity { selector, service_ids })
                if selector.as_str() == "shared"
                    && service_ids.len() == 2
                    && service_ids.contains(&first_id)
                    && service_ids.contains(&second_id)
        ));
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

        assert!(!live.complete);
        assert_eq!(live.services.len(), 1);
        assert_eq!(
            live.services
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
}

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ContainerKind, ContainerObservation, PartialResult, ServiceId};

/// One observer-derived grouping. Every container keeps its own historical spec.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServiceObservation {
    pub service_id: ServiceId,
    #[serde(default)]
    pub containers: Vec<ContainerObservation>,
    #[serde(default)]
    pub hook_containers: Vec<ContainerObservation>,
}

impl ServiceObservation {
    pub fn containers_for(
        &self,
        action: ContainerAction,
    ) -> impl Iterator<Item = &ContainerObservation> {
        self.containers
            .iter()
            .chain(&self.hook_containers)
            .filter(move |container| {
                action != ContainerAction::Start
                    || container.kind == ContainerKind::ServiceContainer
            })
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
    for container in containers {
        let service = services
            .entry(container.service_id.clone())
            .or_insert_with(|| ServiceObservation {
                service_id: container.service_id.clone(),
                containers: Vec::new(),
                hook_containers: Vec::new(),
            });
        match container.kind {
            ContainerKind::ServiceContainer => service.containers.push(container),
            ContainerKind::PreDeployHook => service.hook_containers.push(container),
        }
    }
    services.into_values().collect()
}

#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "error")]
pub enum ServiceSelectorError {
    #[error("Service {selector:?} was not found")]
    NotFound { selector: String },
    #[error("Service Name {selector:?} matches multiple Service IDs: {service_ids:?}")]
    NameAmbiguity {
        selector: String,
        service_ids: Vec<ServiceId>,
    },
}

pub fn select_service<'a>(
    services: &'a [ServiceObservation],
    selector: &str,
) -> Result<&'a ServiceObservation, ServiceSelectorError> {
    // TODO(UT-103): same-named Service IDs remain ambiguous; never select or repair a winner.
    if let Some(service) = services
        .iter()
        .find(|service| service.service_id.as_str() == selector)
    {
        return Ok(service);
    }
    let matches = services
        .iter()
        .filter(|service| {
            service
                .containers
                .iter()
                .chain(&service.hook_containers)
                .any(|container| container.service_name.as_str() == selector)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(ServiceSelectorError::NotFound {
            selector: selector.to_owned(),
        }),
        [service] => Ok(service),
        _ => Err(ServiceSelectorError::NameAmbiguity {
            selector: selector.to_owned(),
            service_ids: matches
                .into_iter()
                .map(|service| service.service_id.clone())
                .collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use crate::{
        ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
        MachineFailure, MachineId, MachineSuccess, PartialResult, ResolvedServiceSpec, RpcError,
        RpcErrorCode, ServiceId, ServiceName,
    };

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
            service.containers.first().unwrap().resolved_spec,
            service.containers.get(1).unwrap().resolved_spec
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
            super::select_service(&services, first_id.as_str())
                .unwrap()
                .service_id,
            first_id
        );
        assert!(matches!(
            super::select_service(&services, "missing"),
            Err(super::ServiceSelectorError::NotFound { .. })
        ));
        assert_eq!(
            super::select_service(&services, "unique")
                .unwrap()
                .service_id,
            unique_id
        );
        assert!(matches!(
            super::select_service(&services, "shared"),
            Err(super::ServiceSelectorError::NameAmbiguity { service_ids, .. })
                if service_ids.len() == 2
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
                machine_id: failed_id.clone(),
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "offline".into(),
                    details: serde_json::Value::Null,
                },
            }],
            omissions: vec![omitted_id.clone()],
        };

        let live = super::derive_live_services(partial);

        assert!(!live.complete);
        assert_eq!(live.services.len(), 1);
        assert_eq!(
            live.containers.failures.first().unwrap().machine_id,
            failed_id
        );
        assert_eq!(live.containers.omissions, vec![omitted_id]);
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

        assert_eq!(
            service
                .containers_for(super::ContainerAction::Start)
                .count(),
            1
        );
        assert_eq!(
            service.containers_for(super::ContainerAction::Stop).count(),
            2
        );
        assert_eq!(
            service
                .containers_for(super::ContainerAction::Remove)
                .count(),
            2
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
            machine_id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
            service_id: service_id.clone(),
            service_name,
            kind,
            runtime: ContainerRuntimeObservation::Created,
            resolved_spec,
            address: None,
            labels: BTreeMap::new(),
        }
    }
}

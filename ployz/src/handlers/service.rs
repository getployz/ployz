use std::collections::HashSet;

use clap::ArgMatches;
use ployz_core::{
    ContainerAction, ContainerRef, ContainerRuntimeObservation, HealthObservation, LiveServices,
    RpcError, ServiceSelector, select_service,
};

use super::{Error, leaf_matches, with_client};

pub fn list(root: &ArgMatches) -> Result<(), Error> {
    with_client(root, |client| {
        Box::pin(async move {
            let live = client.live_services().await?;
            print_observation_warning(&live);
            println!("SERVICE ID\tNAME\tCONTAINERS\tHOOKS");
            for service in &live.services {
                let name = service
                    .service_name()
                    .map(|name| name.as_str())
                    .unwrap_or("-");
                println!(
                    "{}\t{}\t{}\t{}",
                    service.service_id,
                    name,
                    service.containers.len(),
                    service.hook_containers.len()
                );
            }
            Ok(())
        })
    })
}

pub fn processes(root: &ArgMatches) -> Result<(), Error> {
    let sort = leaf_matches(root)
        .get_one::<String>("sort")
        .cloned()
        .ok_or_else(|| Error::usage("sort order is required"))?;
    with_client(root, |client| {
        Box::pin(async move {
            let live = client.live_services().await?;
            print_observation_warning(&live);
            println!("CONTAINER ID\tSERVICE\tKIND\tMACHINE\tSTATE");
            let mut containers = live
                .services
                .iter()
                .flat_map(ployz_core::ServiceObservation::members)
                .collect::<Vec<_>>();
            sort_processes(&mut containers, &sort);
            for container in containers {
                let observation = container.as_observation();
                println!(
                    "{}\t{}\t{}\t{}\t{:?}",
                    observation.container_id,
                    observation.service_name,
                    process_kind(container),
                    observation.machine_id,
                    observation.runtime
                );
            }
            Ok(())
        })
    })
}

fn sort_processes(containers: &mut [ContainerRef<'_>], sort: &str) {
    containers.sort_by(|left, right| {
        let left_observation = left.as_observation();
        let right_observation = right.as_observation();
        let primary = match sort {
            "health" => health_rank(*left).cmp(&health_rank(*right)).then_with(|| {
                left_observation
                    .service_name
                    .as_str()
                    .cmp(right_observation.service_name.as_str())
            }),
            "machine" => left_observation
                .machine_id
                .as_str()
                .cmp(right_observation.machine_id.as_str())
                .then_with(|| {
                    left_observation
                        .service_name
                        .as_str()
                        .cmp(right_observation.service_name.as_str())
                }),
            _ => left_observation
                .service_name
                .as_str()
                .cmp(right_observation.service_name.as_str()),
        };
        primary.then_with(|| {
            left_observation
                .container_id
                .as_str()
                .cmp(right_observation.container_id.as_str())
        })
    });
}

fn process_kind(container: ContainerRef<'_>) -> &'static str {
    match container {
        ContainerRef::Service(_) => "ServiceContainer",
        ContainerRef::Hook(_) => "PreDeployHook",
    }
}

fn health_rank(container: ContainerRef<'_>) -> u8 {
    if let ContainerRef::Hook(container) = container
        && matches!(
            container.as_observation().runtime,
            ContainerRuntimeObservation::Exited { code: 0 }
        )
    {
        return 3;
    }
    runtime_health_rank(&container.as_observation().runtime)
}

fn runtime_health_rank(runtime: &ContainerRuntimeObservation) -> u8 {
    match runtime {
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        }
        | ContainerRuntimeObservation::Dead => 0,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        } => 2,
        ContainerRuntimeObservation::Running { .. } => 3,
        ContainerRuntimeObservation::Created
        | ContainerRuntimeObservation::Paused
        | ContainerRuntimeObservation::Restarting
        | ContainerRuntimeObservation::Exited { .. }
        | ContainerRuntimeObservation::Removing
        | ContainerRuntimeObservation::Unknown { .. } => 1,
    }
}

pub fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let selector = ServiceSelector::parse(
        leaf_matches(root)
            .get_one::<String>("service")
            .cloned()
            .ok_or_else(|| Error::usage("Service selector is required"))?,
    )?;
    with_client(root, |client| {
        Box::pin(async move {
            let live = client.live_services().await?;
            print_observation_warning(&live);
            let service = select_service(&live.services, &selector)?;
            println!("{}", serde_json::to_string_pretty(service)?);
            Ok(())
        })
    })
}

pub fn change(root: &ArgMatches, action: ContainerAction) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let selectors = leaf
        .get_many::<String>("service")
        .ok_or_else(|| Error::usage("at least one Service selector is required"))?
        .map(|selector| ServiceSelector::parse(selector.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let (signal, timeout) = stop_options(leaf, action)?;
    with_client(root, |client| {
        Box::pin(async move {
            let live = client.live_services().await?;
            print_observation_warning(&live);
            let services = select_services(&live.services, &selectors)?;
            let mut partial = false;
            for service in services {
                let outcomes = client
                    .change_observed_service(service, action, signal.clone(), timeout)
                    .await;
                for success in outcomes.successes {
                    println!("{:?}\t{}\t{}", action, success.machine_id, success.value);
                }
                for failure in outcomes.failures {
                    eprintln!(
                        "WARNING: {:?} failed for {} on {}: {}",
                        action,
                        failure.error.container_id,
                        failure.machine_id,
                        failure.error.error.message
                    );
                    partial = true;
                }
            }
            if !live.containers.all_targets_succeeded() {
                eprintln!("WARNING: the Service selection came from a partial Live Observation");
                partial = true;
            }
            if partial {
                Err(Error::usage("Service lifecycle completed partially"))
            } else {
                Ok(())
            }
        })
    })
}

fn select_services<'a>(
    services: &'a [ployz_core::ServiceObservation],
    selectors: &[ServiceSelector],
) -> Result<Vec<&'a ployz_core::ServiceObservation>, Error> {
    let mut ids = HashSet::new();
    let mut selected = Vec::new();
    for selector in selectors {
        let service = select_service(services, selector)?;
        if ids.insert(service.service_id) {
            selected.push(service);
        }
    }
    Ok(selected)
}

fn stop_options(
    matches: &ArgMatches,
    action: ContainerAction,
) -> Result<(Option<String>, Option<i32>), Error> {
    if action != ContainerAction::Stop {
        return Ok((None, None));
    }
    let signal = matches.get_one::<String>("signal").cloned();
    let timeout = matches
        .get_one::<String>("timeout")
        .map(|value| value.parse::<i32>())
        .transpose()?;
    Ok((signal, timeout))
}

fn print_observation_warning(live: &LiveServices<RpcError>) {
    eprintln!("WARNING: Live Observation is observer-relative and not globally complete");
    for failure in &live.containers.failures {
        eprintln!(
            "WARNING: Machine {} failed: {}",
            failure.machine_id, failure.error.message
        );
    }
    for machine_id in &live.containers.omissions {
        eprintln!("WARNING: Machine {machine_id} was omitted");
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{HookContainer, MachineId, ServiceContainer, ServiceId, ServiceName};
    use serde_json::json;

    use super::*;

    #[test]
    fn process_sort_orders_match_the_cli_contract() {
        let beta = observation(
            'b',
            'b',
            "beta",
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
        );
        let alpha = observation(
            'a',
            'c',
            "alpha",
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Unhealthy,
            },
        );
        let gamma = observation('c', 'a', "gamma", ContainerRuntimeObservation::Created);
        let hook = hook_observation(
            'd',
            'd',
            "delta",
            ContainerRuntimeObservation::Exited { code: 0 },
        );

        let beta = ServiceContainer::try_from(beta).unwrap();
        let alpha = ServiceContainer::try_from(alpha).unwrap();
        let gamma = ServiceContainer::try_from(gamma).unwrap();
        let hook = HookContainer::try_from(hook).unwrap();
        let mut containers = vec![
            ContainerRef::Service(&beta),
            ContainerRef::Service(&alpha),
            ContainerRef::Service(&gamma),
            ContainerRef::Hook(&hook),
        ];
        sort_processes(&mut containers, "service");
        assert_eq!(names(&containers), ["alpha", "beta", "delta", "gamma"]);
        sort_processes(&mut containers, "machine");
        assert_eq!(names(&containers), ["gamma", "beta", "alpha", "delta"]);
        sort_processes(&mut containers, "health");
        assert_eq!(names(&containers), ["alpha", "gamma", "beta", "delta"]);
    }

    #[test]
    fn stop_options_are_only_read_for_stop_actions() {
        for (command, action) in [
            ("start", ContainerAction::Start),
            ("rm", ContainerAction::Remove),
        ] {
            let matches = crate::cli::command()
                .try_get_matches_from(["ployz", command, "api"])
                .unwrap();
            assert_eq!(
                stop_options(leaf_matches(&matches), action).unwrap(),
                (None, None)
            );
        }

        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "stop", "api"])
            .unwrap();
        assert_eq!(
            stop_options(leaf_matches(&matches), ContainerAction::Stop).unwrap(),
            (Some("SIGTERM".into()), Some(10))
        );
    }

    #[test]
    fn lifecycle_selectors_deduplicate_names_and_ids() {
        let container = observation('a', 'a', "api", ContainerRuntimeObservation::Created);
        let service_id = container.service_id;
        let services = vec![ployz_core::ServiceObservation {
            service_id,
            containers: vec![ServiceContainer::try_from(container).unwrap()],
            hook_containers: Vec::new(),
        }];
        let selectors = vec![
            ServiceSelector::parse("api").unwrap(),
            ServiceSelector::from(&service_id),
        ];

        assert_eq!(select_services(&services, &selectors).unwrap().len(), 1);
    }

    fn names<'a>(containers: &'a [ContainerRef<'a>]) -> Vec<&'a str> {
        containers
            .iter()
            .map(|container| container.as_observation().service_name.as_str())
            .collect()
    }

    fn hook_observation(
        id: char,
        machine: char,
        name: &str,
        runtime: ContainerRuntimeObservation,
    ) -> ployz_core::ContainerObservation {
        let mut observation = observation(id, machine, name, runtime);
        observation.kind = ployz_core::ContainerKind::PreDeployHook;
        observation
    }

    fn observation(
        id: char,
        machine: char,
        name: &str,
        runtime: ContainerRuntimeObservation,
    ) -> ployz_core::ContainerObservation {
        let service_id = ServiceId::parse(id.to_string().repeat(32)).unwrap();
        let service_name = ServiceName::parse(name).unwrap();
        ployz_core::ContainerObservation {
            container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: name.into(),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
            service_id,
            service_name: service_name.clone(),
            kind: ployz_core::ContainerKind::ServiceContainer,
            runtime,
            effective_healthcheck: None,
            resolved_spec: serde_json::from_value(json!({
                "service_id": service_id,
                "name": service_name,
                "mode": { "mode": "replicated", "replicas": 1 },
                "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
            }))
            .unwrap(),
            address: None,
            labels: Default::default(),
        }
    }
}

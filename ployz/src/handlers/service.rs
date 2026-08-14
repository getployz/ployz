use std::{collections::HashSet, future::Future, path::Path, pin::Pin};

use clap::ArgMatches;
use ployz_core::{
    ContainerAction, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
    HealthObservation, LiveServices, RpcError, select_service,
};

use super::{Error, leaf_matches};

pub fn list(root: &ArgMatches) -> Result<(), Error> {
    with_client(root, |client| {
        Box::pin(async move {
            let live = client
                .live_services()
                .await
                .map_err(|error| error.to_string())?;
            print_observation_warning(&live);
            println!("SERVICE ID\tNAME\tCONTAINERS\tHOOKS");
            for service in &live.services {
                let name = service
                    .containers
                    .iter()
                    .chain(&service.hook_containers)
                    .next()
                    .map(|container| container.service_name.as_str())
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
        .ok_or_else(|| "sort order is required".to_owned())?;
    with_client(root, |client| {
        Box::pin(async move {
            let live = client
                .live_services()
                .await
                .map_err(|error| error.to_string())?;
            print_observation_warning(&live);
            println!("CONTAINER ID\tSERVICE\tKIND\tMACHINE\tSTATE");
            let mut containers = live
                .services
                .iter()
                .flat_map(|service| service.containers.iter().chain(&service.hook_containers))
                .collect::<Vec<_>>();
            sort_processes(&mut containers, &sort);
            for container in containers {
                println!(
                    "{}\t{}\t{:?}\t{}\t{:?}",
                    container.container_id,
                    container.service_name,
                    container.kind,
                    container.machine_id,
                    container.runtime
                );
            }
            Ok(())
        })
    })
}

fn sort_processes(containers: &mut [&ContainerObservation], sort: &str) {
    containers.sort_by(|left, right| {
        let primary = match sort {
            "health" => health_rank(left)
                .cmp(&health_rank(right))
                .then_with(|| left.service_name.as_str().cmp(right.service_name.as_str())),
            "machine" => left
                .machine_id
                .as_str()
                .cmp(right.machine_id.as_str())
                .then_with(|| left.service_name.as_str().cmp(right.service_name.as_str())),
            _ => left.service_name.as_str().cmp(right.service_name.as_str()),
        };
        primary.then_with(|| left.container_id.as_str().cmp(right.container_id.as_str()))
    });
}

fn health_rank(container: &ContainerObservation) -> u8 {
    match &container.runtime {
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        }
        | ContainerRuntimeObservation::Dead => 0,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        } => 2,
        ContainerRuntimeObservation::Running { .. } => 3,
        ContainerRuntimeObservation::Exited { code: 0 }
            if container.kind == ContainerKind::PreDeployHook =>
        {
            3
        }
        ContainerRuntimeObservation::Created
        | ContainerRuntimeObservation::Paused
        | ContainerRuntimeObservation::Restarting
        | ContainerRuntimeObservation::Exited { .. }
        | ContainerRuntimeObservation::Removing
        | ContainerRuntimeObservation::Unknown { .. } => 1,
    }
}

pub fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let selector = leaf_matches(root)
        .get_one::<String>("service")
        .cloned()
        .ok_or_else(|| "Service selector is required".to_owned())?;
    with_client(root, |client| {
        Box::pin(async move {
            let live = client
                .live_services()
                .await
                .map_err(|error| error.to_string())?;
            print_observation_warning(&live);
            let service =
                select_service(&live.services, &selector).map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(service).map_err(|error| error.to_string())?
            );
            Ok(())
        })
    })
}

pub fn change(root: &ArgMatches, action: ContainerAction) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let selectors = leaf
        .get_many::<String>("service")
        .ok_or_else(|| "at least one Service selector is required".to_owned())?
        .cloned()
        .collect::<Vec<_>>();
    let (signal, timeout) = stop_options(leaf, action)?;
    with_client(root, |client| {
        Box::pin(async move {
            let live = client
                .live_services()
                .await
                .map_err(|error| error.to_string())?;
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
                Err("Service lifecycle completed partially".into())
            } else {
                Ok(())
            }
        })
    })
}

fn select_services<'a>(
    services: &'a [ployz_core::ServiceObservation],
    selectors: &[String],
) -> Result<Vec<&'a ployz_core::ServiceObservation>, Error> {
    let mut ids = HashSet::new();
    let mut selected = Vec::new();
    for selector in selectors {
        let service = select_service(services, selector).map_err(|error| error.to_string())?;
        if ids.insert(service.service_id.clone()) {
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
        .map(|value| value.parse::<i32>().map_err(|error| error.to_string()))
        .transpose()?;
    Ok((signal, timeout))
}

pub(super) fn with_client<F>(root: &ArgMatches, work: F) -> Result<(), Error>
where
    F: for<'a> FnOnce(
        &'a mut crate::connect::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>,
{
    with_client_context(root, None, work)
}

pub(super) fn with_client_context<F>(
    root: &ArgMatches,
    context_override: Option<&str>,
    work: F,
) -> Result<(), Error>
where
    F: for<'a> FnOnce(
        &'a mut crate::connect::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>,
{
    let leaf = leaf_matches(root);
    let config = leaf
        .get_one::<String>("ployz-config")
        .ok_or_else(|| "Ployz config path is required".to_owned())?;
    let config = crate::context::expand_home(Path::new(config));
    let direct = leaf.get_one::<String>("connect").map(String::as_str);
    let context =
        context_override.or_else(|| leaf.get_one::<String>("context").map(String::as_str));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?
        .block_on(async {
            let mut client = crate::connect::connect(&config, direct, context)
                .await
                .map_err(|error| error.to_string())?;
            work(&mut client).await
        })
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
    use ployz_core::{MachineId, ServiceId, ServiceName};
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

        let mut containers = vec![&beta, &alpha, &gamma];
        sort_processes(&mut containers, "service");
        assert_eq!(names(&containers), ["alpha", "beta", "gamma"]);
        sort_processes(&mut containers, "machine");
        assert_eq!(names(&containers), ["gamma", "beta", "alpha"]);
        sort_processes(&mut containers, "health");
        assert_eq!(names(&containers), ["alpha", "gamma", "beta"]);
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
        let service_id = container.service_id.clone();
        let services = vec![ployz_core::ServiceObservation {
            service_id: service_id.clone(),
            containers: vec![container],
            hook_containers: Vec::new(),
        }];
        let selectors = vec!["api".to_owned(), service_id.to_string()];

        assert_eq!(select_services(&services, &selectors).unwrap().len(), 1);
    }

    fn names<'a>(containers: &'a [&ContainerObservation]) -> Vec<&'a str> {
        containers
            .iter()
            .map(|container| container.service_name.as_str())
            .collect()
    }

    fn observation(
        id: char,
        machine: char,
        name: &str,
        runtime: ContainerRuntimeObservation,
    ) -> ContainerObservation {
        let service_id = ServiceId::parse(id.to_string().repeat(32)).unwrap();
        let service_name = ServiceName::parse(name).unwrap();
        ContainerObservation {
            container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: name.into(),
            machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
            service_id: service_id.clone(),
            service_name: service_name.clone(),
            kind: ContainerKind::ServiceContainer,
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

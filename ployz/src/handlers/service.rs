use std::{future::Future, path::Path, pin::Pin};

use clap::ArgMatches;
use ployz_core::{ContainerAction, LiveServices, RpcError, select_service};

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
    with_client(root, |client| {
        Box::pin(async move {
            let live = client
                .live_services()
                .await
                .map_err(|error| error.to_string())?;
            print_observation_warning(&live);
            println!("CONTAINER ID\tSERVICE\tKIND\tMACHINE\tSTATE");
            for container in live
                .services
                .iter()
                .flat_map(|service| service.containers.iter().chain(&service.hook_containers))
            {
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
    let signal = leaf.get_one::<String>("signal").cloned();
    let timeout = leaf
        .get_one::<String>("timeout")
        .map(|value| value.parse::<i32>().map_err(|error| error.to_string()))
        .transpose()?;
    with_client(root, |client| {
        Box::pin(async move {
            let live = client
                .live_services()
                .await
                .map_err(|error| error.to_string())?;
            print_observation_warning(&live);
            let mut partial = false;
            for selector in selectors {
                let service =
                    select_service(&live.services, &selector).map_err(|error| error.to_string())?;
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

fn with_client<F>(root: &ArgMatches, work: F) -> Result<(), Error>
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
    let context = leaf.get_one::<String>("context").map(String::as_str);
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

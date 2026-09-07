use std::collections::{BTreeSet, HashSet};

use clap::ArgMatches;
use ployz_core::{
    ContainerAction, ContainerId, ContainerObservation, ContainerRef, ContainerRuntimeObservation,
    DataLoss, DockerVolumeId, DockerVolumeName, HealthObservation, LiveServices,
    MachineObservation, MembershipObservation, ObservedDataLoss, RemoveVolumesRequest, RpcError,
    ServiceObservation, ServicePlacementEligibility, ServiceSelector, VolumeSource, select_service,
};

use crate::cluster::ContainerObservationCondition;

use super::{
    Error, cancellation_on_ctrl_c, confirm, data_loss, leaf_matches, string_values, with_client,
};

/// List the observed Services.
///
/// # Errors
///
/// Returns a connection, RPC, or serialization error.
pub fn list(root: &ArgMatches) -> Result<(), Error> {
    let json = leaf_matches(root)
        .get_one::<String>("output")
        .map(String::as_str)
        == Some("json");
    with_client(root, |client| {
        Box::pin(async move {
            let mut machines = client.machines().await?;
            client.observe_machine_storage(&mut machines).await;
            let live = client.live_services_from(&machines).await?;
            print_observation_warning(&live);
            let services = live.services();
            if json {
                println!("{}", serde_json::to_string_pretty(&services)?);
            } else {
                println!("SERVICE ID\tSERVICE\tCONTAINERS\tHOOKS");
                for service in &services {
                    let counts = service_counts(service, &machines);
                    println!(
                        "{}\t{}\t{}\t{}",
                        service.service_id,
                        service.identity,
                        service_count_text(counts),
                        service.hook_containers.len()
                    );
                    if counts.unknown > 0 {
                        eprintln!(
                            "WARNING: {} has unknown storage eligibility on {} Machine(s)",
                            service.identity, counts.unknown
                        );
                    }
                }
            }
            Ok(())
        })
    })
}

fn service_counts(service: &ServiceObservation, machines: &[MachineObservation]) -> ServiceCounts {
    let running = service
        .containers
        .iter()
        .filter(|container| {
            matches!(
                container.as_observation().runtime,
                ContainerRuntimeObservation::Running { .. }
            )
        })
        .count();
    let Some(spec) = service.observed_global_slot_spec() else {
        return ServiceCounts {
            running,
            expected: service.containers.len(),
            unknown: 0,
        };
    };
    let mut eligible = 0;
    let mut unknown = 0;
    for machine in machines
        .iter()
        .filter(|machine| machine.membership == MembershipObservation::Up)
    {
        match spec.placement_eligibility(&machine.machine, machine.storage.as_ref()) {
            ServicePlacementEligibility::Eligible => eligible += 1,
            ServicePlacementEligibility::Unknown(_) => unknown += 1,
            ServicePlacementEligibility::Ineligible(_) => {}
        }
    }
    ServiceCounts {
        running,
        expected: eligible,
        unknown,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ServiceCounts {
    running: usize,
    expected: usize,
    unknown: usize,
}

fn service_count_text(
    ServiceCounts {
        running,
        expected,
        unknown,
    }: ServiceCounts,
) -> String {
    if unknown == 0 {
        format!("{running}/{expected}")
    } else {
        format!("{running}/{expected} (+{unknown} unknown)")
    }
}

/// List observed Service and hook Containers.
///
/// # Errors
///
/// Returns a connection, RPC, or serialization error.
pub fn processes(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let sort = matches
        .get_one::<String>("sort")
        .cloned()
        .ok_or_else(|| Error::usage("sort order is required"))?;
    let json = matches.get_one::<String>("output").map(String::as_str) == Some("json");
    with_client(root, |client| {
        Box::pin(async move {
            let live = client.live_services().await?;
            print_observation_warning(&live);
            let services = live.services();
            let mut containers = services
                .iter()
                .flat_map(ployz_core::ServiceObservation::members)
                .collect::<Vec<_>>();
            sort_processes(&mut containers, &sort);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &containers
                            .iter()
                            .map(|container| container.as_observation())
                            .collect::<Vec<_>>()
                    )?
                );
            } else {
                println!("CONTAINER ID\tSERVICE\tKIND\tMACHINE\tSTATE");
                for container in containers {
                    let observation = container.as_observation();
                    println!(
                        "{}\t{}\t{}\t{}\t{:?}",
                        observation.container_id,
                        observation.identity(),
                        process_kind(container),
                        observation.machine_id,
                        observation.runtime
                    );
                }
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
                    .identity()
                    .cmp(&right_observation.identity())
            }),
            "machine" => left_observation
                .machine_id
                .as_str()
                .cmp(right_observation.machine_id.as_str())
                .then_with(|| {
                    left_observation
                        .identity()
                        .cmp(&right_observation.identity())
                }),
            _ => left_observation
                .identity()
                .cmp(&right_observation.identity()),
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
            let services = live.services();
            let service = select_service(&services, &selector)?;
            println!("{}", serde_json::to_string_pretty(service)?);
            Ok(())
        })
    })
}

/// Start, stop, or remove observed Services.
///
/// # Errors
///
/// Returns a connection, RPC, usage, or wait error.
pub fn change(root: &ArgMatches, action: ContainerAction) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let selectors = change_selectors(leaf)?;
    let (signal, timeout) = stop_options(leaf, action)?;
    with_client(root, |client| {
        Box::pin(async move {
            let live = client.live_services().await?;
            print_observation_warning(&live);
            let observed = live.services();
            let services = select_services(&observed, &selectors)?;
            let outcome =
                apply_service_action(client, &live, &services, action, signal, timeout).await?;
            service_action_result(outcome.partial)
        })
    })
}

/// Remove observed Services, and their named Docker Volumes when `--volumes` is set.
///
/// # Errors
///
/// Returns a connection, RPC, usage, or confirmation error.
pub fn remove(root: &ArgMatches) -> Result<(), Error> {
    if !leaf_matches(root).get_flag("volumes") {
        return change(root, ContainerAction::Remove);
    }
    remove_with_volumes(root)
}

fn remove_with_volumes(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let selectors = change_selectors(leaf)?;
    let yes = leaf.get_flag("yes");
    let named = string_values(leaf, "data-loss");
    with_client(root, |client| {
        Box::pin(async move {
            let live = client.live_services().await?;
            print_observation_warning(&live);
            let observed = live.services();
            let services = select_services(&observed, &selectors)?;
            let volumes = service_volume_teardown(&services, &observed)?;
            let data_loss_observed = ObservedDataLoss {
                data_loss: volumes
                    .iter()
                    .map(|id| DataLoss::DockerVolume { id: id.clone() })
                    .collect(),
            };
            data_loss::collect_data_loss_confirmation(&data_loss_observed, &named)?;
            if !volumes.is_empty() {
                println!("The following Docker Volumes will be removed:");
                for id in &volumes {
                    println!("  {}/{}", id.machine_id, id.name);
                }
                if !yes && !confirm()? {
                    println!("Cancelled. No Services or volumes were removed.");
                    return Ok(());
                }
            }
            let outcome = apply_service_action(
                client,
                &live,
                &services,
                ContainerAction::Remove,
                None,
                None,
            )
            .await?;
            let (volumes, skipped) = volumes_safe_to_remove(volumes, &services, &outcome.affected);
            let volume_result = if volumes.is_empty() {
                Ok(())
            } else {
                match client
                    .remove_volumes(RemoveVolumesRequest {
                        volumes,
                        force: false,
                    })
                    .await
                {
                    Ok(removal) => super::volume::refuse_unless_removed(removal),
                    Err(error) => Err(error.into()),
                }
            };
            combined_teardown_result(
                combined_teardown_result(
                    service_action_result(outcome.partial),
                    skipped_volume_result(&skipped),
                ),
                volume_result,
            )
        })
    })
}

fn service_volume_teardown(
    selected: &[&ServiceObservation],
    observed: &[ServiceObservation],
) -> Result<Vec<DockerVolumeId>, Error> {
    let mut volumes = BTreeSet::new();
    for service in selected {
        volumes.extend(managed_volume_ids(service));
    }
    let selected_identities = selected
        .iter()
        .map(|service| &service.identity)
        .collect::<HashSet<_>>();
    for service in observed {
        if selected_identities.contains(&service.identity) {
            continue;
        }
        if let Some(id) = docker_volume_ids(service)
            .into_iter()
            .find(|id| volumes.contains(id))
        {
            return Err(Error::usage(format!(
                "Docker Volume {} on {} is still mounted by {}",
                id.name, id.machine_id, service.identity
            )));
        }
    }
    Ok(volumes.into_iter().collect())
}

fn volumes_safe_to_remove(
    planned: Vec<DockerVolumeId>,
    selected: &[&ServiceObservation],
    gone: &HashSet<ContainerId>,
) -> (Vec<DockerVolumeId>, Vec<DockerVolumeId>) {
    let still_mounted = selected
        .iter()
        .flat_map(|service| service.members())
        .filter(|member| !gone.contains(&member.as_observation().container_id))
        .flat_map(|member| {
            member_volume_ids(member.as_observation(), VolumeSource::docker_volume_name)
        })
        .collect::<HashSet<_>>();
    planned
        .into_iter()
        .partition(|id| !still_mounted.contains(id))
}

fn skipped_volume_result(skipped: &[DockerVolumeId]) -> Result<(), Error> {
    if skipped.is_empty() {
        Ok(())
    } else {
        Err(Error::usage(format!(
            "Docker Volume removals not attempted: {}",
            skipped
                .iter()
                .map(|id| format!("{}/{}", id.machine_id, id.name))
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
}

fn service_action_result(partial: bool) -> Result<(), Error> {
    if partial {
        Err(Error::usage("Service lifecycle completed partially"))
    } else {
        Ok(())
    }
}

fn combined_teardown_result(
    action: Result<(), Error>,
    volumes: Result<(), Error>,
) -> Result<(), Error> {
    match (action, volumes) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(action), Err(volumes)) => Err(Error::usage(format!("{action}; {volumes}"))),
    }
}

fn managed_volume_ids(service: &ServiceObservation) -> Vec<DockerVolumeId> {
    volume_ids(service, VolumeSource::managed_docker_volume_name)
}

fn docker_volume_ids(service: &ServiceObservation) -> Vec<DockerVolumeId> {
    volume_ids(service, VolumeSource::docker_volume_name)
}

fn volume_ids(
    service: &ServiceObservation,
    name_of: fn(&VolumeSource) -> Option<&DockerVolumeName>,
) -> Vec<DockerVolumeId> {
    service
        .members()
        .flat_map(|member| member_volume_ids(member.as_observation(), name_of))
        .collect()
}

fn member_volume_ids(
    observation: &ContainerObservation,
    name_of: fn(&VolumeSource) -> Option<&DockerVolumeName>,
) -> Vec<DockerVolumeId> {
    observation
        .resolved_spec
        .volume_graph()
        .mounted_volumes()
        .filter_map(|volume| name_of(&volume.source))
        .map(|name| DockerVolumeId {
            machine_id: observation.machine_id,
            name: name.clone(),
        })
        .collect()
}

struct ServiceActionOutcome {
    affected: HashSet<ContainerId>,
    partial: bool,
}

async fn apply_service_action(
    client: &crate::connect::Client,
    live: &LiveServices<RpcError>,
    services: &[&ServiceObservation],
    action: ContainerAction,
    signal: Option<String>,
    timeout: Option<i32>,
) -> Result<ServiceActionOutcome, Error> {
    let service_container_ids = services
        .iter()
        .copied()
        .flat_map(|service| service.containers_for(action))
        .map(|container| container.as_observation().container_id)
        .collect::<HashSet<_>>();
    let mut changed = Vec::new();
    let mut partial = false;
    for service in services {
        let outcomes = client
            .change_observed_service(service, action, signal.clone(), timeout)
            .await;
        for success in outcomes.successes {
            println!("{:?}\t{}\t{}", action, success.machine_id, success.value);
            if service_container_ids.contains(&success.value) {
                changed.push(success.value);
            }
        }
        for failure in outcomes.failures {
            eprintln!(
                "WARNING: {:?} failed for {} on {}: {}",
                action, failure.error.container_id, failure.machine_id, failure.error.error.message
            );
            partial = true;
        }
    }
    let cancellation = cancellation_on_ctrl_c();
    let _parent = cancellation.clone().drop_guard();
    client
        .wait_for_container_observations(
            &changed,
            match action {
                ContainerAction::Start => ContainerObservationCondition::Serving,
                ContainerAction::Stop | ContainerAction::Remove => {
                    ContainerObservationCondition::Dropped
                }
            },
            &cancellation,
        )
        .await?;
    if !live.containers.all_targets_succeeded() {
        eprintln!("WARNING: the Service selection came from a partial Live Observation");
        partial = true;
    }
    Ok(ServiceActionOutcome {
        affected: changed.into_iter().collect(),
        partial,
    })
}

fn change_selectors(matches: &ArgMatches) -> Result<Vec<ServiceSelector>, Error> {
    let project = crate::project::resolve_explicit(matches)?;
    matches
        .get_many::<String>("service")
        .ok_or_else(|| Error::usage("at least one Service selector is required"))?
        .map(|selector| {
            let selector = ServiceSelector::parse(selector.as_str())?;
            match project.as_ref() {
                Some(project) => selector.with_project(&project.name).map_err(Into::into),
                None => Ok(selector),
            }
        })
        .collect()
}

fn select_services<'a>(
    services: &'a [ployz_core::ServiceObservation],
    selectors: &[ServiceSelector],
) -> Result<Vec<&'a ployz_core::ServiceObservation>, Error> {
    let mut seen = HashSet::new();
    let mut selected = Vec::new();
    for selector in selectors {
        let service = select_service(services, selector)?;
        if seen.insert(&service.identity) {
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
    for line in observation_warning_lines(live) {
        eprintln!("{line}");
    }
}

fn observation_warning_lines(live: &LiveServices<RpcError>) -> Vec<String> {
    let mut lines =
        vec!["WARNING: Live Observation is observer-relative and not globally complete".into()];
    lines.extend(live.containers.failures.iter().map(|failure| {
        format!(
            "WARNING: Machine {} failed: {}",
            failure.machine_id, failure.error.message
        )
    }));
    lines.extend(
        live.containers
            .omissions
            .iter()
            .map(|machine_id| format!("WARNING: Machine {machine_id} was omitted")),
    );
    lines
}

#[cfg(test)]
mod tests;

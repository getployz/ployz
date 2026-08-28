use std::collections::HashSet;

use clap::ArgMatches;
use ployz_core::{
    ContainerAction, ContainerRef, ContainerRuntimeObservation, HealthObservation, LiveServices,
    MachineObservation, MembershipObservation, RpcError, ServiceObservation,
    ServicePlacementEligibility, ServiceSelector, select_service,
};

use crate::cluster::ContainerObservationCondition;

use super::{Error, cancellation_on_ctrl_c, leaf_matches, with_client};

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
            let service_container_ids = services
                .iter()
                .flat_map(|service| &service.containers)
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
                        action,
                        failure.error.container_id,
                        failure.machine_id,
                        failure.error.error.message
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
            if partial {
                Err(Error::usage("Service lifecycle completed partially"))
            } else {
                Ok(())
            }
        })
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
mod tests {
    use ployz_core::{
        HookContainer, Machine, MachineFailure, MachineId, MachineName, MachineObservation,
        MachineTarget, ManagementAddress, MembershipObservation, PartialResult, Placement,
        RpcError, RpcErrorCode, ServiceContainer, ServiceId, ServiceMode, ServiceName,
        WireGuardPublicKey, derive_live_services,
    };
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
    fn global_summary_counts_only_up_placement_eligible_machines() {
        let mut service = service_named('a', "app", "api");
        let mut observation = service.containers.pop().unwrap().into_observation();
        observation.runtime = ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        };
        observation.resolved_spec.mode = ServiceMode::Global;
        observation.resolved_spec.placement = Placement {
            machines: ["edge-a", "edge-b", "edge-c"]
                .into_iter()
                .map(|name| MachineTarget::parse(name).unwrap())
                .collect(),
        };
        service.containers = vec![ServiceContainer::try_from(observation).unwrap()];
        let machines = [
            machine('a', "edge-a", MembershipObservation::Up),
            machine('b', "edge-b", MembershipObservation::Up),
            machine('c', "edge-c", MembershipObservation::Down),
            machine('d', "batch", MembershipObservation::Up),
        ];

        assert_eq!(
            service_counts(&service, &machines),
            ServiceCounts {
                running: 1,
                expected: 2,
                unknown: 0,
            }
        );
    }

    #[test]
    fn global_summary_exposes_unknown_storage_and_over_placement() {
        use std::num::NonZeroU64;

        use ployz_core::{
            ContainerPath, DockerVolumeName, MachineStorageObservation,
            ProvisionedVolumeMaximumBytes, ServiceMount, ServiceVolume, ServiceVolumeGraph,
            ServiceVolumeReference, VolumeSource,
        };

        let mut service = service_named('a', "app", "api");
        let mut first = service.containers.pop().unwrap().into_observation();
        first.runtime = ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        };
        first.resolved_spec.mode = ServiceMode::Global;
        let reference = ServiceVolumeReference::parse("data").unwrap();
        first.resolved_spec.volume_graph = ServiceVolumeGraph::parse(
            vec![ServiceVolume {
                reference: reference.clone(),
                source: VolumeSource::Provisioned {
                    name: DockerVolumeName::parse("app_data").unwrap(),
                    maximum_bytes: ProvisionedVolumeMaximumBytes::new(
                        NonZeroU64::new(100).unwrap(),
                    ),
                    labels: Default::default(),
                },
            }],
            vec![ServiceMount {
                volume: reference,
                target: ContainerPath::parse("/data").unwrap(),
                read_only: false,
                no_copy: false,
                subpath: None,
            }],
        )
        .unwrap();
        let containers = ('a'..='f')
            .map(|machine| {
                let mut observation = first.clone();
                observation.container_id =
                    ployz_core::ContainerId::parse(machine.to_string().repeat(64)).unwrap();
                observation.machine_id = MachineId::parse(machine.to_string().repeat(32)).unwrap();
                ServiceContainer::try_from(observation).unwrap()
            })
            .collect::<Vec<_>>();
        service.containers = containers.iter().take(3).cloned().collect();
        let mut machines = ('a'..='f')
            .chain(std::iter::once('1'))
            .map(|id| machine(id, &format!("edge-{id}"), MembershipObservation::Up))
            .collect::<Vec<_>>();
        for machine in machines.iter_mut().take(3) {
            machine.storage = Some(MachineStorageObservation::Ready);
        }
        for machine in machines.iter_mut().skip(3).take(3) {
            machine.storage = Some(MachineStorageObservation::Stateless);
        }

        assert_eq!(
            service_counts(&service, &machines),
            ServiceCounts {
                running: 3,
                expected: 3,
                unknown: 1,
            }
        );
        assert_eq!(
            service_count_text(service_counts(&service, &machines)),
            "3/3 (+1 unknown)"
        );
        service
            .containers
            .extend(containers.iter().skip(3).cloned());
        assert_eq!(
            service_counts(&service, &machines),
            ServiceCounts {
                running: 6,
                expected: 3,
                unknown: 1,
            }
        );
        assert_eq!(
            service_count_text(ServiceCounts {
                running: 6,
                expected: 3,
                unknown: 0,
            }),
            "6/3"
        );
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
    fn observation_warnings_come_from_partial_result_failures_and_omissions() {
        let failed_id = MachineId::parse("2".repeat(32)).unwrap();
        let omitted_id = MachineId::parse("3".repeat(32)).unwrap();
        let live =
            derive_live_services(PartialResult::<Vec<ployz_core::ContainerObservation>, _> {
                successes: Vec::new(),
                failures: vec![MachineFailure {
                    machine_id: failed_id,
                    error: RpcError {
                        code: RpcErrorCode::Unavailable,
                        message: "offline".into(),
                        details: serde_json::Value::Null,
                    },
                }],
                omissions: vec![omitted_id],
            });

        assert_eq!(
            observation_warning_lines(&live),
            vec![
                "WARNING: Live Observation is observer-relative and not globally complete"
                    .to_string(),
                format!("WARNING: Machine {failed_id} failed: offline"),
                format!("WARNING: Machine {omitted_id} was omitted"),
            ]
        );
    }

    #[test]
    fn lifecycle_selectors_deduplicate_names_and_ids() {
        let container = observation('a', 'a', "api", ContainerRuntimeObservation::Created);
        let service_id = container.service_id;
        let services = vec![ployz_core::ServiceObservation {
            identity: container.identity(),
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

    #[test]
    fn rm_project_name_removes_an_ambiguous_service_name() {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "rm", "alpha", "--project-name", "st1"])
            .unwrap();
        let services = vec![
            service_named('a', "st1", "alpha"),
            service_named('b', "st2", "alpha"),
        ];
        let selectors = change_selectors(leaf_matches(&matches)).unwrap();
        assert_eq!(
            select_services(&services, &selectors)
                .unwrap()
                .into_iter()
                .map(|service| service.identity.to_string())
                .collect::<Vec<_>>(),
            ["st1/alpha"]
        );
    }

    fn service_named(id: char, project: &str, name: &str) -> ployz_core::ServiceObservation {
        let mut container = observation(id, id, name, ContainerRuntimeObservation::Created);
        container.project_name = ployz_core::ProjectName::parse(project).unwrap();
        ployz_core::ServiceObservation {
            identity: container.identity(),
            service_id: container.service_id,
            containers: vec![ServiceContainer::try_from(container).unwrap()],
            hook_containers: Vec::new(),
        }
    }

    fn machine(id: char, name: &str, membership: MembershipObservation) -> MachineObservation {
        MachineObservation::new(
            Machine {
                id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
                name: MachineName::parse(name).unwrap(),
                subnet: format!("10.210.{}.0/24", id.to_digit(16).unwrap())
                    .parse()
                    .unwrap(),
                management_address: ManagementAddress("::1".parse().unwrap()),
                public_key: WireGuardPublicKey([id as u8; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
            },
            membership,
        )
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
            project_name: ployz_core::ProjectName::parse("app").unwrap(),
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

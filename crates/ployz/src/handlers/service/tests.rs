use std::collections::HashSet;

use ployz_core::{
    HookContainer, Machine, MachineFailure, MachineId, MachineName, MachineObservation,
    MachineTarget, MembershipObservation, PartialResult, Placement, RpcError, RpcErrorCode,
    ServiceContainer, ServiceId, ServiceMode, ServiceName, WireGuardPublicKey,
    derive_live_services,
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
    observation
        .try_update(|parts| {
            parts.runtime = ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            }
        })
        .unwrap();
    observation
        .try_update(|parts| parts.resolved_spec.mode = ServiceMode::Global)
        .unwrap();
    observation
        .try_update(|parts| {
            parts.resolved_spec.placement = Placement {
                machines: ["edge-a", "edge-b", "edge-c"]
                    .into_iter()
                    .map(|name| MachineTarget::parse(name).unwrap())
                    .collect(),
            }
        })
        .unwrap();
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
        ContainerPath, DockerVolumeName, MachineStorageObservation, ProvisionedVolumeMaximumBytes,
        ServiceMount, ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference,
    };

    let mut service = service_named('a', "app", "api");
    let mut first = service.containers.pop().unwrap().into_observation();
    first
        .try_update(|parts| {
            parts.runtime = ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            }
        })
        .unwrap();
    first
        .try_update(|parts| parts.resolved_spec.mode = ServiceMode::Global)
        .unwrap();
    let reference = ServiceVolumeReference::parse("data").unwrap();
    first
        .try_update(|parts| {
            parts
                .resolved_spec
                .set_volume_graph(
                    ServiceVolumeGraph::parse(
                        vec![ServiceVolume {
                            reference: reference.clone(),
                            source: ployz_core::RawVolumeSource::Provisioned {
                                name: DockerVolumeName::parse("data").unwrap(),
                                maximum_bytes: ProvisionedVolumeMaximumBytes::new(
                                    NonZeroU64::new(100).unwrap(),
                                ),
                                labels: Default::default(),
                            }
                            .admit()
                            .expect("valid volume declaration"),
                        }],
                        vec![ServiceMount {
                            volume: reference,
                            target: ContainerPath::parse("/data").unwrap(),
                            read_only: false,
                            no_copy: false,
                            subpath: None,
                        }],
                    )
                    .unwrap()
                    .scope_to_project(&ployz_core::ProjectName::parse("app").unwrap())
                    .unwrap()
                    .try_into()
                    .unwrap(),
                )
                .unwrap();
        })
        .unwrap();
    let containers = ('a'..='f')
        .map(|machine| {
            let mut observation = first.clone();
            observation
                .try_update(|parts| {
                    parts.container_id =
                        ployz_core::ContainerId::parse(machine.to_string().repeat(64)).unwrap()
                })
                .unwrap();
            observation
                .try_update(|parts| {
                    parts.machine_id = MachineId::parse(machine.to_string().repeat(32)).unwrap()
                })
                .unwrap();
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
    let live = derive_live_services(PartialResult::<Vec<ployz_core::ContainerObservation>, _> {
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
            "WARNING: Live Observation is observer-relative and not globally complete".to_string(),
            format!("WARNING: Machine {failed_id} failed: offline"),
            format!("WARNING: Machine {omitted_id} was omitted"),
        ]
    );
}

#[test]
fn lifecycle_selectors_deduplicate_names_and_ids() {
    let container = observation('a', 'a', "api", ContainerRuntimeObservation::Created);
    let service_id = container.service_id();
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

#[test]
fn service_volume_teardown_collects_managed_named_volumes() {
    let db = with_mounts(
        service_named('a', "app", "db"),
        vec![
            (ordinary("data"), "data", "/data"),
            (provisioned("cache"), "cache", "/cache"),
            (external("shared"), "shared", "/shared"),
            (bind(), "host", "/host"),
            (tmpfs(), "tmp", "/tmp"),
        ],
    );
    let volumes = service_volume_teardown(&[&db], std::slice::from_ref(&db)).unwrap();
    assert_eq!(
        volumes
            .iter()
            .map(|id| id.name.as_str())
            .collect::<Vec<_>>(),
        ["app_cache", "app_data"]
    );
    assert!(volumes.iter().all(|id| id.machine_id == machine_id('a')));
}

#[test]
fn service_volume_teardown_refuses_another_service_on_the_same_machine() {
    let db = with_mounts(
        service_named('a', "app", "db"),
        vec![(ordinary("data"), "data", "/data")],
    );
    let api = on_machine(
        with_mounts(
            service_named('b', "app", "api"),
            vec![(ordinary("data"), "data", "/data")],
        ),
        'a',
    );
    let error = service_volume_teardown(&[&db], &[db.clone(), api]).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "Docker Volume app_data on {} is still mounted by app/api",
            machine_id('a')
        )
    );
}

#[test]
fn service_volume_teardown_refuses_an_external_mount_of_the_same_volume() {
    let db = with_mounts(
        service_named('a', "app", "db"),
        vec![(ordinary("data"), "data", "/data")],
    );
    let api = on_machine(
        with_mounts(
            service_named('b', "app", "api"),
            vec![(external("app_data"), "shared", "/shared")],
        ),
        'a',
    );
    let error = service_volume_teardown(&[&db], &[db.clone(), api]).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "Docker Volume app_data on {} is still mounted by app/api",
            machine_id('a')
        )
    );
}

#[test]
fn service_volume_teardown_allows_selected_services_that_share_a_volume() {
    let db = with_mounts(
        service_named('a', "app", "db"),
        vec![(ordinary("data"), "data", "/data")],
    );
    let api = on_machine(
        with_mounts(
            service_named('b', "app", "api"),
            vec![(ordinary("data"), "data", "/data")],
        ),
        'a',
    );
    let volumes = service_volume_teardown(&[&db, &api], &[db.clone(), api.clone()]).unwrap();
    assert_eq!(
        volumes
            .iter()
            .map(|id| (id.machine_id, id.name.as_str()))
            .collect::<Vec<_>>(),
        [(machine_id('a'), "app_data")]
    );
}

#[test]
fn service_volume_teardown_allows_the_same_name_on_another_machine() {
    let db = with_mounts(
        service_named('a', "app", "db"),
        vec![(ordinary("data"), "data", "/data")],
    );
    let replica = with_mounts(
        service_named('b', "app", "replica"),
        vec![(ordinary("data"), "data", "/data")],
    );
    let volumes = service_volume_teardown(&[&db], &[db.clone(), replica]).unwrap();
    assert_eq!(
        volumes
            .iter()
            .map(|id| (id.machine_id, id.name.as_str()))
            .collect::<Vec<_>>(),
        [(machine_id('a'), "app_data")]
    );
}

#[test]
fn volumes_safe_to_remove_after_selected_containers_are_gone() {
    let db = with_mounts(
        service_named('a', "app", "db"),
        vec![(ordinary("data"), "data", "/data")],
    );
    let planned = service_volume_teardown(&[&db], std::slice::from_ref(&db)).unwrap();
    let removed = HashSet::from([container_id(&db)]);
    let (safe, skipped) = volumes_safe_to_remove(planned.clone(), &[&db], &removed);
    assert_eq!(safe, planned);
    assert!(skipped.is_empty());
    let (safe, skipped) = volumes_safe_to_remove(planned, &[&db], &HashSet::new());
    assert!(safe.is_empty());
    assert_eq!(
        skipped
            .iter()
            .map(|id| id.name.as_str())
            .collect::<Vec<_>>(),
        ["app_data"]
    );
}

#[test]
fn volumes_safe_to_remove_a_fully_removed_service_from_a_multi_service_request() {
    let db = with_mounts(
        service_named('a', "app", "db"),
        vec![(ordinary("data"), "data", "/data")],
    );
    let replica = with_mounts(
        service_named('b', "app", "replica"),
        vec![(ordinary("cache"), "cache", "/cache")],
    );
    let planned =
        service_volume_teardown(&[&db, &replica], &[db.clone(), replica.clone()]).unwrap();
    let (safe, skipped) = volumes_safe_to_remove(
        planned,
        &[&db, &replica],
        &HashSet::from([container_id(&db)]),
    );
    assert_eq!(
        safe.iter()
            .map(|id| (id.machine_id, id.name.as_str()))
            .collect::<Vec<_>>(),
        [(machine_id('a'), "app_data")]
    );
    assert_eq!(
        skipped
            .iter()
            .map(|id| id.name.as_str())
            .collect::<Vec<_>>(),
        ["app_cache"]
    );
}

#[test]
fn volumes_safe_to_remove_keeps_a_shared_volume_until_every_holder_is_gone() {
    let db = with_mounts(
        service_named('a', "app", "db"),
        vec![(ordinary("data"), "data", "/data")],
    );
    let api = on_machine(
        with_mounts(
            service_named('b', "app", "api"),
            vec![(ordinary("data"), "data", "/data")],
        ),
        'a',
    );
    let planned = service_volume_teardown(&[&db, &api], &[db.clone(), api.clone()]).unwrap();
    let (safe, skipped) = volumes_safe_to_remove(
        planned.clone(),
        &[&db, &api],
        &HashSet::from([container_id(&db)]),
    );
    assert!(safe.is_empty());
    assert_eq!(
        skipped
            .iter()
            .map(|id| id.name.as_str())
            .collect::<Vec<_>>(),
        ["app_data"]
    );
    let (safe, skipped) = volumes_safe_to_remove(
        planned,
        &[&db, &api],
        &HashSet::from([container_id(&db), container_id(&api)]),
    );
    assert_eq!(
        safe.iter().map(|id| id.name.as_str()).collect::<Vec<_>>(),
        ["app_data"]
    );
    assert!(skipped.is_empty());
}

#[test]
fn combined_teardown_result_preserves_action_error_and_joins_volume_failures() {
    assert!(combined_teardown_result(Ok(()), Ok(())).is_ok());
    assert_eq!(
        combined_teardown_result(
            Err(Error::usage("Service lifecycle completed partially")),
            Ok(())
        )
        .unwrap_err()
        .to_string(),
        "Service lifecycle completed partially"
    );
    assert_eq!(
        combined_teardown_result(
            Err(Error::usage("Service lifecycle completed partially")),
            Err(Error::usage(
                "one or more Docker Volume removals failed or were omitted: busy"
            )),
        )
        .unwrap_err()
        .to_string(),
        "Service lifecycle completed partially; one or more Docker Volume removals failed or were omitted: busy"
    );
}

#[test]
fn skipped_volumes_join_the_partial_lifecycle_error() {
    let db = with_mounts(
        service_named('a', "app", "db"),
        vec![(ordinary("data"), "data", "/data")],
    );
    let planned = service_volume_teardown(&[&db], std::slice::from_ref(&db)).unwrap();
    let (_, skipped) = volumes_safe_to_remove(planned, &[&db], &HashSet::new());
    let skipped_id = skipped.first().expect("still-mounted volume");
    assert_eq!(
        combined_teardown_result(service_action_result(true), skipped_volume_result(&skipped),)
            .unwrap_err()
            .to_string(),
        format!(
            "Service lifecycle completed partially; Docker Volume removals not attempted: {}/{}",
            skipped_id.machine_id, skipped_id.name
        )
    );
}

fn service_named(id: char, project: &str, name: &str) -> ployz_core::ServiceObservation {
    let mut container = observation(id, id, name, ContainerRuntimeObservation::Created);
    container
        .try_update(|parts| parts.project_name = ployz_core::ProjectName::parse(project).unwrap())
        .unwrap();
    ployz_core::ServiceObservation {
        identity: container.identity(),
        service_id: container.service_id(),
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
        .map(|container| container.as_observation().resolved_spec.name.as_str())
        .collect()
}

fn hook_observation(
    id: char,
    machine: char,
    name: &str,
    runtime: ContainerRuntimeObservation,
) -> ployz_core::ContainerObservation {
    let mut observation = observation(id, machine, name, runtime);
    observation
        .try_update(|parts| parts.kind = ployz_core::ContainerKind::PreDeployHook)
        .unwrap();
    observation
}

fn with_mounts(
    mut service: ployz_core::ServiceObservation,
    mounts: Vec<(ployz_core::RawVolumeSource, &'static str, &'static str)>,
) -> ployz_core::ServiceObservation {
    use ployz_core::{ContainerPath, ServiceMount, ServiceVolume, ServiceVolumeGraph};

    let mut observation = service.containers.pop().unwrap().into_observation();
    observation
        .try_update(|parts| {
            let project = parts.project_name.clone();
            let (volumes, mounts) = mounts
                .into_iter()
                .map(|(source, reference, target)| {
                    let reference = ployz_core::ServiceVolumeReference::parse(reference).unwrap();
                    (
                        ServiceVolume {
                            reference: reference.clone(),
                            source: source.admit().expect("valid volume declaration"),
                        },
                        ServiceMount {
                            volume: reference,
                            target: ContainerPath::parse(target).unwrap(),
                            read_only: false,
                            no_copy: false,
                            subpath: None,
                        },
                    )
                })
                .unzip();
            parts
                .resolved_spec
                .set_volume_graph(
                    ServiceVolumeGraph::parse(volumes, mounts)
                        .unwrap()
                        .scope_to_project(&project)
                        .unwrap()
                        .try_into()
                        .unwrap(),
                )
                .unwrap();
        })
        .unwrap();
    service.containers = vec![ServiceContainer::try_from(observation).unwrap()];
    service
}

fn on_machine(
    mut service: ployz_core::ServiceObservation,
    machine: char,
) -> ployz_core::ServiceObservation {
    let mut observation = service.containers.pop().unwrap().into_observation();
    observation
        .try_update(|parts| parts.machine_id = machine_id(machine))
        .unwrap();
    service.containers = vec![ServiceContainer::try_from(observation).unwrap()];
    service
}

fn ordinary(name: &str) -> ployz_core::RawVolumeSource {
    ployz_core::RawVolumeSource::Ordinary {
        name: ployz_core::DockerVolumeName::parse(name).unwrap(),
        driver: ployz_core::VolumeDriver::parse("local", Default::default()).unwrap(),
        labels: Default::default(),
    }
}

fn provisioned(name: &str) -> ployz_core::RawVolumeSource {
    use std::num::NonZeroU64;

    ployz_core::RawVolumeSource::Provisioned {
        name: ployz_core::DockerVolumeName::parse(name).unwrap(),
        maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
            NonZeroU64::new(100).unwrap(),
        ),
        labels: Default::default(),
    }
}

fn external(name: &str) -> ployz_core::RawVolumeSource {
    ployz_core::RawVolumeSource::External {
        name: ployz_core::DockerVolumeName::parse(name).unwrap(),
    }
}

fn bind() -> ployz_core::RawVolumeSource {
    ployz_core::RawVolumeSource::Bind {
        machine_path: ployz_core::MachinePath::parse("/var/lib/data").unwrap(),
        create_machine_path: false,
        propagation: None,
        recursive: None,
    }
}

fn tmpfs() -> ployz_core::RawVolumeSource {
    ployz_core::RawVolumeSource::Tmpfs {
        size_bytes: None,
        mode: None,
        options: Vec::new(),
    }
}

fn container_id(service: &ployz_core::ServiceObservation) -> ployz_core::ContainerId {
    service
        .members()
        .next()
        .expect("fixture services have a member")
        .as_observation()
        .container_id
}

fn machine_id(id: char) -> MachineId {
    MachineId::parse(id.to_string().repeat(32)).unwrap()
}

fn observation(
    id: char,
    machine: char,
    name: &str,
    runtime: ContainerRuntimeObservation,
) -> ployz_core::ContainerObservation {
    let service_id = ServiceId::parse(id.to_string().repeat(32)).unwrap();
    let service_name = ServiceName::parse(name).unwrap();
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: name.into(),
        created_at_unix_nanos: 0,
        machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
        project_name: ployz_core::ProjectName::parse("app").unwrap(),
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
    })
    .unwrap()
}

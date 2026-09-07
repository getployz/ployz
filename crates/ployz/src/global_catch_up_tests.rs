//! Tests for bounded Global catch-up and target storage eligibility.

use std::{
    cell::Cell,
    num::{NonZeroU32, NonZeroU64},
};

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerPath, ContainerResources,
    ContainerRuntimeObservation, DockerVolumeName, HealthObservation, Machine, MachineId,
    MachineName, MachineTarget, Placement, ProjectName, ProvisionedVolumeMaximumBytes, PullPolicy,
    RequestedServiceSpec, ResolvedServiceSpec, ResolvedUpdateConfig, RestartPolicy,
    ServiceContainerSpec, ServiceId, ServiceMode, ServiceMount, ServiceName, ServiceObservation,
    ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference, UpdateConfig, WireGuardPublicKey,
    service_containers,
};

use super::*;

#[tokio::test]
async fn partial_observations_reject_catch_up_before_any_placement() {
    let joiner = machine('1', "joiner");
    let peer = machine('f', "peer");
    for failed in [true, false] {
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: Vec::new(),
            target_services: None,
            capacity: None,
            storage: Ok(None),
            ensure_calls: Cell::new(0),
            failures: if failed {
                vec![ployz_core::MachineFailure {
                    machine_id: peer.id,
                    error: RpcError {
                        code: ployz_core::RpcErrorCode::Unavailable,
                        message: "peer unavailable".into(),
                        details: serde_json::Value::Null,
                    },
                }]
            } else {
                Vec::new()
            },
            omissions: if failed { Vec::new() } else { vec![peer.id] },
        };
        let error = catch_up_globals(&mut client, &joiner, false)
            .await
            .unwrap_err();
        let message = joined_catch_up_error(error);
        assert!(
            message.contains("partial Service observations"),
            "{message}"
        );
        assert!(message.contains(peer.id.as_str()), "{message}");
        assert_eq!(client.ensure_calls.get(), 0);
    }
}

#[tokio::test]
async fn stale_local_generation_checks_capacity_before_ensuring_current_slot() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let stale = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        created_on(&joiner, 'a'),
    );
    let current = global_service_with_image(
        qualified("app", "api"),
        'b',
        Placement::default(),
        running_on(&founder, 'b'),
        "ghcr.io/getployz/api:2",
    );
    let mut client = FakeCatchUpClient {
        machine_id: joiner.id,
        services: vec![stale, current],
        target_services: None,
        capacity: None,
        ensure_calls: Cell::new(0),
        failures: Vec::new(),
        omissions: Vec::new(),
        storage: Ok(None),
    };

    assert!(catch_up_globals(&mut client, &joiner, true).await.is_err());
    assert_eq!(client.ensure_calls.get(), 0);
}

#[tokio::test]
async fn successful_ensure_is_reobserved_before_success() {
    let joiner = machine('1', "joiner");
    let service = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        created_on(&joiner, 'a'),
    );
    let mut client = FakeCatchUpClient {
        machine_id: joiner.id,
        services: vec![service],
        target_services: None,
        capacity: None,
        ensure_calls: Cell::new(0),
        failures: Vec::new(),
        omissions: Vec::new(),
        storage: Ok(None),
    };

    let error = catch_up_globals(&mut client, &joiner, true)
        .await
        .unwrap_err();
    assert_eq!(client.ensure_calls.get(), 1);
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

#[tokio::test]
async fn initially_eligible_global_absent_from_target_inspection_remains_missing() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let service = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&founder, 'a'),
    );
    let mut client = FakeCatchUpClient {
        machine_id: joiner.id,
        services: vec![service],
        target_services: Some(Vec::new()),
        capacity: None,
        ensure_calls: Cell::new(0),
        failures: Vec::new(),
        omissions: Vec::new(),
        storage: Ok(None),
    };

    let error = catch_up_globals(&mut client, &joiner, true)
        .await
        .unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

#[tokio::test]
async fn initially_eligible_global_with_only_hook_visible_remains_missing() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let service = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&founder, 'a'),
    );
    let mut hook = service
        .containers
        .first()
        .expect("test Global has one Service container")
        .clone()
        .into_observation();
    hook.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
        .unwrap();
    let hook_only = ServiceObservation {
        identity: service.identity.clone(),
        service_id: service.service_id,
        containers: Vec::new(),
        hook_containers: vec![ployz_core::HookContainer::try_from(hook).unwrap()],
    };
    let mut client = FakeCatchUpClient {
        machine_id: joiner.id,
        services: vec![service],
        target_services: Some(vec![hook_only]),
        capacity: None,
        ensure_calls: Cell::new(0),
        failures: Vec::new(),
        omissions: Vec::new(),
        storage: Ok(None),
    };

    let error = catch_up_globals(&mut client, &joiner, true)
        .await
        .unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

#[tokio::test]
async fn initially_eligible_generation_absent_from_target_inspection_remains_missing() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let stale = global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&joiner, 'a'),
    );
    let current = global_service_with_image(
        qualified("app", "api"),
        'b',
        Placement::default(),
        running_on(&founder, 'b'),
        "ghcr.io/getployz/api:2",
    );
    let mut client = FakeCatchUpClient {
        machine_id: joiner.id,
        services: vec![stale.clone(), current],
        target_services: Some(vec![stale]),
        capacity: Some(BridgeEndpointCapacity::new(10, 0)),
        ensure_calls: Cell::new(0),
        failures: Vec::new(),
        omissions: Vec::new(),
        storage: Ok(None),
    };

    let error = catch_up_globals(&mut client, &joiner, true)
        .await
        .unwrap_err();
    assert_eq!(client.ensure_calls.get(), 1);
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

#[tokio::test]
async fn another_projects_matching_shape_does_not_satisfy_catch_up() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let shop = global_service(
        qualified("shop", "api"),
        'c',
        Placement::default(),
        running_on(&joiner, 'c'),
    );
    let prod = global_service(
        qualified("prod", "api"),
        'd',
        Placement::default(),
        running_on(&founder, 'e'),
    );
    let mut client = FakeCatchUpClient {
        machine_id: joiner.id,
        services: vec![shop.clone(), prod],
        target_services: Some(vec![shop]),
        capacity: Some(BridgeEndpointCapacity::new(10, 0)),
        ensure_calls: Cell::new(0),
        failures: Vec::new(),
        omissions: Vec::new(),
        storage: Ok(None),
    };

    let error = catch_up_globals(&mut client, &joiner, true)
        .await
        .unwrap_err();
    assert_eq!(client.ensure_calls.get(), 1);
    assert_eq!(error.unresolved, [qualified("prod", "api")]);
}

struct FakeCatchUpClient {
    machine_id: MachineId,
    services: Vec<ServiceObservation>,
    target_services: Option<Vec<ServiceObservation>>,
    capacity: Option<BridgeEndpointCapacity>,
    storage: Result<Option<MachineStorageObservation>, &'static str>,
    ensure_calls: Cell<usize>,
    failures: Vec<ployz_core::MachineFailure<RpcError>>,
    omissions: Vec<MachineId>,
}

impl CatchUpClient for FakeCatchUpClient {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, Failure> {
        Ok(LiveServices {
            containers: ployz_core::PartialResult {
                successes: vec![ployz_core::MachineSuccess {
                    machine_id: self.machine_id,
                    value: self
                        .services
                        .iter()
                        .flat_map(ServiceObservation::members)
                        .map(|container| container.as_observation().clone())
                        .collect(),
                }],
                failures: self.failures.clone(),
                omissions: self.omissions.clone(),
            },
        })
    }

    async fn target_storage(
        &mut self,
        _machine_id: &MachineId,
    ) -> Result<Option<MachineStorageObservation>, Failure> {
        self.storage.map_err(Failure::usage)
    }

    async fn bridge_capacity(
        &mut self,
        _machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure> {
        Ok(self.capacity.clone())
    }

    async fn ensure_global_slot(
        &mut self,
        _machine_id: &MachineId,
        _request: EnsureGlobalSlotRequest,
    ) -> Result<(), RpcError> {
        self.ensure_calls.set(self.ensure_calls.get() + 1);
        Ok(())
    }

    async fn target_containers(
        &mut self,
        _machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure> {
        Ok(self
            .target_services
            .as_ref()
            .unwrap_or(&self.services)
            .iter()
            .flat_map(ServiceObservation::members)
            .map(|container| container.as_observation().clone())
            .collect())
    }
}

#[test]
fn two_joiners_each_plan_only_their_own_slot() {
    let founder = machine('f', "founder");
    let first = machine('1', "first");
    let second = machine('2', "second");
    let ingress = global_service(
        QualifiedService::system_ingress(),
        'c',
        Placement::default(),
        running_on(&founder, 'a'),
    );

    let first_slots = plan_global_catch_up(std::slice::from_ref(&ingress), &first, None, false);
    let second_slots = plan_global_catch_up(std::slice::from_ref(&ingress), &second, None, false);

    assert_eq!(identities(&first_slots), ["ployz-system/ingress"]);
    assert_eq!(identities(&second_slots), ["ployz-system/ingress"]);
    assert!(
        first_slots
            .iter()
            .chain(second_slots.iter())
            .all(|slot| slot.resolved_spec().service_id.as_str() == service_id('c').as_str())
    );
}

#[test]
fn add_machine_inherits_observed_caddy_ingress_spec() {
    let founder = machine('f', "founder");
    let joiner = machine('1', "joiner");
    let slots = plan_global_catch_up(
        &[observed_caddy_ingress(&founder, 'c')],
        &joiner,
        None,
        false,
    );
    assert_eq!(slots.len(), 1);
    assert_eq!(
        slots.first().unwrap().resolved_spec().container.command,
        ["caddy", "run", "-c", "/config/caddy/Caddyfile"]
    );
}

#[test]
fn skip_ingress_omits_system_ingress_and_keeps_other_globals() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let services = [
        global_service(
            QualifiedService::system_ingress(),
            'c',
            Placement::default(),
            running_on(&founder, 'a'),
        ),
        global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&founder, 'b'),
        ),
    ];

    assert_eq!(
        identities(&plan_global_catch_up(&services, &joiner, None, true)),
        ["app/api"]
    );
    assert_eq!(
        identities(&plan_global_catch_up(&services, &joiner, None, false)),
        ["ployz-system/ingress", "app/api"]
    );
}

#[test]
fn x_machines_excluding_this_joiner_plans_no_slot() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let services = [global_service(
        qualified("app", "api"),
        'a',
        Placement {
            machines: vec![MachineTarget::parse("founder").unwrap()],
        },
        running_on(&founder, 'a'),
    )];
    assert!(plan_global_catch_up(&services, &joiner, None, false).is_empty());
}

#[test]
fn x_machines_including_this_joiner_plans_a_slot() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let services = [global_service(
        qualified("app", "api"),
        'a',
        Placement {
            machines: vec![
                MachineTarget::parse("founder").unwrap(),
                MachineTarget::parse("joiner").unwrap(),
            ],
        },
        running_on(&founder, 'a'),
    )];
    assert_eq!(
        identities(&plan_global_catch_up(&services, &joiner, None, false)),
        ["app/api"]
    );
}

#[test]
fn catch_up_never_names_a_peer_machine() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let slots = plan_global_catch_up(
        &[global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&founder, 'a'),
        )],
        &joiner,
        None,
        false,
    );
    let spec = &slots
        .first()
        .expect("eligible Global must produce a slot")
        .resolved_spec();
    let encoded = serde_json::to_string(spec).unwrap();
    assert!(
        !encoded.contains(founder.id.as_str()),
        "catch-up output must not target a peer Machine, got {encoded}"
    );
    assert_eq!(identities(&slots), ["app/api"]);
}

#[test]
fn machine_add_places_user_globals_not_only_ingress() {
    let added = machine('2', "edge");
    let founder = machine('f', "founder");
    let services = [
        global_service(
            QualifiedService::system_ingress(),
            'c',
            Placement::default(),
            running_on(&founder, 'a'),
        ),
        global_service(
            qualified("shop", "worker"),
            'b',
            Placement::default(),
            running_on(&founder, 'b'),
        ),
    ];
    assert_eq!(
        identities(&plan_global_catch_up(&services, &added, None, false)),
        ["ployz-system/ingress", "shop/worker"]
    );
}

#[test]
fn created_not_started_on_this_machine_is_still_a_slot() {
    let joiner = machine('1', "joiner");
    let services = [global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        created_on(&joiner, 'a'),
    )];
    assert_eq!(
        identities(&plan_global_catch_up(&services, &joiner, None, false)),
        ["app/api"]
    );
}

#[test]
fn running_on_this_machine_is_not_a_slot() {
    let joiner = machine('1', "joiner");
    let services = [global_service(
        qualified("app", "api"),
        'a',
        Placement::default(),
        running_on(&joiner, 'a'),
    )];
    assert!(plan_global_catch_up(&services, &joiner, None, false).is_empty());
}

#[test]
fn replicated_services_are_not_catch_up_slots() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let mut spec = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    spec.name = ServiceName::parse("api").unwrap();
    let services = [grouped(
        qualified("app", "api"),
        spec.to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped"),
        running_on(&founder, 'a'),
    )];
    assert!(plan_global_catch_up(&services, &joiner, None, false).is_empty());
}

#[tokio::test]
async fn provisioned_globals_use_target_storage_and_report_unknown() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let mut spec = requested(ServiceMode::Global);
    let reference = ServiceVolumeReference::parse("data").unwrap();
    spec.set_volume_graph(
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
        .unwrap(),
    )
    .unwrap();
    let service = grouped(
        qualified("app", "api"),
        spec.to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped"),
        running_on(&founder, 'a'),
    );

    for (storage, expected_calls, incomplete) in [
        (Ok(Some(MachineStorageObservation::Ready)), 1, false),
        (
            Ok(Some(MachineStorageObservation::Pool {
                size_bytes: NonZeroU64::new(100).unwrap(),
                used_bytes: 0,
                free_bytes: 100,
            })),
            1,
            false,
        ),
        (Ok(Some(MachineStorageObservation::Stateless)), 0, false),
        (Ok(None), 0, true),
        (Err("storage inspection failed"), 0, true),
    ] {
        let local = grouped(
            service.identity.clone(),
            service.observed_global_slot_spec().unwrap().clone(),
            running_on(&joiner, 'b'),
        );
        let stateless = global_service(
            qualified("app", "worker"),
            'c',
            Placement::default(),
            running_on(&founder, 'c'),
        );
        let local_stateless = global_service(
            qualified("app", "worker"),
            'c',
            Placement::default(),
            running_on(&joiner, 'd'),
        );
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![service.clone(), stateless],
            target_services: Some(vec![local, local_stateless]),
            capacity: Some(BridgeEndpointCapacity::new(10, 0)),
            storage,
            ensure_calls: Cell::new(0),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        let result = catch_up_globals(&mut client, &joiner, false).await;
        assert_eq!(client.ensure_calls.get(), expected_calls + 1);
        assert_eq!(result.is_err(), incomplete);
        if let Err(error) = result {
            assert_eq!(error.unresolved, [qualified("app", "api")]);
            let message = joined_catch_up_error(error);
            assert!(message.contains("storage eligibility is unknown"));
            if let Err(cause) = storage {
                assert!(message.contains(cause));
            }
        }
    }
}

fn observed_caddy_ingress(machine: &Machine, id: char) -> ServiceObservation {
    let spec = ployz_core::caddy_service_spec("caddy:test".into(), Vec::new(), None)
        .to_resolved(service_id(id), ResolvedUpdateConfig::default())
        .expect("volume graph is scoped");
    let mut container = running_on(machine, id);
    container
        .try_update(|parts| parts.created_at_unix_nanos = 1)
        .unwrap();
    grouped(QualifiedService::system_ingress(), spec, container)
}

fn identities(slots: &[ObservedGlobalSlotSpec]) -> Vec<String> {
    slots
        .iter()
        .map(|slot| slot.identity().to_string())
        .collect()
}

fn machine(hex: char, name: &str) -> Machine {
    Machine {
        id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
            .parse()
            .unwrap(),
        public_key: WireGuardPublicKey([hex as u8; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
    }
}

fn qualified(project: &str, name: &str) -> QualifiedService {
    QualifiedService::new(
        ProjectName::parse(project).unwrap(),
        ServiceName::parse(name).unwrap(),
    )
}

fn service_id(hex: char) -> ServiceId {
    ServiceId::parse(hex.to_string().repeat(32)).unwrap()
}

fn container_id(hex: char) -> ContainerId {
    ContainerId::parse(hex.to_string().repeat(64)).unwrap()
}

fn requested(mode: ServiceMode) -> RequestedServiceSpec {
    RequestedServiceSpec {
        name: ServiceName::parse("api").unwrap(),
        mode,
        container: ServiceContainerSpec {
            image: "ghcr.io/getployz/api:1".into(),
            command: Vec::new(),
            entrypoint: Vec::new(),
            environment: Default::default(),
            labels: Default::default(),
            hostname: None,
            extra_hosts: Vec::new(),
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            healthcheck: None,
            pull_policy: PullPolicy::Missing,
            init: None,
            user: None,
            working_directory: None,
            tty: false,
            open_stdin: false,
            privileged: false,
            pid_mode: None,
            log_driver: None,
            resources: ContainerResources::default(),
            stop_timeout_secs: None,
            sysctls: Default::default(),
            restart: RestartPolicy::default(),
        },
        placement: Placement::default(),
        ports: Vec::new(),
        mount_graph: Default::default(),
        pre_deploy: None,
        ingress_proxy_fragment: None,
        update: UpdateConfig::default(),
    }
}

fn global_service(
    identity: QualifiedService,
    id: char,
    placement: Placement,
    container: ContainerObservation,
) -> ServiceObservation {
    global_service_with_image(identity, id, placement, container, "ghcr.io/getployz/api:1")
}

fn global_service_with_image(
    identity: QualifiedService,
    id: char,
    placement: Placement,
    container: ContainerObservation,
    image: &str,
) -> ServiceObservation {
    let mut spec = requested(ServiceMode::Global);
    spec.name = identity.name.clone();
    spec.placement = placement;
    spec.container.image = image.into();
    grouped(
        identity,
        spec.to_resolved(service_id(id), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped"),
        container,
    )
}

fn grouped(
    identity: QualifiedService,
    spec: ResolvedServiceSpec,
    mut container: ContainerObservation,
) -> ServiceObservation {
    container
        .try_update(|parts| {
            parts.project_name = identity.project.clone();
            parts.resolved_spec = spec.clone();
        })
        .unwrap();
    ServiceObservation {
        identity,
        service_id: spec.service_id,
        containers: service_containers([container]),
        hook_containers: Vec::new(),
    }
}

fn running_on(machine: &Machine, hex: char) -> ContainerObservation {
    container_on(
        machine,
        hex,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
    )
}

fn created_on(machine: &Machine, hex: char) -> ContainerObservation {
    container_on(machine, hex, ContainerRuntimeObservation::Created)
}

fn container_on(
    machine: &Machine,
    hex: char,
    runtime: ContainerRuntimeObservation,
) -> ContainerObservation {
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: container_id(hex),
        display_name: format!("slot-{hex}"),
        created_at_unix_nanos: 0,
        machine_id: machine.id,
        project_name: ProjectName::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime,
        effective_healthcheck: None,
        resolved_spec: requested(ServiceMode::Global)
            .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped"),
        address: None,
        labels: Default::default(),
    })
    .unwrap()
}

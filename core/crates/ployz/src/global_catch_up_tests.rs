//! Tests for bounded Global catch-up and target storage eligibility.

use std::num::NonZeroU64;

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerPath, ContainerResources,
    ContainerRuntimeObservation, DockerVolumeName, HealthObservation, Machine, MachineId,
    MachineName, MachineStorageObservation, Namespace, Placement, ProvisionedVolumeMaximumBytes,
    PullPolicy, RequestedServiceSpec, ResolvedServiceSpec, ResolvedUpdateConfig, RestartPolicy,
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
            create_result: Ok(Some(created())),
            create_calls: Vec::new(),
            retire_calls: Vec::new(),
            retire_error: None,
            cancel_on_retire: None,
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
        let error = catch_up_globals(&mut client, &joiner).await.unwrap_err();
        let message = joined_catch_up_error(error, &joiner);
        assert!(
            message.contains("partial Service observations"),
            "{message}"
        );
        assert!(message.contains(peer.id.as_str()), "{message}");
        assert!(client.create_calls.is_empty());
    }
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
        create_result: Ok(Some(created())),
        create_calls: Vec::new(),
        retire_calls: Vec::new(),
        retire_error: None,
        cancel_on_retire: None,
        failures: Vec::new(),
        omissions: Vec::new(),
    };

    let error = catch_up_globals(&mut client, &joiner).await.unwrap_err();
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
        create_result: Ok(Some(created())),
        create_calls: Vec::new(),
        retire_calls: Vec::new(),
        retire_error: None,
        cancel_on_retire: None,
        failures: Vec::new(),
        omissions: Vec::new(),
    };

    let error = catch_up_globals(&mut client, &joiner).await.unwrap_err();
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
        create_result: Ok(Some(created())),
        create_calls: Vec::new(),
        retire_calls: Vec::new(),
        retire_error: None,
        cancel_on_retire: None,
        failures: Vec::new(),
        omissions: Vec::new(),
    };

    let error = catch_up_globals(&mut client, &joiner).await.unwrap_err();
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
        create_result: Ok(Some(created())),
        create_calls: Vec::new(),
        retire_calls: Vec::new(),
        retire_error: None,
        cancel_on_retire: None,
        failures: Vec::new(),
        omissions: Vec::new(),
    };

    let error = catch_up_globals(&mut client, &joiner).await.unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

#[tokio::test]
async fn another_namespaces_matching_shape_does_not_satisfy_catch_up() {
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
        create_result: Ok(Some(created())),
        create_calls: Vec::new(),
        retire_calls: Vec::new(),
        retire_error: None,
        cancel_on_retire: None,
        failures: Vec::new(),
        omissions: Vec::new(),
    };

    let error = catch_up_globals(&mut client, &joiner).await.unwrap_err();
    assert_eq!(error.unresolved, [qualified("prod", "api")]);
}

struct FakeCatchUpClient {
    machine_id: MachineId,
    services: Vec<ServiceObservation>,
    target_services: Option<Vec<ServiceObservation>>,
    create_calls: Vec<CreateContainerRequest>,
    create_result: Result<Option<ployz_core::ContainerCreated>, RpcError>,
    retire_calls: Vec<QualifiedService>,
    retire_error: Option<RpcError>,
    cancel_on_retire: Option<CancellationToken>,
    failures: Vec<ployz_core::MachineFailure<RpcError>>,
    omissions: Vec<MachineId>,
}

impl CatchUpClient for FakeCatchUpClient {
    async fn live_services(
        &mut self,
        _environment: EnvironmentValues,
    ) -> Result<LiveServices<RpcError>, Failure> {
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

    async fn bridge_capacity(
        &mut self,
        _machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure> {
        panic!("orchestration delegates capacity checks to create_slot")
    }

    async fn create_slot(
        &mut self,
        _machine_id: &MachineId,
        request: CreateContainerRequest,
    ) -> Result<Option<ployz_core::ContainerCreated>, RpcError> {
        self.create_calls.push(request);
        self.create_result.clone()
    }

    async fn start_slot(
        &mut self,
        _machine_id: &MachineId,
        _container_id: ContainerId,
    ) -> Result<(), RpcError> {
        Ok(())
    }

    async fn retire_slot(
        &mut self,
        _machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
    ) -> Result<(), RpcError> {
        self.retire_calls.push(slot.identity().clone());
        if let Some(cancellation) = &self.cancel_on_retire {
            cancellation.cancel();
        }
        self.retire_error.clone().map_or(Ok(()), Err)
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

fn machine(hex: char, name: &str) -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
            .parse()
            .unwrap(),
        public_key: WireGuardPublicKey([hex as u8; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        build_concurrency: None,
    }
}

fn qualified(namespace: &str, name: &str) -> QualifiedService {
    QualifiedService::new(
        Namespace::parse(namespace).unwrap(),
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
            parts.namespace = identity.namespace.clone();
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
        namespace: Namespace::parse("app").unwrap(),
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

fn provisioned_global_spec() -> RequestedServiceSpec {
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
        .scope_to_namespace(&ployz_core::Namespace::parse("app").unwrap())
        .unwrap(),
    )
    .unwrap();
    spec
}

#[tokio::test]
async fn failed_placement_is_reported_even_if_final_observation_is_running() {
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let mut client = FakeCatchUpClient {
        machine_id: joiner.id,
        services: vec![global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&founder, 'a'),
        )],
        target_services: Some(vec![global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&joiner, 'b'),
        )]),

        create_calls: Vec::new(),
        retire_calls: Vec::new(),
        retire_error: None,
        cancel_on_retire: None,
        create_result: Err(RpcError {
            code: ployz_core::RpcErrorCode::Conflict,
            message: "creation key conflict".into(),
            details: serde_json::Value::Null,
        }),
        failures: Vec::new(),
        omissions: Vec::new(),
    };
    let error = catch_up_globals(&mut client, &joiner).await.unwrap_err();
    assert!(joined_catch_up_error(error, &joiner).contains("creation key conflict"));
}

fn retiring(drained: &Machine, services: Vec<ServiceObservation>) -> FakeCatchUpClient {
    FakeCatchUpClient {
        machine_id: drained.id,
        services,
        target_services: None,
        create_calls: Vec::new(),
        retire_calls: Vec::new(),
        retire_error: None,
        cancel_on_retire: None,
        create_result: Ok(Some(created())),
        failures: Vec::new(),
        omissions: Vec::new(),
    }
}

fn global_on(drained: &Machine, namespace: &str, hex: char) -> ServiceObservation {
    global_service(
        qualified(namespace, "api"),
        hex,
        Placement::default(),
        running_on(drained, hex),
    )
}

#[tokio::test]
async fn retirement_touches_only_the_chosen_globals_and_starts_nothing() {
    let drained = machine('1', "drained");
    let mut client = retiring(
        &drained,
        vec![
            global_on(&drained, "app", 'a'),
            global_on(&drained, "other", 'b'),
        ],
    );
    let chosen = qualified("app", "api");
    let never = CancellationToken::new();
    let retired =
        retire_globals(&mut client, &drained, std::slice::from_ref(&chosen), &never).await;
    assert_eq!(retired, [(chosen.clone(), Retirement::Retired)]);
    assert_eq!(client.retire_calls, [chosen]);
    assert!(client.create_calls.is_empty());

    client.retire_calls.clear();
    assert!(
        retire_globals(&mut client, &drained, &[], &never)
            .await
            .is_empty()
    );
    assert!(client.retire_calls.is_empty() && client.create_calls.is_empty());
}

#[tokio::test]
async fn retirement_reports_every_global_and_stops_at_the_next_once_cancelled() {
    let drained = machine('1', "drained");
    let globals = [qualified("app", "api"), qualified("other", "api")];
    let services = vec![
        global_on(&drained, "app", 'a'),
        global_on(&drained, "other", 'b'),
    ];

    let cancellation = CancellationToken::new();
    let mut client = retiring(&drained, services.clone());
    client.cancel_on_retire = Some(cancellation.clone());
    assert_eq!(
        retire_globals(&mut client, &drained, &globals, &cancellation).await,
        [
            (globals[0].clone(), Retirement::Retired),
            (globals[1].clone(), Retirement::NotAttempted),
        ]
    );
    assert_eq!(client.retire_calls, [globals[0].clone()]);

    let mut client = retiring(&drained, services);
    client.retire_error = Some(RpcError {
        code: ployz_core::RpcErrorCode::Conflict,
        message: "the Server accepts it again".into(),
        details: serde_json::Value::Null,
    });
    let gone = qualified("gone", "api");
    let outcomes = retire_globals(
        &mut client,
        &drained,
        &[globals[0].clone(), gone.clone()],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes,
        [
            (
                globals[0].clone(),
                Retirement::NotRetired("the Server accepts it again".into())
            ),
            (gone, Retirement::Retired),
        ],
        "a Global a full observation no longer finds has nothing left to retire"
    );
}

#[tokio::test]
async fn a_global_whose_newest_container_is_not_global_is_reported_not_retired() {
    let drained = machine('1', "drained");
    let identity = qualified("app", "api");
    let mut spec = requested(ServiceMode::Replicated {
        replicas: std::num::NonZeroU32::MIN,
    });
    spec.name = identity.name.clone();
    let replicated = grouped(
        identity.clone(),
        spec.to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped"),
        running_on(&drained, 'a'),
    );
    let mut client = retiring(&drained, vec![replicated]);
    let outcomes = retire_globals(
        &mut client,
        &drained,
        std::slice::from_ref(&identity),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes,
        [(
            identity,
            Retirement::NotRetired("its newest Container is not Global; deploy it first".into())
        )]
    );
    assert!(client.retire_calls.is_empty());
}

fn created() -> ployz_core::ContainerCreated {
    ployz_core::ContainerCreated {
        container_id: container_id('a'),
        display_name: "api".into(),
    }
}

#[path = "global_catch_up_rpc_tests.rs"]
mod rpc_tests;

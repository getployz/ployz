//! Tests for bounded Global catch-up and target storage eligibility.

use std::num::NonZeroU64;

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerPath, ContainerRuntimeObservation,
    DockerVolumeName, HealthObservation, MachineId, MachineStorageObservation, Namespace,
    Placement, ProvisionedVolumeMaximumBytes, RequestedServiceSpec, ResolvedUpdateConfig,
    ServiceMode, ServiceMount, ServiceObservation, ServiceVolume, ServiceVolumeGraph,
    ServiceVolumeReference,
};

use super::*;
use crate::global_fixtures::{
    container_id, created_on, global_service, global_service_with_image, grouped, machine,
    qualified, requested, running_on, service_id,
};
use crate::serving_publish::tests::FakeRow;

#[tokio::test]
async fn partial_observations_reject_catch_up_before_any_placement() {
    let joiner = machine('1', "joiner");
    let peer = machine('f', "peer");
    for failed in [true, false] {
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: Vec::new(),
            target_services: None,
            run_result: Ok(Some(container_id('a'))),
            run_calls: Vec::new(),
            row: FakeRow::default(),
            failures: if failed {
                vec![ployz_core::MachineFailure {
                    machine_id: peer.id,
                    error: RpcError {
                        code: ployz_core::RpcErrorCode::Unavailable,
                        message: "peer unavailable".into(),
                        details: serde_json::Value::Null,
                        cause: Vec::new(),
                    },
                }]
            } else {
                Vec::new()
            },
            omissions: if failed { Vec::new() } else { vec![peer.id] },
            relisted: None,
            listings: 0,
        };
        let error = catch_up_globals(
            &mut client,
            &joiner,
            &tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap_err();
        let message = crate::ui::chain_text(&joined_catch_up_error(error, &joiner, |args| {
            shell_words::join(std::iter::once("ployz").chain(args.iter().copied()))
        }));
        assert!(
            message.contains("partial Service observations"),
            "{message}"
        );
        assert!(message.contains(peer.id.as_str()), "{message}");
        assert!(client.run_calls.is_empty());
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
        run_result: Ok(Some(container_id('a'))),
        run_calls: Vec::new(),
        row: FakeRow::default(),
        failures: Vec::new(),
        omissions: Vec::new(),
        relisted: None,
        listings: 0,
    };

    let error = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
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
        run_result: Ok(Some(container_id('a'))),
        run_calls: Vec::new(),
        row: FakeRow::default(),
        failures: Vec::new(),
        omissions: Vec::new(),
        relisted: None,
        listings: 0,
    };

    let error = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
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
        run_result: Ok(Some(container_id('a'))),
        run_calls: Vec::new(),
        row: FakeRow::default(),
        failures: Vec::new(),
        omissions: Vec::new(),
        relisted: None,
        listings: 0,
    };

    let error = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
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
        run_result: Ok(Some(container_id('a'))),
        run_calls: Vec::new(),
        row: FakeRow::default(),
        failures: Vec::new(),
        omissions: Vec::new(),
        relisted: None,
        listings: 0,
    };

    let error = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
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
        run_result: Ok(Some(container_id('a'))),
        run_calls: Vec::new(),
        row: FakeRow::default(),
        failures: Vec::new(),
        omissions: Vec::new(),
        relisted: None,
        listings: 0,
    };

    let error = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(error.unresolved, [qualified("prod", "api")]);
}

#[tokio::test]
async fn interruption_settles_started_work_and_only_reports_unresolved_globals() {
    for excluded in [false, true] {
        let joiner = machine('1', "joiner");
        let founder = machine('f', "founder");
        let first = qualified("app", "api");
        let second = qualified("app", "worker");
        let mut client = FakeCatchUpClient {
            machine_id: joiner.id,
            services: vec![
                global_service(
                    first.clone(),
                    'a',
                    Placement::default(),
                    running_on(&founder, 'a'),
                ),
                global_service(
                    second.clone(),
                    'b',
                    Placement::default(),
                    running_on(&founder, 'b'),
                ),
            ],
            target_services: Some(vec![global_service(
                first.clone(),
                'a',
                Placement::default(),
                running_on(&joiner, 'a'),
            )]),
            run_calls: Vec::new(),
            row: FakeRow::default(),
            run_result: Ok((!excluded).then(|| container_id('a'))),
            failures: Vec::new(),
            omissions: Vec::new(),
            relisted: None,
            listings: 0,
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut running = Vec::new();
        let error = catch_up_globals(&mut client, &joiner, &cancel, |fact| match fact {
            CatchUpFact::Creating(_) => cancel.cancel(),
            CatchUpFact::Running { service, .. } => running.push(service),
            CatchUpFact::Identified(_) | CatchUpFact::Excluded(_) | CatchUpFact::Failed(_) => {}
        })
        .await
        .unwrap_err();
        assert!(error.cause.is_interrupted());
        assert_eq!(error.unresolved, [second]);
        assert_eq!(
            client.run_calls.len(),
            1,
            "interruption stops before the next Global"
        );
        if excluded {
            assert!(running.is_empty());
        } else {
            assert_eq!(
                running,
                [first],
                "final target observation preserves completed work"
            );
        }
    }
}

struct FakeCatchUpClient {
    machine_id: MachineId,
    services: Vec<ServiceObservation>,
    target_services: Option<Vec<ServiceObservation>>,
    run_calls: Vec<NewContainer>,
    row: FakeRow,
    run_result: Result<Option<ContainerId>, RpcError>,
    failures: Vec<ployz_core::MachineFailure<RpcError>>,
    omissions: Vec<MachineId>,
    /// Every listing after the first, when a deploy lands between them.
    relisted: Option<Vec<ServiceObservation>>,
    listings: u32,
}

impl PointerClient for FakeCatchUpClient {
    async fn read_pointer(
        &mut self,
        service: &QualifiedService,
    ) -> Result<Option<ployz_core::ServingPointer>, RpcError> {
        self.row.read_pointer(service).await
    }

    async fn publish_pointer(
        &mut self,
        request: ployz_core::PublishServingPointerRequest,
    ) -> Result<ployz_core::ServingPointerPublication, RpcError> {
        self.row.publish_pointer(request).await
    }
}

impl CatchUpClient for FakeCatchUpClient {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, Failure> {
        self.listings += 1;
        let services = match &self.relisted {
            Some(relisted) if self.listings > 1 => relisted,
            _ => &self.services,
        };
        Ok(LiveServices {
            containers: ployz_core::PartialResult {
                successes: vec![ployz_core::MachineSuccess {
                    machine_id: self.machine_id,
                    value: services
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
        panic!("orchestration delegates capacity checks to run_slot")
    }

    async fn run_slot(
        &mut self,
        machine_id: &MachineId,
        new: NewContainer,
    ) -> Result<Option<ContainerId>, RpcError> {
        assert_eq!(machine_id, &self.machine_id);
        self.run_calls.push(new);
        self.run_result.clone()
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

        run_calls: Vec::new(),

        row: FakeRow::default(),
        run_result: Err(RpcError {
            code: ployz_core::RpcErrorCode::Conflict,
            message: "creation key conflict".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        }),
        failures: Vec::new(),
        omissions: Vec::new(),
        relisted: None,
        listings: 0,
    };
    let error = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert!(
        crate::ui::chain_text(&joined_catch_up_error(error, &joiner, |args| {
            shell_words::join(std::iter::once("ployz").chain(args.iter().copied()))
        }))
        .contains("creation key conflict")
    );
}

fn pointed(deploy: &str, containers: &[char], retiring: &[char]) -> ployz_core::ServingPointer {
    ployz_core::ServingPointer {
        deploy: ployz_core::DeploymentLogId::parse(deploy).unwrap(),
        containers: containers.iter().map(|id| container_id(*id)).collect(),
        previous: None,
        retiring: retiring.iter().map(|id| container_id(*id)).collect(),
    }
}

fn replacing_client(row: FakeRow) -> (Machine, FakeCatchUpClient) {
    let joiner = machine('1', "joiner");
    let client = FakeCatchUpClient {
        machine_id: joiner.id,
        services: vec![global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&joiner, 'd'),
        )],
        target_services: Some(vec![global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(&joiner, 'b'),
        )]),
        run_calls: Vec::new(),
        row,
        run_result: Ok(Some(container_id('b'))),
        failures: Vec::new(),
        omissions: Vec::new(),
        relisted: None,
        listings: 0,
    };
    (joiner, client)
}

#[tokio::test]
async fn a_healthy_copy_joins_the_pointer_and_what_it_replaced_retires() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointed("dep-a", &['d', 'c'], &[]));
    let (joiner, mut client) = replacing_client(row.clone());
    catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(
        *row.row.lock().unwrap(),
        Some(pointed("dep-a", &['b'], &['d', 'c'])),
        "the replaced copy here and the pointed copy no Machine lists both retire"
    );
}

#[tokio::test]
async fn a_lost_publish_retries_over_the_winner() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointed("dep-a", &['d'], &[]));
    row.interlopers
        .lock()
        .unwrap()
        .push(pointed("dep-a", &['d', 'e'], &[]));
    let (joiner, mut client) = replacing_client(row.clone());
    catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(*row.publishes.lock().unwrap(), 2);
    assert_eq!(
        *row.row.lock().unwrap(),
        Some(pointed("dep-a", &['e', 'b'], &['d']))
    );
}

#[tokio::test]
async fn a_service_with_no_pointer_gets_no_row() {
    let row = FakeRow::default();
    let (joiner, mut client) = replacing_client(row.clone());
    catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(*row.row.lock().unwrap(), None);
    assert_eq!(*row.publishes.lock().unwrap(), 0);
}

#[tokio::test]
async fn a_publish_that_keeps_losing_fails_the_slot() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointed("dep-a", &['d'], &[]));
    *row.interlopers.lock().unwrap() = ['5', '6', '7', '8']
        .iter()
        .map(|id| pointed("dep-a", &['d', *id], &[]))
        .collect();
    let (joiner, mut client) = replacing_client(row);
    let error = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

/// A deploy that lands between catch-up's listing and its publish.
fn newer_deploy(joiner: &Machine, row: &FakeRow, before_read: bool) -> Vec<ServiceObservation> {
    let newer = pointed("dep-q", &['e'], &['d']);
    if before_read {
        *row.row.lock().unwrap() = Some(newer);
    } else {
        row.interlopers.lock().unwrap().push(newer);
    }
    let mut relisted = vec![
        global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(joiner, 'd'),
        ),
        global_service(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(joiner, 'b'),
        ),
    ];
    if before_read {
        relisted.push(global_service_with_image(
            qualified("app", "api"),
            'a',
            Placement::default(),
            running_on(joiner, 'e'),
            "ghcr.io/getployz/api:2",
        ));
    }
    relisted
}

#[tokio::test]
async fn a_newer_deploy_read_before_the_publish_keeps_its_copy_serving() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointed("dep-a", &['d'], &[]));
    let (joiner, mut client) = replacing_client(row.clone());
    client.relisted = Some(newer_deploy(&joiner, &row, true));
    let error = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(
        *row.row.lock().unwrap(),
        Some(pointed("dep-q", &['e'], &['d'])),
        "the newer deploy's copy stays serving and the old spec's copy never joins"
    );
    assert_eq!(*row.publishes.lock().unwrap(), 0);
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

#[tokio::test]
async fn a_newer_deploy_published_after_the_listing_keeps_its_copy_serving() {
    let row = FakeRow::default();
    *row.row.lock().unwrap() = Some(pointed("dep-a", &['d'], &[]));
    let (joiner, mut client) = replacing_client(row.clone());
    client.relisted = Some(newer_deploy(&joiner, &row, false));
    let error = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(
        *row.row.lock().unwrap(),
        Some(pointed("dep-q", &['e'], &['d'])),
        "a lost publish re-reads the newer deploy and adds nothing beside its copy"
    );
    assert_eq!(error.unresolved, [qualified("app", "api")]);
}

#[path = "global_catch_up_rpc_tests.rs"]
mod rpc_tests;

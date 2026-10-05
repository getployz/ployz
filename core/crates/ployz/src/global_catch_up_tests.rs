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
        };
        let error = catch_up_globals(&mut client, &joiner).await.unwrap_err();
        let message = crate::ui::chain_text(&joined_catch_up_error(error, &joiner));
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

        create_calls: Vec::new(),
        create_result: Err(RpcError {
            code: ployz_core::RpcErrorCode::Conflict,
            message: "creation key conflict".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        }),
        failures: Vec::new(),
        omissions: Vec::new(),
    };
    let error = catch_up_globals(&mut client, &joiner).await.unwrap_err();
    assert!(
        crate::ui::chain_text(&joined_catch_up_error(error, &joiner))
            .contains("creation key conflict")
    );
}

fn created() -> ployz_core::ContainerCreated {
    ployz_core::ContainerCreated {
        container_id: container_id('a'),
        display_name: "api".into(),
    }
}

#[path = "global_catch_up_rpc_tests.rs"]
mod rpc_tests;

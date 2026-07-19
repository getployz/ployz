use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::{
    CONNECT_TIMEOUT, CoreContext, DEPLOY_TERMINAL_BUDGET, DindMachine, NAMESPACE_ID_LABEL,
    WORKLOAD_IMAGE, add_and_join_edge, assert_unit_active, connect_core_client, finish,
    init_core_cluster, locally_ready, managed_workload_containers, operation_events,
    operation_status, read_intent, reserved_deploy_request, terminal_operation_events,
    wait_for_machine_observations, wait_for_ready_dataplane, wait_for_terminal_deploy_status,
    with_evidence,
};
use ployz_core::deploy::{
    ContainerCommand, ContainerRuntimeSpec, DeployPlan, DeployPlanStep, DeployRequest,
    DeployServiceSpec, ImageReference, ImageSource, PreStartHook, ReplicaCount, ReplicaSlot,
    ServiceMode, VolumeName, VolumeSpec,
};
use ployz_core::intent::ActiveMachineState;
use ployz_core::network::{
    DataplaneProjection, DataplaneProjectionMember, MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS,
    WireGuardHandshakeStatus, WireGuardRttStatus,
};
use ployz_core::operation::{
    DeployCompletionOutcome, DeployOperationFailure, DeployOperationState,
    MachineLifecycleOperationState, OperationEvent, OperationStatus,
};
use ployz_core::security::NatsPrincipal;
use ployz_e2e::dind;
use ployz_nats::operation_api_client::OperationApiClientError;
use ployz_sdk_types::{
    DeployReserveRequest, DeploySubmitError, DeploySubmitRequest, MachineLifecycleRequest,
    NamespaceRemoveError, NamespaceRemoveRequest, NetworkDataplaneTestimony, NetworkStatusMachine,
    NetworkStatusRequest, OpsStatusError, OpsStatusRequest, ServiceRestartError,
    ServiceRestartRequest, SystemDeployRequest, SystemDeployTarget, VolumeCreateError,
    VolumeCreateRequest, VolumeRemoveError, VolumeRemoveRequest,
};
use ployz_test_support::ids::{
    idempotency_key, machine_id, namespace_id, operation_id, service_id,
};
use ployz_test_support::ops::wait_for_terminal_status;
/// Placement uses durable intent plus fresh machine testimony, fixes its peer
/// validation set for one pass, and does not require RTT.
#[tokio::test]
async fn group_placement_peer_health() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 2).await;
    with_evidence(&core.cluster, async {
        let [edge_2, edge_3] = core.cluster.edges() else {
            panic!("scenario requires exactly two edge machines");
        };
        for edge in [edge_2, edge_3] {
            add_and_join_edge(&core, edge).await;
        }
        for machine in ["core_1", "edge_2", "edge_3"] {
            wait_for_machine_observations(&core, &machine_id(machine)).await;
        }

        let controller = connect_core_client(
            &core,
            NatsPrincipal::Controller,
            &core.material.controller_seed,
        )
        .await
        .expect("connect controller for placement intent");
        let intent = read_intent(&controller, CONNECT_TIMEOUT)
            .await
            .expect("read placement intent");
        assert_declared_machines(&intent.dataplane_projection, &intent.active_machines);
        wait_for_ready_dataplane(&core, &intent.dataplane_projection).await;

        super::timed(
            "direct_push_multi_machine",
            super::assert_direct_push_multi_machine_deploy(&core),
        )
        .await;

        let stopped = core
            .exec_on(edge_3, &["systemctl", "stop", "ployzd-machine-edge_3"])
            .await;
        assert!(
            stopped.success(),
            "stop edge_3 machine role failed: {stopped:?}"
        );
        for machine in [core.cluster.core(), edge_2] {
            let dropped = core
                .exec_on(
                    machine,
                    &[
                        "iptables",
                        "-I",
                        "OUTPUT",
                        "1",
                        "-o",
                        "ployz-wg0",
                        "-p",
                        "icmp",
                        "-j",
                        "DROP",
                    ],
                )
                .await;
            assert!(
                dropped.success(),
                "suppress RTT on {} failed: {dropped:?}",
                machine.name
            );
        }

        wait_for_silent_edge_and_unavailable_rtt(&core, &intent.dataplane_projection).await;

        let first_namespace = namespace_id("placement_silent");
        let first_plan = deploy_and_read_plan(
            &core,
            "idem_dind_placement_silent",
            placement_target("placement_silent"),
        )
        .await;
        assert_eq!(
            first_plan.target_machines(),
            [machine_id("core_1"), machine_id("edge_2")],
            "silent member or configured deploy_machines changed placement: {first_plan:?}"
        );
        assert_eq!(
            runtime_machines(&core, &first_namespace).await,
            [machine_id("core_1"), machine_id("edge_2")],
            "first deploy runtime did not match its plan"
        );

        let restarted = core
            .exec_on(edge_3, &["systemctl", "start", "ployzd-machine-edge_3"])
            .await;
        assert!(
            restarted.success(),
            "restart edge_3 machine role failed: {restarted:?}"
        );
        assert_unit_active(&core, edge_3, "ployzd-machine-edge_3").await;
        wait_for_machine_observations(&core, &machine_id("edge_3")).await;
        wait_for_ready_dataplane(&core, &intent.dataplane_projection).await;

        block_wireguard_pair(&core, core.cluster.core(), edge_2).await;
        block_wireguard_pair(&core, core.cluster.core(), edge_3).await;
        reset_wireguard_peer(
            &core,
            core.cluster.core(),
            projection_member(&intent.dataplane_projection, "edge_2"),
        )
        .await;
        reset_wireguard_peer(
            &core,
            edge_3,
            projection_member(&intent.dataplane_projection, "core_1"),
        )
        .await;

        let asymmetric = wait_for_asymmetric_handshakes(&core, &intent.dataplane_projection).await;
        assert!(
            asymmetric
                .iter()
                .all(|machine| locally_ready(machine, &intent.dataplane_projection)),
            "handshake fault changed local dataplane readiness: {asymmetric:?}"
        );

        let second_namespace = namespace_id("placement_single_pass");
        let second_plan = deploy_and_read_plan(
            &core,
            "idem_dind_placement_single_pass",
            placement_target("placement_single_pass"),
        )
        .await;
        let steps = second_plan
            .phases
            .iter()
            .flat_map(|phase| &phase.services)
            .flat_map(|service| &service.steps)
            .collect::<Vec<_>>();
        assert!(
            matches!(
                steps.as_slice(),
                [
                    DeployPlanStep::RunContainer { machine_id: first, .. },
                    DeployPlanStep::RunContainer { machine_id: second, .. },
                ] if first == &machine_id("edge_2") && second == &machine_id("edge_2")
            ),
            "fixed preliminary set did not leave only edge_2 eligible: {second_plan:?}"
        );
        assert_eq!(
            runtime_machines(&core, &second_namespace).await,
            [machine_id("edge_2"), machine_id("edge_2")],
            "single-pass deploy runtime did not match its plan"
        );
    })
    .await;

    finish(core).await;
}

#[tokio::test]
async fn group_system_deploy_explicit_convergence() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 2).await;
    with_evidence(&core.cluster, async {
        let [edge_2, edge_3] = core.cluster.edges() else {
            panic!("scenario requires exactly two edges");
        };
        add_and_join_edge(&core, edge_2).await;
        for machine in ["core_1", "edge_2"] {
            wait_for_machine_observations(&core, &machine_id(machine)).await;
        }

        assert_reserved_system_namespace_rejections(&core).await;

        let initial = submit_system_probe(&core, "idem_system_initial", None).await;
        assert_completed_system_deploy(&core, &initial.operation_id, 2).await;
        assert_system_container_machines(&core, &["core_1", "edge_2"]).await;

        add_and_join_edge(&core, edge_3).await;
        wait_for_machine_observations(&core, &machine_id("edge_3")).await;
        assert_system_container_machines(&core, &["core_1", "edge_2"]).await;
        let joined = submit_system_probe(&core, "idem_system_joined", None).await;
        assert_completed_system_deploy(&core, &joined.operation_id, 3).await;
        assert_system_container_machines(&core, &["core_1", "edge_2", "edge_3"]).await;

        set_machine_lifecycle(&core, "edge_2", true).await;
        assert_system_container_machines(&core, &["core_1", "edge_2", "edge_3"]).await;
        let drained = submit_system_probe(&core, "idem_system_drained", None).await;
        assert_completed_system_deploy(&core, &drained.operation_id, 2).await;
        assert_system_container_machines(&core, &["core_1", "edge_3"]).await;

        set_machine_lifecycle(&core, "edge_2", false).await;
        assert_system_container_machines(&core, &["core_1", "edge_3"]).await;
        let resumed = submit_system_probe(&core, "idem_system_resumed", None).await;
        assert_completed_system_deploy(&core, &resumed.operation_id, 3).await;
        assert_system_container_machines(&core, &["core_1", "edge_2", "edge_3"]).await;

        let stopped = core
            .exec_on(edge_3, &["systemctl", "stop", "ployzd-machine-edge_3"])
            .await;
        assert!(stopped.success(), "stop edge testimony: {stopped:?}");
        wait_for_no_testimony(&core, "edge_3").await;
        let deferred = submit_system_probe(&core, "idem_system_deferred", None).await;
        let deferred_status = wait_for_terminal_deploy_status(
            &core,
            &deferred.operation_id,
            DEPLOY_TERMINAL_BUDGET,
        )
        .await;
        assert!(matches!(
            deferred_status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::CompletedWithWarnings
                }, ..
            }
        ));
        let deferred_plan = deploy_plan(&core, &deferred.operation_id).await;
        assert!(deferred_plan.phases.iter().flat_map(|phase| &phase.services).any(|service| {
            matches!(&service.placement, ployz_core::deploy::DeployServicePlacement::Global { deferred, .. }
                if deferred.iter().any(|machine| machine.machine_id == machine_id("edge_3")
                    && matches!(machine.reason, ployz_core::machine::MachineUsabilityReason::FactsUnavailable)))
        }));
        assert_system_container_machines(&core, &["core_1", "edge_2", "edge_3"]).await;

        let restarted = core
            .exec_on(edge_3, &["systemctl", "start", "ployzd-machine-edge_3"])
            .await;
        assert!(restarted.success(), "restore edge testimony: {restarted:?}");
        wait_for_machine_observations(&core, &machine_id("edge_3")).await;
        let recovered = submit_system_probe(&core, "idem_system_recovered", None).await;
        assert_completed_system_deploy(&core, &recovered.operation_id, 3).await;

        let before_failure = system_serving_entry(&core).await;
        let gated = submit_system_probe(
            &core,
            "idem_system_gated_failure",
            Some("timeout 120 sh -c 'until test -f /tmp/release; do sleep 0.1; done'"),
        )
        .await;
        let plan = wait_for_deploy_plan(&core, &gated.operation_id).await;
        let pre_start_machine = plan
            .phases
            .iter()
            .flat_map(|phase| &phase.services)
            .find_map(|service| service.pre_start.as_ref())
            .expect("gated plan has pre-start host")
            .machine_id
            .clone();
        let selected = plan
            .target_machines()
            .into_iter()
            .find(|machine| machine != &pre_start_machine)
            .expect("global plan selects another machine");
        let selected_host = dind_machine(&core, &selected);
        let pre_start_host = dind_machine(&core, &pre_start_machine);
        let hook = wait_for_pre_start_container(&core, pre_start_host).await;
        stop_docker_runtime(&core, selected_host).await;
        let release = core
            .exec_on(pre_start_host, &["docker", "exec", &hook, "touch", "/tmp/release"])
            .await;
        assert!(release.success(), "release pre-start gate: {release:?}");
        let failed = wait_for_terminal_deploy_status(
            &core,
            &gated.operation_id,
            DEPLOY_TERMINAL_BUDGET,
        )
        .await;
        let OperationStatus::Deploy {
            state: DeployOperationState::Failed { failure }, ..
        } = &failed else {
            panic!("selected-slot deploy must fail: {failed:?}");
        };
        assert!(matches!(failure,
            DeployOperationFailure::RuntimeUnavailable { machine_id, .. }
            | DeployOperationFailure::ContainerStartFailed { machine_id, .. }
                if machine_id == &selected));
        assert_eq!(system_serving_entry(&core).await, before_failure);
        assert!(!terminal_operation_events(&core, &gated.operation_id).await.is_empty());

        let docker_started = core
            .exec_on(
                selected_host,
                &["systemctl", "start", "docker.socket", "docker.service"],
            )
            .await;
        assert!(docker_started.success(), "restart selected Docker: {docker_started:?}");
        assert_unit_active(&core, selected_host, "docker").await;
        let retry = submit_system_probe(&core, "idem_system_retry", None).await;
        assert_completed_system_deploy(&core, &retry.operation_id, 3).await;
        assert_eq!(operation_status(&core, &gated.operation_id).await, failed);
        assert_system_container_machines(&core, &["core_1", "edge_2", "edge_3"]).await;
    })
    .await;
    finish(core).await;
}

async fn submit_system_probe(
    core: &CoreContext,
    idempotency: &str,
    pre_start: Option<&str>,
) -> ployz_sdk_types::AcceptedOperation {
    let namespace_id = ployz_core::namespace::reserved_system_namespace();
    let reservation = core
        .api
        .deploy_reserve(&DeployReserveRequest {
            namespace_id: namespace_id.clone(),
        })
        .await
        .expect("reserve system deploy");
    let pre_start = pre_start.map(|command| PreStartHook {
        command: ContainerCommand::try_new(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            command.to_owned(),
        ])
        .expect("pre-start command"),
    });
    core.api
        .system_deploy(&SystemDeployRequest {
            idempotency_key: idempotency_key(idempotency),
            reservation_id: reservation.reservation_id,
            target: SystemDeployTarget {
                origin: None,
                services: vec![DeployServiceSpec {
                    keep: None,
                    service_id: service_id("placement-probe"),
                    image: ImageReference::try_new(WORKLOAD_IMAGE).expect("workload image"),
                    image_source: ImageSource::Registry,
                    mode: ServiceMode::Global,
                    runtime: ContainerRuntimeSpec::image_defaults(),
                    pre_start,
                    depends_on: Vec::new(),
                    routes: Vec::new(),
                }],
            },
            registry_credentials: BTreeMap::new(),
        })
        .await
        .expect("system deploy submits")
}

async fn assert_completed_system_deploy(
    core: &CoreContext,
    operation_id: &ployz_core::ids::OperationId,
    expected_slots: usize,
) {
    let status = wait_for_terminal_deploy_status(core, operation_id, DEPLOY_TERMINAL_BUDGET).await;
    assert!(
        matches!(
            status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed
                },
                ..
            }
        ),
        "system deploy did not complete: {status:?}"
    );
    let plan = deploy_plan(core, operation_id).await;
    let slots = plan
        .phases
        .iter()
        .flat_map(|phase| &phase.services)
        .flat_map(|service| &service.steps)
        .filter(|step| {
            matches!(
                step,
                DeployPlanStep::RunContainer {
                    slot: ReplicaSlot::Global,
                    ..
                } | DeployPlanStep::UseExistingContainer {
                    slot: ReplicaSlot::Global,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        slots, expected_slots,
        "global slots must be machine-keyed: {plan:?}"
    );
}

async fn deploy_plan(
    core: &CoreContext,
    operation_id: &ployz_core::ids::OperationId,
) -> DeployPlan {
    terminal_operation_events(core, operation_id)
        .await
        .into_iter()
        .find_map(|event| match event {
            OperationEvent::DeployPlanCreated { plan, .. } => Some(plan),
            _ => None,
        })
        .expect("deploy plan evidence")
}

async fn wait_for_deploy_plan(
    core: &CoreContext,
    operation_id: &ployz_core::ids::OperationId,
) -> DeployPlan {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(plan) = operation_events(core, operation_id)
            .await
            .into_iter()
            .find_map(|event| match event {
                OperationEvent::DeployPlanCreated { plan, .. } => Some(plan),
                _ => None,
            })
        {
            return plan;
        }
        assert!(
            Instant::now() < deadline,
            "deploy plan was not recorded in time"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn assert_system_container_machines(core: &CoreContext, expected: &[&str]) {
    let mut actual = Vec::new();
    for (id, machine) in all_dind_machines(core) {
        let matching = managed_workload_containers(core, machine)
            .await
            .into_iter()
            .filter(|container| {
                container.labels.get(NAMESPACE_ID_LABEL).map(String::as_str) == Some("ployz-system")
                    && container
                        .labels
                        .get(super::CONTAINER_TYPE_LABEL)
                        .map(String::as_str)
                        == Some("service")
                    && container
                        .labels
                        .get(super::SERVICE_ID_LABEL)
                        .map(String::as_str)
                        == Some("placement-probe")
            })
            .count();
        assert!(matching <= 1, "more than one placement-probe on {id:?}");
        if matching == 1 {
            actual.push(id.as_str().to_owned());
        }
    }
    actual.sort();
    let mut expected = expected
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(actual, expected);
}

fn all_dind_machines(core: &CoreContext) -> Vec<(ployz_core::ids::MachineId, &DindMachine)> {
    std::iter::once((machine_id("core_1"), core.cluster.core()))
        .chain(
            core.cluster
                .edges()
                .iter()
                .enumerate()
                .map(|(index, machine)| (machine_id(&format!("edge_{}", index + 2)), machine)),
        )
        .collect()
}

fn dind_machine<'a>(core: &'a CoreContext, id: &ployz_core::ids::MachineId) -> &'a DindMachine {
    all_dind_machines(core)
        .into_iter()
        .find_map(|(candidate, machine)| (candidate == *id).then_some(machine))
        .unwrap_or_else(|| panic!("no DinD machine for {id:?}"))
}

async fn set_machine_lifecycle(core: &CoreContext, machine: &str, draining: bool) {
    let request = MachineLifecycleRequest {
        operation_id: operation_id(if draining {
            "op_system_drain"
        } else {
            "op_system_resume"
        }),
        machine_id: machine_id(machine),
    };
    let accepted = if draining {
        core.api
            .machine_drain(&request)
            .await
            .expect("drain submits")
    } else {
        core.api
            .machine_resume(&request)
            .await
            .expect("resume submits")
    };
    let status =
        wait_for_terminal_status(&core.api, &accepted.operation_id, Duration::from_secs(60)).await;
    assert!(
        matches!(
            status,
            OperationStatus::MachineLifecycle {
                state: MachineLifecycleOperationState::Completed,
                ..
            }
        ),
        "machine lifecycle failed: {status:?}"
    );
}

async fn wait_for_no_testimony(core: &CoreContext, machine: &str) {
    super::wait_for_inspect(
        core,
        &machine_id(machine),
        Duration::from_secs(60),
        "testimony did not become silent",
        |snapshot| {
            matches!(
                snapshot.testimony,
                ployz_sdk_types::MachineTestimony::NoAnswer
            )
        },
    )
    .await;
}

async fn wait_for_pre_start_container(core: &CoreContext, machine: &DindMachine) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(container) = managed_workload_containers(core, machine)
            .await
            .into_iter()
            .find(|container| {
                container.labels.get(NAMESPACE_ID_LABEL).map(String::as_str) == Some("ployz-system")
                    && container
                        .labels
                        .get(super::CONTAINER_TYPE_LABEL)
                        .map(String::as_str)
                        == Some("predeploy")
            })
        {
            return container.id;
        }
        assert!(
            Instant::now() < deadline,
            "pre-start gate container did not appear"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn stop_docker_runtime(core: &CoreContext, machine: &DindMachine) {
    let stopped = core
        .exec_on(
            machine,
            &["systemctl", "stop", "docker.service", "docker.socket"],
        )
        .await;
    assert!(stopped.success(), "stop selected Docker: {stopped:?}");
    let service = core
        .exec_on(machine, &["systemctl", "is-active", "docker.service"])
        .await;
    let socket = core
        .exec_on(machine, &["systemctl", "is-active", "docker.socket"])
        .await;
    assert!(
        !service.success() && !socket.success(),
        "Docker runtime remains active: {service:?} {socket:?}"
    );
}

async fn system_serving_entry(core: &CoreContext) -> ployz_core::intent::ServingTargetEntry {
    let controller = connect_core_client(
        core,
        NatsPrincipal::Controller,
        &core.material.controller_seed,
    )
    .await
    .expect("controller connects");
    read_intent(&controller, CONNECT_TIMEOUT)
        .await
        .expect("read intent")
        .serving_target_entries
        .into_iter()
        .find(|entry| {
            entry.namespace_id.as_str() == "ployz-system"
                && entry.service_id.as_str() == "placement-probe"
        })
        .expect("system serving entry")
}

async fn assert_reserved_system_namespace_rejections(core: &CoreContext) {
    let namespace_id = ployz_core::namespace::reserved_system_namespace();
    let reservation = core
        .api
        .deploy_reserve(&DeployReserveRequest {
            namespace_id: namespace_id.clone(),
        })
        .await
        .expect("reserved namespace reservation");
    let deploy_error = core
        .api
        .deploy_submit(&DeploySubmitRequest {
            idempotency_key: idempotency_key("idem_ordinary_reserved_deploy"),
            reservation_id: reservation.reservation_id,
            target: DeployRequest {
                namespace_id: namespace_id.clone(),
                origin: None,
                volumes: BTreeMap::new(),
                services: vec![DeployServiceSpec {
                    keep: None,
                    service_id: service_id("ordinary-probe"),
                    image: ImageReference::try_new(WORKLOAD_IMAGE).expect("workload image"),
                    image_source: ImageSource::Registry,
                    mode: ServiceMode::Global,
                    runtime: ContainerRuntimeSpec::image_defaults(),
                    pre_start: None,
                    depends_on: Vec::new(),
                    routes: Vec::new(),
                }],
            },
            registry_credentials: BTreeMap::new(),
        })
        .await;
    let deploy_operation = match deploy_error {
        Err(OperationApiClientError::Domain {
            error:
                DeploySubmitError::ReservedSystemNamespace {
                    operation_id,
                    namespace_id: rejected,
                },
            ..
        }) if rejected == namespace_id => operation_id,
        other => panic!("ordinary deploy must return typed reserved namespace: {other:?}"),
    };
    assert_no_operation(core, &deploy_operation).await;

    let restart_id = operation_id("op_ordinary_reserved_restart");
    assert!(matches!(core.api.service_restart(&ServiceRestartRequest {
        operation_id: restart_id.clone(), namespace_id: namespace_id.clone(), service_id: service_id("placement-probe"),
    }).await,
        Err(OperationApiClientError::Domain { error: ServiceRestartError::ReservedSystemNamespace { operation_id, namespace_id: rejected }, .. })
            if operation_id == restart_id && rejected == namespace_id));
    assert_no_operation(core, &restart_id).await;

    let remove_namespace_id = operation_id("op_ordinary_reserved_namespace_remove");
    assert!(matches!(core.api.namespace_remove(&NamespaceRemoveRequest {
        operation_id: remove_namespace_id.clone(), namespace_id: namespace_id.clone(),
    }).await,
        Err(OperationApiClientError::Domain { error: NamespaceRemoveError::ReservedSystemNamespace { operation_id, namespace_id: rejected }, .. })
            if operation_id == remove_namespace_id && rejected == namespace_id));
    assert_no_operation(core, &remove_namespace_id).await;

    let create_id = operation_id("op_ordinary_reserved_volume_create");
    let volume_name = VolumeName::try_new("probe-data").expect("volume name");
    assert!(matches!(core.api.volume_create(&VolumeCreateRequest {
        operation_id: create_id.clone(), namespace_id: namespace_id.clone(), volume_name: volume_name.clone(), machine_id: machine_id("core_1"), spec: VolumeSpec::Plain,
    }).await,
        Err(OperationApiClientError::Domain { error: VolumeCreateError::ReservedSystemNamespace { operation_id, namespace_id: rejected }, .. })
            if operation_id == create_id && rejected == namespace_id));
    assert_no_operation(core, &create_id).await;

    let remove_volume_id = operation_id("op_ordinary_reserved_volume_remove");
    assert!(matches!(core.api.volume_remove(&VolumeRemoveRequest {
        operation_id: remove_volume_id.clone(), namespace_id: namespace_id.clone(), volume_name,
    }).await,
        Err(OperationApiClientError::Domain { error: VolumeRemoveError::ReservedSystemNamespace { operation_id, namespace_id: rejected }, .. })
            if operation_id == remove_volume_id && rejected == namespace_id));
    assert_no_operation(core, &remove_volume_id).await;
}

async fn assert_no_operation(core: &CoreContext, operation_id: &ployz_core::ids::OperationId) {
    assert!(
        matches!(core.api.ops_status(&OpsStatusRequest { operation_id: operation_id.clone() }).await,
        Err(OperationApiClientError::Domain { error: OpsStatusError::NoSuchOperation { operation_id: missing }, .. })
            if missing == *operation_id)
    );
}

fn assert_declared_machines(
    projection: &DataplaneProjection,
    active_machines: &[ActiveMachineState],
) {
    let expected = [
        machine_id("core_1"),
        machine_id("edge_2"),
        machine_id("edge_3"),
    ];
    assert_eq!(
        active_machines
            .iter()
            .map(|machine| machine.machine_id.clone())
            .collect::<Vec<_>>(),
        expected,
        "durable active-machine roster changed"
    );
    assert_eq!(
        projection
            .declared_members()
            .iter()
            .map(|member| member.machine_id.clone())
            .collect::<Vec<_>>(),
        expected,
        "declared dataplane projection changed"
    );
}

async fn network_status(core: &CoreContext) -> Vec<NetworkStatusMachine> {
    core.api
        .network_status(&NetworkStatusRequest::First {
            mode: ployz_sdk_types::NetworkStatusMode::Snapshot,
        })
        .await
        .expect("network status succeeds")
        .machines
}

async fn wait_for_silent_edge_and_unavailable_rtt(
    core: &CoreContext,
    projection: &DataplaneProjection,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let machines = network_status(core).await;
        let silent = machines.iter().any(|machine| {
            machine.active.machine_id == machine_id("edge_3")
                && matches!(machine.dataplane, NetworkDataplaneTestimony::NoAnswer)
        });
        let healthy_pair = ["core_1", "edge_2"].iter().all(|machine_id_value| {
            let Some(machine) = machines
                .iter()
                .find(|machine| machine.active.machine_id == machine_id(machine_id_value))
            else {
                return false;
            };
            locally_ready(machine, projection)
                && peer(
                    machine,
                    projection,
                    if *machine_id_value == "core_1" {
                        "edge_2"
                    } else {
                        "core_1"
                    },
                )
                .is_some_and(|peer| {
                    matches!(
                        peer.handshake,
                        WireGuardHandshakeStatus::Ago { seconds }
                            if seconds <= MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS
                    ) && matches!(peer.rtt, WireGuardRttStatus::Unavailable { .. })
                })
        });
        if machines.len() == 3 && silent && healthy_pair {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "silent edge and RTT-unavailable healthy pair did not appear: {machines:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn peer<'a>(
    machine: &'a NetworkStatusMachine,
    projection: &DataplaneProjection,
    peer_machine: &str,
) -> Option<&'a ployz_core::network::WireGuardPeerStatus> {
    let expected_key = &projection_member(projection, peer_machine).wireguard_public_key;
    let NetworkDataplaneTestimony::Answered { value } = &machine.dataplane else {
        return None;
    };
    value
        .wireguard
        .peers
        .iter()
        .find(|peer| &peer.public_key == expected_key)
}

fn projection_member<'a>(
    projection: &'a DataplaneProjection,
    machine: &str,
) -> &'a DataplaneProjectionMember {
    projection
        .declared_members()
        .iter()
        .find(|member| member.machine_id == machine_id(machine))
        .unwrap_or_else(|| panic!("projection omitted {machine}"))
}

async fn block_wireguard_pair(core: &CoreContext, left: &DindMachine, right: &DindMachine) {
    for (machine, destination) in [(left, right.bridge_ip), (right, left.bridge_ip)] {
        let destination = destination.to_string();
        let dropped = core
            .exec_on(
                machine,
                &[
                    "iptables",
                    "-I",
                    "OUTPUT",
                    "1",
                    "-p",
                    "udp",
                    "-d",
                    &destination,
                    "--dport",
                    "51820",
                    "-j",
                    "DROP",
                ],
            )
            .await;
        assert!(
            dropped.success(),
            "block WireGuard from {} to {destination} failed: {dropped:?}",
            machine.name
        );
    }
}

async fn reset_wireguard_peer(
    core: &CoreContext,
    machine: &DindMachine,
    peer: &DataplaneProjectionMember,
) {
    let removed = core
        .exec_on(
            machine,
            &[
                "wg",
                "set",
                "ployz-wg0",
                "peer",
                peer.wireguard_public_key.as_str(),
                "remove",
            ],
        )
        .await;
    assert!(
        removed.success(),
        "remove WireGuard peer failed: {removed:?}"
    );
    let Some(endpoint) = peer.mesh_endpoints.first() else {
        panic!("peer {:?} has no mesh endpoint", peer.machine_id);
    };
    let endpoint = endpoint.to_string();
    let subnet = peer.endpoint_subnet.as_string();
    let restored = core
        .exec_on(
            machine,
            &[
                "wg",
                "set",
                "ployz-wg0",
                "peer",
                peer.wireguard_public_key.as_str(),
                "endpoint",
                &endpoint,
                "allowed-ips",
                &subnet,
                "persistent-keepalive",
                "25",
            ],
        )
        .await;
    assert!(
        restored.success(),
        "restore WireGuard peer failed: {restored:?}"
    );
}

async fn wait_for_asymmetric_handshakes(
    core: &CoreContext,
    projection: &DataplaneProjection,
) -> Vec<NetworkStatusMachine> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let machines = network_status(core).await;
        let handshake = |machine: &str, remote: &str| {
            machines
                .iter()
                .find(|status| status.active.machine_id == machine_id(machine))
                .and_then(|status| peer(status, projection, remote))
                .map(|peer| peer.handshake)
        };
        let unhealthy = |machine, remote| match handshake(machine, remote) {
            Some(WireGuardHandshakeStatus::Never) => true,
            Some(WireGuardHandshakeStatus::Ago { seconds }) => {
                seconds > MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS
            }
            None => false,
        };
        let fresh = |machine, remote| {
            matches!(
                handshake(machine, remote),
                Some(WireGuardHandshakeStatus::Ago { seconds })
                    if seconds <= MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS
            )
        };
        if machines.len() == 3
            && unhealthy("core_1", "edge_2")
            && unhealthy("edge_3", "core_1")
            && fresh("edge_2", "core_1")
            && fresh("edge_2", "edge_3")
            && fresh("core_1", "edge_3")
            && fresh("edge_3", "edge_2")
        {
            return machines;
        }
        assert!(
            Instant::now() < deadline,
            "asymmetric handshake graph did not appear: {machines:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn placement_target(namespace: &str) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id(namespace),
        origin: None,
        volumes: BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id("server"),
            image: ImageReference::try_new(WORKLOAD_IMAGE).expect("workload image reference"),
            image_source: ImageSource::Registry,
            mode: ployz_core::deploy::ServiceMode::Replicated {
                replicas: ReplicaCount::try_new(2).expect("two replicas"),
            },
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

async fn deploy_and_read_plan(
    core: &CoreContext,
    idempotency: &str,
    target: DeployRequest,
) -> DeployPlan {
    let accepted = core
        .api
        .deploy_submit(&reserved_deploy_request(core, idempotency, target).await)
        .await
        .expect("placement deploy submits");
    let status =
        wait_for_terminal_deploy_status(core, &accepted.operation_id, DEPLOY_TERMINAL_BUDGET).await;
    assert!(
        matches!(
            status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
                ..
            }
        ),
        "placement deploy did not complete: {status:?}"
    );
    let events = terminal_operation_events(core, &accepted.operation_id).await;
    let Some(OperationEvent::DeployPlanCreated { plan, .. }) = events
        .into_iter()
        .find(|event| matches!(event, OperationEvent::DeployPlanCreated { .. }))
    else {
        panic!("placement deploy did not record its plan");
    };
    plan
}

async fn runtime_machines(
    core: &CoreContext,
    namespace: &ployz_core::ids::NamespaceId,
) -> Vec<ployz_core::ids::MachineId> {
    let [edge_2, edge_3] = core.cluster.edges() else {
        panic!("placement scenario requires exactly two edges");
    };
    let namespace = namespace.as_str();
    let mut machines = Vec::new();
    for (machine_id, machine) in [
        (machine_id("core_1"), core.cluster.core()),
        (machine_id("edge_2"), edge_2),
        (machine_id("edge_3"), edge_3),
    ] {
        for container in managed_workload_containers(core, machine).await {
            if container
                .labels
                .get(NAMESPACE_ID_LABEL)
                .is_some_and(|label| label == namespace)
            {
                machines.push(machine_id.clone());
            }
        }
    }
    machines.sort();
    machines
}

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::{
    CONNECT_TIMEOUT, CoreContext, DEPLOY_TERMINAL_BUDGET, DindMachine, ManagedWorkloadContainer,
    NAMESPACE_ID_LABEL, WORKLOAD_IMAGE, add_and_join_edge, connect_core_client, finish,
    init_core_cluster, managed_workload_containers, operation_status, read_intent,
    terminal_operation_events, wait_for_machine_observations, wait_for_terminal_deploy_status,
    with_evidence,
};
use ployz_core::deploy::{
    ContainerCommand, ContainerRuntimeSpec, DeployPlan, DeployPlanStep, DeployRequest,
    DeployServicePlacement, DeployServiceSpec, ImageReference, ImageSource, ReplicaSlot,
    ServiceMode, VolumeName, VolumeSpec,
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
    NamespaceRemoveError, NamespaceRemoveRequest, OpsStatusError, OpsStatusRequest,
    ServiceRestartError, ServiceRestartRequest, SystemDeployRequest, SystemDeployTarget,
    VolumeCreateError, VolumeCreateRequest, VolumeRemoveError, VolumeRemoveRequest,
};
use ployz_test_support::ids::{idempotency_key, machine_id, operation_id, service_id};
use ployz_test_support::ops::wait_for_terminal_status;

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
        system_deploy_admission_and_initial(&core, edge_2).await;
        system_deploy_silence_and_recovery(&core, edge_2, "edge_2").await;
        system_deploy_lifecycle_convergence(&core, edge_3).await;
        system_deploy_failure_atomicity_and_retry(&core).await;
    })
    .await;
    finish(core).await;
}

async fn system_deploy_admission_and_initial(core: &CoreContext, edge_2: &DindMachine) {
    add_and_join_edge(core, edge_2).await;
    for machine in ["core_1", "edge_2"] {
        wait_for_machine_observations(core, &machine_id(machine)).await;
    }
    assert_reserved_system_namespace_rejections(core).await;

    let initial = submit_system_probe(core, "idem_system_initial").await;
    assert_completed_system_deploy(core, &initial.operation_id).await;
}

async fn system_deploy_lifecycle_convergence(core: &CoreContext, edge_3: &DindMachine) {
    let before_join = placement_probe_container_identities(core).await;
    add_and_join_edge(core, edge_3).await;
    wait_for_machine_observations(core, &machine_id("edge_3")).await;
    assert_placement_probe_container_identities(core, &before_join).await;
    let joined = submit_system_probe(core, "idem_system_joined").await;
    assert_completed_system_deploy(core, &joined.operation_id).await;

    let before_drain = placement_probe_container_identities(core).await;
    set_machine_lifecycle(core, "edge_2", true).await;
    assert_placement_probe_container_identities(core, &before_drain).await;
    let drained = submit_system_probe(core, "idem_system_drained").await;
    assert_completed_system_deploy(core, &drained.operation_id).await;

    let before_resume = placement_probe_container_identities(core).await;
    set_machine_lifecycle(core, "edge_2", false).await;
    assert_placement_probe_container_identities(core, &before_resume).await;
    let resumed = submit_system_probe(core, "idem_system_resumed").await;
    assert_completed_system_deploy(core, &resumed.operation_id).await;
}

async fn system_deploy_silence_and_recovery(
    core: &CoreContext,
    silent_machine: &DindMachine,
    silent_machine_id: &str,
) {
    let before_silence = placement_probe_container_identities(core).await;
    let machine_unit = format!("ployzd-machine-{silent_machine_id}");
    let stopped = core
        .exec_on(silent_machine, &["systemctl", "stop", &machine_unit])
        .await;
    assert!(stopped.success(), "stop edge testimony: {stopped:?}");
    wait_for_no_testimony(core, silent_machine_id).await;
    let deferred = submit_system_probe(core, "idem_system_deferred").await;
    let deferred_status =
        wait_for_terminal_deploy_status(core, &deferred.operation_id, DEPLOY_TERMINAL_BUDGET).await;
    assert!(matches!(
        deferred_status,
        OperationStatus::Deploy {
            state: DeployOperationState::Completed {
                outcome: DeployCompletionOutcome::CompletedWithWarnings
            },
            ..
        }
    ));
    let deferred_plan = deploy_plan(core, &deferred.operation_id).await;
    assert!(deferred_plan.phases.iter().flat_map(|phase| &phase.services).any(|service| {
        matches!(&service.placement, ployz_core::deploy::DeployServicePlacement::Global { deferred, .. }
            if deferred.iter().any(|machine| machine.machine_id == machine_id(silent_machine_id)
                && matches!(machine.reason, ployz_core::machine::MachineUsabilityReason::FactsUnavailable)))
    }));
    assert_placement_probe_container_identities(core, &before_silence).await;

    let restarted = core
        .exec_on(silent_machine, &["systemctl", "start", &machine_unit])
        .await;
    assert!(restarted.success(), "restore edge testimony: {restarted:?}");
    wait_for_machine_observations(core, &machine_id(silent_machine_id)).await;
    assert_placement_probe_container_identities(core, &before_silence).await;
    let recovered = submit_system_probe(core, "idem_system_recovered").await;
    assert_completed_system_deploy(core, &recovered.operation_id).await;
}

async fn system_deploy_failure_atomicity_and_retry(core: &CoreContext) {
    let before_failure = system_serving_entry(core).await;
    let before_containers = placement_probe_container_identities(core).await;
    let failing = submit_failing_system_probe(core, "idem_system_start_failure").await;
    let failed =
        wait_for_terminal_deploy_status(core, &failing.operation_id, DEPLOY_TERMINAL_BUDGET).await;
    let OperationStatus::Deploy {
        state:
            DeployOperationState::Failed {
                failure:
                    DeployOperationFailure::ContainerStartFailed {
                        machine_id: failed_machine,
                        ..
                    },
            },
        ..
    } = &failed
    else {
        panic!("selected-slot deploy must fail: {failed:?}");
    };
    let failed_plan = deploy_plan(core, &failing.operation_id).await;
    assert!(
        failed_plan
            .phases
            .iter()
            .flat_map(|phase| &phase.services)
            .flat_map(|service| &service.steps)
            .any(|step| matches!(
                step,
                DeployPlanStep::RunContainer {
                    machine_id,
                    slot: ReplicaSlot::Global,
                } if machine_id == failed_machine
            )),
        "failed machine must come from plan-selected work: {failed_plan:?}"
    );
    assert_eq!(system_serving_entry(core).await, before_failure);
    assert!(
        !terminal_operation_events(core, &failing.operation_id)
            .await
            .is_empty()
    );
    assert_placement_probe_container_identities(core, &before_containers).await;

    let retry = submit_system_probe(core, "idem_system_retry").await;
    assert_completed_system_deploy(core, &retry.operation_id).await;
    assert_eq!(operation_status(core, &failing.operation_id).await, failed);
}

async fn submit_system_probe(
    core: &CoreContext,
    idempotency: &str,
) -> ployz_sdk_types::AcceptedOperation {
    submit_system_probe_with_runtime(core, idempotency, ContainerRuntimeSpec::image_defaults())
        .await
}

async fn submit_failing_system_probe(
    core: &CoreContext,
    idempotency: &str,
) -> ployz_sdk_types::AcceptedOperation {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.command = Some(
        ContainerCommand::try_new(vec!["/ployz-missing-system-probe".to_owned()])
            .expect("failing probe command"),
    );
    submit_system_probe_with_runtime(core, idempotency, runtime).await
}

async fn submit_system_probe_with_runtime(
    core: &CoreContext,
    idempotency: &str,
    runtime: ContainerRuntimeSpec,
) -> ployz_sdk_types::AcceptedOperation {
    let namespace_id = ployz_core::namespace::reserved_system_namespace();
    let reservation = core
        .api
        .deploy_reserve(&DeployReserveRequest {
            namespace_id: namespace_id.clone(),
        })
        .await
        .expect("reserve system deploy");
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
                    runtime,
                    pre_start: None,
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
) -> Vec<ployz_core::ids::MachineId> {
    let status = wait_for_terminal_deploy_status(core, operation_id, DEPLOY_TERMINAL_BUDGET).await;
    let plan = deploy_plan(core, operation_id).await;
    let service = plan
        .phases
        .iter()
        .flat_map(|phase| &phase.services)
        .find(|service| service.service_id == service_id("placement-probe"))
        .expect("system deploy plan contains placement-probe");
    let DeployServicePlacement::Global {
        candidates,
        selected,
        deferred,
        draining,
    } = &service.placement
    else {
        panic!("placement-probe must use global placement: {plan:?}");
    };

    let expected_candidates = current_active_machine_ids(core).await;
    let mut candidates = candidates.clone();
    candidates.sort();
    assert_eq!(
        candidates, expected_candidates,
        "global candidates: {plan:?}"
    );

    let mut classified = selected.clone();
    classified.extend(deferred.iter().map(|machine| machine.machine_id.clone()));
    classified.extend(draining.iter().cloned());
    let classification_count = classified.len();
    classified.sort();
    classified.dedup();
    assert_eq!(
        classified.len(),
        classification_count,
        "global classification overlaps: {plan:?}"
    );
    assert_eq!(
        classified, expected_candidates,
        "global classification: {plan:?}"
    );

    let mut step_machines = service
        .steps
        .iter()
        .map(|step| match step {
            DeployPlanStep::RunContainer {
                machine_id,
                slot: ReplicaSlot::Global,
            }
            | DeployPlanStep::UseExistingContainer {
                machine_id,
                slot: ReplicaSlot::Global,
                ..
            } => machine_id.clone(),
            step => panic!("global service has non-global slot: {step:?}"),
        })
        .collect::<Vec<_>>();
    let step_count = step_machines.len();
    step_machines.sort();
    step_machines.dedup();
    assert_eq!(
        step_machines.len(),
        step_count,
        "duplicate global steps: {plan:?}"
    );
    let mut selected = selected.clone();
    selected.sort();
    assert_eq!(step_machines, selected, "selected global steps: {plan:?}");

    let expected_outcome = if deferred.is_empty() {
        DeployCompletionOutcome::Completed
    } else {
        DeployCompletionOutcome::CompletedWithWarnings
    };
    assert!(
        matches!(
            status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed { outcome },
                ..
            } if outcome == expected_outcome
        ),
        "system deploy terminal outcome did not match typed deferrals: {status:?} {plan:?}"
    );
    assert_selected_system_containers(core, &selected, draining).await;
    selected
}

async fn current_active_machine_ids(core: &CoreContext) -> Vec<ployz_core::ids::MachineId> {
    let controller = connect_core_client(
        core,
        NatsPrincipal::Controller,
        &core.material.controller_seed,
    )
    .await
    .expect("connect controller for active intent");
    let mut machine_ids = read_intent(&controller, CONNECT_TIMEOUT)
        .await
        .expect("read active intent")
        .active_machines
        .into_iter()
        .map(|machine| machine.machine_id)
        .collect::<Vec<_>>();
    machine_ids.sort();
    machine_ids
}

async fn deploy_plan(
    core: &CoreContext,
    operation_id: &ployz_core::ids::OperationId,
) -> DeployPlan {
    terminal_operation_events(core, operation_id)
        .await
        .into_iter()
        .find_map(|event| {
            let OperationEvent::DeployPlanCreated { plan, .. } = event else {
                return None;
            };
            Some(plan)
        })
        .expect("deploy plan evidence")
}

async fn assert_selected_system_containers(
    core: &CoreContext,
    selected: &[ployz_core::ids::MachineId],
    draining: &[ployz_core::ids::MachineId],
) {
    for (id, machine) in all_dind_machines(core) {
        let matching = managed_workload_containers(core, machine)
            .await
            .into_iter()
            .filter(is_placement_probe_service)
            .count();
        assert!(matching <= 1, "more than one placement-probe on {id:?}");
        if selected.contains(&id) {
            assert_eq!(
                matching, 1,
                "selected machine has no running placement-probe: {id:?}"
            );
        }
        if draining.contains(&id) {
            assert_eq!(
                matching, 0,
                "draining machine retained a running placement-probe: {id:?}"
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlacementProbeContainerIdentity {
    machine_id: ployz_core::ids::MachineId,
    container_id: String,
    labels: BTreeMap<String, String>,
}

async fn placement_probe_container_identities(
    core: &CoreContext,
) -> Vec<PlacementProbeContainerIdentity> {
    let mut identities = Vec::new();
    for (machine_id, machine) in all_dind_machines(core) {
        for container in managed_workload_containers(core, machine)
            .await
            .into_iter()
            .filter(is_placement_probe_service)
        {
            identities.push(PlacementProbeContainerIdentity {
                machine_id: machine_id.clone(),
                container_id: container.id,
                labels: container.labels.into_iter().collect(),
            });
        }
    }
    identities.sort_by(|left, right| {
        left.machine_id
            .as_str()
            .cmp(right.machine_id.as_str())
            .then_with(|| left.container_id.cmp(&right.container_id))
    });
    identities
}

async fn assert_placement_probe_container_identities(
    core: &CoreContext,
    expected: &[PlacementProbeContainerIdentity],
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let actual = placement_probe_container_identities(core).await;
        if actual == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "failed system deploy changed serving placement-probe container identities: expected {expected:?}, actual {actual:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn is_placement_probe_service(container: &ManagedWorkloadContainer) -> bool {
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

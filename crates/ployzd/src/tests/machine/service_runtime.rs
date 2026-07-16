use crate::control::operations::deploy::{
    MachineContainerRuntime, MachineContainerRuntimeError, MachineRuntimeUnavailableReason,
};
use crate::control::role_client::machine::{NatsMachineContainerRuntime, NatsMachineFactsReader};
use crate::roles::machine::protocol::{
    MachineContainerInspectRpcRequest, MachineContainerRemoveDomainError,
    MachineContainerRemoveRpcRequest, MachineContainerRemoveRpcResponse,
    MachineContainerRestartDomainError, MachineContainerRestartRpcRequest,
    MachineContainerRestartRpcResponse, MachineContainerRpcOk, MachineContainerRunHookRpcOk,
    MachineContainerRunHookRpcRequest, MachineContainerRunRpcRequest,
    MachineContainerStopDomainError, MachineContainerStopRpcRequest,
    MachineContainerStopRpcResponse, MachineDataplanePublicKeyRpcRequest,
    MachineDataplanePublicKeyRpcResponse, MachineDataplaneStatusRpcOk,
    MachineDataplaneStatusRpcRequest, MachineDataplaneStatusRpcResponse, MachineImagePull,
    MachineLogsTailRpcOk, MachineLogsTailRpcRequest, MachineLogsTailRpcResponse,
    MachineRunContainerOutcome, MachineSubstrateReportRpcRequest,
    MachineSubstrateReportRpcResponse, MachineVolumeEnsureRpcOk, MachineVolumeEnsureRpcRequest,
    MachineVolumeEnsureRpcResponse, MachineVolumeRemoveDomainError, MachineVolumeRemoveRpcOk,
    MachineVolumeRemoveRpcRequest, MachineVolumeRemoveRpcResponse,
};
use crate::roles::machine::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerRunner, MachineContainerRunnerError, MachineLogReader, MachineLogReaderError,
    MachineLogTail,
};
use crate::roles::machine::service::{
    MachinePloyzNativeMeshPreparer as LocalWireGuardEbpfPreparer, start_machine_role_service,
};
use futures_util::StreamExt;
use ployz_core::deploy::{
    DatasetName, ImageReference, VolumeMaxSizeBytes, VolumeName, ZfsPoolName,
};
use ployz_core::ids::ContainerId;
use ployz_core::intent::{ProvisionedVolumePinState, VolumePinState};
use ployz_core::machine::VolumeEnsureFailure;
use ployz_core::machine::runtime::{
    ContainerRuntimeState, MachineContainerFactDelta, MachineFactsSnapshot,
    ManagedContainerIdentity, ManagedContainerKind,
};
use ployz_core::network::{
    EbpfAttachmentStatus, EbpfForwardingReady, EbpfForwardingReadyEvidence, MachineDataplaneStatus,
    PloyzNativeMeshReady, WireGuardConfiguredMtu, WireGuardDetectedMtu, WireGuardEbpfPrepareError,
    WireGuardInterfaceMtu, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
    WireGuardStatus,
};
use ployz_nats::service_runtime::request_json;
use ployz_nats::subjects::{
    MachineServiceEndpoint, machine_container_facts, machine_facts, machine_service,
};
use ployz_test_support::containers;
use ployz_test_support::ids::{
    container_id, failure_message, machine_id, namespace_id, operation_id,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn machine_role_service_reports_unknown_substrate_without_evidence() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineSubstrateReportRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::SubstrateReport,
        ),
        &MachineSubstrateReportRpcRequest {
            operation_id: operation_id("op_update_1"),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("substrate report succeeds");

    let MachineSubstrateReportRpcResponse::Ok(ok) = response else {
        panic!("substrate report returned domain error");
    };
    assert_eq!(ok.machine_id, machine_id("machine_a"));
    assert_eq!(ok.reported.ployzd, None);
    assert_eq!(ok.reported.host_runner, None);
}

#[tokio::test]
async fn machine_role_service_gets_fresh_facts_without_observation_tick() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_existing(existing_container("ctr_existing", managed_identity())),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let facts = NatsMachineFactsReader::new(nats.client)
        .with_request_timeout(Duration::from_secs(1))
        .machine_facts(&machine_id("machine_a"))
        .await
        .expect("facts get succeeds");

    assert_eq!(facts.machine_id(), &machine_id("machine_a"));
    let observed = facts.containers();
    assert_eq!(observed.containers().len(), 1);
    assert_eq!(
        observed
            .container(&container_id("ctr_existing"))
            .expect("container exists")
            .state,
        ContainerRuntimeState::running_unroutable()
    );
    assert!(facts.observed_at_unix_ms() > 0);
}

#[tokio::test]
async fn machine_role_service_refreshes_and_publishes_full_facts() {
    let nats = test_nats().await;
    let mut snapshots = nats
        .client
        .subscribe(machine_facts(&machine_id("machine_a")))
        .await
        .expect("subscribe machine facts");
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_existing(existing_container("ctr_existing", managed_identity())),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.client
        .flush()
        .await
        .expect("flush snapshot subscription");
    let refresh = NatsMachineFactsReader::new(nats.client)
        .with_request_timeout(Duration::from_secs(1))
        .refresh_machine_facts(&machine_id("machine_a"))
        .await
        .expect("facts refresh succeeds");
    let message = tokio::time::timeout(Duration::from_secs(1), snapshots.next())
        .await
        .expect("snapshot arrives")
        .expect("snapshot subscription stays open");
    let facts = serde_json::from_slice::<MachineFactsSnapshot>(&message.payload)
        .expect("snapshot payload decodes");

    assert_eq!(facts.observed_at_unix_ms(), refresh.observed_at_unix_ms);
    assert!(
        facts
            .containers()
            .container(&container_id("ctr_existing"))
            .is_some()
    );
}

#[tokio::test]
async fn machine_role_service_creates_missing_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()).with_next_container("ctr_created"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let outcome = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect("container run succeeds");

    assert_eq!(
        outcome,
        MachineRunContainerOutcome::Created {
            container_id: container_id("ctr_created"),
        }
    );
    assert_eq!(
        state.creates(),
        vec![CreateManagedContainer {
            pull: MachineImagePull::Registry {
                credential: None,
                reference: image("registry.example/api:rev_2"),
            },
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            provisioned_volumes: Vec::new(),
            identity: managed_identity(),
        }]
    );
}

#[tokio::test]
async fn machine_role_service_removes_successful_hook_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone())
            .with_next_container("ctr_hook")
            .with_wait_exit_code(0),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a.flush().await.expect("flush machine service");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let outcome = client
        .run_pre_start_hook(&machine_id("machine_a"), hook_request())
        .await
        .expect("hook run succeeds");

    assert_eq!(
        outcome,
        MachineContainerRunHookRpcOk {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_hook"),
            exit_code: 0,
        }
    );
    client
        .remove_pre_start_hook(
            &machine_id("machine_a"),
            MachineContainerRemoveRpcRequest {
                operation_id: operation_id("op_123"),
                container_id: container_id("ctr_hook"),
                expected_identity: hook_request().container,
            },
        )
        .await
        .expect("hook cleanup succeeds");
    assert_eq!(state.removes(), vec![container_id("ctr_hook")]);
}

#[tokio::test]
async fn machine_role_service_retains_failed_hook_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone())
            .with_next_container("ctr_hook")
            .with_wait_exit_code(7),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a.flush().await.expect("flush machine service");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let outcome = client
        .run_pre_start_hook(&machine_id("machine_a"), hook_request())
        .await
        .expect("nonzero hook is a completed RPC");

    assert_eq!(outcome.exit_code, 7);
    assert!(state.removes().is_empty());
}

#[tokio::test]
async fn machine_role_service_recovers_completed_hook_without_rerunning_it() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone())
            .with_existing(existing_container_with_state(
                "ctr_hook",
                hook_request().container,
                ExistingManagedContainerState::NotStartable {
                    description: "exited".to_owned(),
                },
            ))
            .with_wait_exit_code(0),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a.flush().await.expect("flush machine service");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let outcome = client
        .run_pre_start_hook(&machine_id("machine_a"), hook_request())
        .await
        .expect("completed hook is recovered");

    assert_eq!(outcome.container_id, container_id("ctr_hook"));
    assert!(state.creates().is_empty());
    assert!(state.starts().is_empty());
}

#[tokio::test]
async fn machine_role_service_reuses_existing_operation_step_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone())
            .with_existing(existing_container("ctr_existing", managed_identity())),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let outcome = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect("container run succeeds");

    assert_eq!(
        outcome,
        MachineRunContainerOutcome::ReusedRunning {
            container_id: container_id("ctr_existing"),
        }
    );
    assert!(state.creates().is_empty());
}

#[tokio::test]
async fn machine_role_service_creates_when_sibling_service_uses_same_operation_step() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let sibling_identity = containers::identity("svc_worker")
        .entry("entry_worker")
        .operation("op_123")
        .step("run_1")
        .build();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone())
            .with_existing(existing_container("ctr_sibling", sibling_identity))
            .with_next_container("ctr_created"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let outcome = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect("container run succeeds");

    assert_eq!(
        outcome,
        MachineRunContainerOutcome::Created {
            container_id: container_id("ctr_created"),
        }
    );
    assert_eq!(
        state.creates(),
        vec![CreateManagedContainer {
            pull: MachineImagePull::Registry {
                credential: None,
                reference: image("registry.example/api:rev_2"),
            },
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            provisioned_volumes: Vec::new(),
            identity: managed_identity(),
        }]
    );
}

#[tokio::test]
async fn machine_role_service_publishes_observed_delta_after_run() {
    let nats = test_nats().await;
    let mut deltas = nats
        .client
        .subscribe(machine_container_facts(&machine_id("machine_a")))
        .await
        .expect("subscribe deltas");
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state)
            .with_existing(existing_container("ctr_existing", managed_identity())),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client.clone());

    client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect("container run succeeds");

    match next_container_fact_delta(&mut deltas).await {
        MachineContainerFactDelta::ContainerObserved { observation, .. } => {
            assert_eq!(observation.container_id, container_id("ctr_existing"));
            assert_eq!(
                observation.state,
                ContainerRuntimeState::running_unroutable()
            );
        }
        other => panic!("unexpected delta: {other:?}"),
    }
}

#[tokio::test]
async fn machine_role_service_starts_existing_stopped_operation_step_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()).with_existing(existing_container_with_state(
            "ctr_existing",
            managed_identity(),
            ExistingManagedContainerState::StartableStopped,
        )),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let outcome = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect("container run succeeds");

    assert_eq!(
        outcome,
        MachineRunContainerOutcome::StartedExisting {
            container_id: container_id("ctr_existing"),
        }
    );
    assert_eq!(state.starts(), vec![container_id("ctr_existing")]);
    assert!(state.creates().is_empty());
}

#[tokio::test]
async fn machine_role_service_inspects_live_container() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_existing(existing_container("ctr_existing", managed_identity())),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let client = NatsMachineContainerRuntime::new(nats.client);

    let inspection = client
        .inspect_container(
            &machine_id("machine_a"),
            MachineContainerInspectRpcRequest {
                container_id: container_id("ctr_existing"),
            },
        )
        .await
        .expect("inspect succeeds");
    let observation = inspection.observation.expect("container exists");

    assert_eq!(observation.container_id, container_id("ctr_existing"));
    assert_eq!(
        observation.state,
        ContainerRuntimeState::running_unroutable()
    );
}

#[tokio::test]
async fn machine_role_service_reports_start_failure_with_container_evidence() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state).with_start_failure("ctr_created", "exec format error"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("container start failure is returned");

    assert_eq!(
        error,
        MachineContainerRuntimeError::CreatedContainerStartFailed {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_created"),
            message: failure_message("container start failed: exec format error"),
            inspect_hint: inspect_hint("ctr_created"),
        }
    );
}

#[tokio::test]
async fn machine_role_service_reports_existing_start_failure_without_created_evidence() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state).with_existing_start_failure("ctr_existing", "still stopping"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("existing container start failure is returned");

    assert_eq!(
        error,
        MachineContainerRuntimeError::ExistingContainerStartFailed {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_existing"),
            message: failure_message("container start failed: still stopping"),
            inspect_hint: inspect_hint("ctr_existing"),
        }
    );
}

#[tokio::test]
async fn machine_role_service_reports_duplicate_exact_run_identity_as_ambiguous() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state)
            .with_existing(existing_container("ctr_a", managed_identity()))
            .with_existing(existing_container("ctr_b", managed_identity())),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("container run reports ambiguity");

    assert_eq!(
        error,
        MachineContainerRuntimeError::OperationStepAmbiguous {
            machine_id: machine_id("machine_a"),
            operation_id: operation_id("op_123"),
            step_id: ployz_test_support::ids::step_id("run_1"),
            container_ids: vec![container_id("ctr_a"), container_id("ctr_b")],
        }
    );
    assert_eq!(service.health().domain_failures, 1);
}

#[tokio::test]
async fn machine_role_service_maps_create_failure_to_unavailable_runtime() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default()).with_create_failure("disk full"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("container create fails");

    assert_eq!(
        error,
        MachineContainerRuntimeError::Unavailable {
            machine_id: machine_id("machine_a"),
            reason: MachineRuntimeUnavailableReason::ServiceInternal {
                message: "container create failed: disk full".to_owned(),
            },
        }
    );
}

#[tokio::test]
async fn machine_role_service_removes_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineContainerRemoveRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::ContainerRemove,
        ),
        &MachineContainerRemoveRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_old"),
            expected_identity: managed_identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineContainerRemoveRpcResponse::Ok(MachineContainerRpcOk {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_old"),
        })
    );
    assert_eq!(state.removes(), vec![container_id("ctr_old")]);
}

#[tokio::test]
async fn machine_role_service_removes_volume() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineVolumeRemoveRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::VolumeRemove,
        ),
        &MachineVolumeRemoveRpcRequest::DockerReference {
            operation_id: operation_id("op_123"),
            volume: VolumePinState::plain(
                namespace_id("prod"),
                VolumeName::try_new("data").expect("valid volume name"),
                machine_id("machine_a"),
            ),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineVolumeRemoveRpcResponse::Ok(MachineVolumeRemoveRpcOk {
            machine_id: machine_id("machine_a"),
        })
    );
    assert_eq!(
        state.volume_removes(),
        vec!["ployz-n4-prod-v4-data".to_owned()]
    );
    assert_eq!(
        state.volume_remove_effects(),
        [RecordedVolumeRemoveEffect::DockerReference(
            "ployz-n4-prod-v4-data".to_owned(),
        )]
    );
}

#[tokio::test]
async fn machine_role_service_does_not_destroy_dataset_when_docker_remove_fails() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let runner = RecordingRunner::new(state.clone()).with_volume_remove_failure("volume is in use");
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        runner,
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let namespace_id = namespace_id("prod");
    let volume_name = VolumeName::try_new("data").expect("valid volume name");
    let dataset = DatasetName::for_volume(
        &ZfsPoolName::try_new("stored-pool").expect("valid pool"),
        &namespace_id,
        &volume_name,
    )
    .expect("valid stored dataset");
    let volume = VolumePinState::try_new(
        namespace_id,
        volume_name,
        machine_id("machine_a"),
        ployz_core::intent::VolumeKind::Provisioned {
            dataset,
            max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("valid quota"),
        },
    )
    .expect("valid provisioned pin");

    let response = request_json::<_, MachineVolumeRemoveRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::VolumeRemove,
        ),
        &MachineVolumeRemoveRpcRequest::ProvisionedDataset {
            operation_id: operation_id("op_123"),
            volume: ProvisionedVolumePinState::try_new(volume).expect("provisioned pin"),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineVolumeRemoveRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: MachineVolumeRemoveDomainError::DockerRemoveFailed {
                message: ployz_core::operation::FailureMessage::try_new(
                    "volume remove failed before dataset destroy: volume is in use",
                )
                .expect("valid failure message"),
            },
        }
    );
    assert_eq!(
        state.volume_remove_effects(),
        [RecordedVolumeRemoveEffect::DockerReference(
            "ployz-n4-prod-v4-data".to_owned(),
        )]
    );
    assert!(state.dataset_destroys().is_empty());
}

#[tokio::test]
async fn machine_role_service_destroys_the_exact_stored_dataset() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let namespace_id = namespace_id("prod");
    let volume_name = VolumeName::try_new("data").expect("valid volume name");
    let dataset = DatasetName::for_volume(
        &ZfsPoolName::try_new("stored-pool").expect("valid pool"),
        &namespace_id,
        &volume_name,
    )
    .expect("valid stored dataset");
    let volume = VolumePinState::try_new(
        namespace_id,
        volume_name,
        machine_id("machine_a"),
        ployz_core::intent::VolumeKind::Provisioned {
            dataset: dataset.clone(),
            max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("valid quota"),
        },
    )
    .expect("valid provisioned pin");

    let response = request_json::<_, MachineVolumeRemoveRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::VolumeRemove,
        ),
        &MachineVolumeRemoveRpcRequest::ProvisionedDataset {
            operation_id: operation_id("op_123"),
            volume: ProvisionedVolumePinState::try_new(volume).expect("provisioned pin"),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineVolumeRemoveRpcResponse::Ok(MachineVolumeRemoveRpcOk {
            machine_id: machine_id("machine_a"),
        })
    );
    assert_eq!(state.dataset_destroys(), vec![dataset.clone()]);
    assert_eq!(
        state.volume_removes(),
        vec!["ployz-n4-prod-v4-data".to_owned()]
    );
    assert_eq!(
        state.volume_remove_effects(),
        [
            RecordedVolumeRemoveEffect::DockerReference("ployz-n4-prod-v4-data".to_owned(),),
            RecordedVolumeRemoveEffect::ProvisionedDataset(dataset),
        ]
    );
}

#[tokio::test]
async fn machine_role_service_rejects_wrong_machine() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let subject = machine_service(
        &machine_id("machine_a"),
        MachineServiceEndpoint::VolumeRemove,
    );

    let wrong_machine = request_json::<_, MachineVolumeRemoveRpcResponse>(
        &nats.client,
        subject.clone(),
        &MachineVolumeRemoveRpcRequest::DockerReference {
            operation_id: operation_id("op_wrong_machine"),
            volume: VolumePinState::plain(
                namespace_id("prod"),
                VolumeName::try_new("data").expect("valid volume name"),
                machine_id("machine_b"),
            ),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");
    assert_eq!(
        wrong_machine,
        MachineVolumeRemoveRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: MachineVolumeRemoveDomainError::MachineMismatch {
                expected_machine_id: machine_id("machine_b"),
                responder_machine_id: machine_id("machine_a"),
            },
        }
    );

    drop(subject);
    assert!(state.volume_removes().is_empty());
    assert!(state.dataset_destroys().is_empty());
}

#[tokio::test]
async fn machine_role_service_publishes_removed_delta_after_remove() {
    let nats = test_nats().await;
    let mut deltas = nats
        .client
        .subscribe(machine_container_facts(&machine_id("machine_a")))
        .await
        .expect("subscribe deltas");
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client.clone());

    client
        .remove_container(
            &machine_id("machine_a"),
            MachineContainerRemoveRpcRequest {
                operation_id: operation_id("op_123"),
                container_id: container_id("ctr_old"),
                expected_identity: managed_identity(),
            },
        )
        .await
        .expect("remove succeeds");

    match next_container_fact_delta(&mut deltas).await {
        MachineContainerFactDelta::ContainerRemoved {
            machine_id: observed_machine_id,
            container_id: observed_container_id,
            ..
        } => {
            assert_eq!(observed_machine_id, machine_id("machine_a"));
            assert_eq!(observed_container_id, container_id("ctr_old"));
        }
        other => panic!("unexpected delta: {other:?}"),
    }
}

#[tokio::test]
async fn machine_role_service_publishes_observed_delta_after_stop() {
    let nats = test_nats().await;
    let mut deltas = nats
        .client
        .subscribe(machine_container_facts(&machine_id("machine_a")))
        .await
        .expect("subscribe deltas");
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default()).with_existing(
            existing_container_with_state(
                "ctr_old",
                managed_identity(),
                ExistingManagedContainerState::StartableStopped,
            ),
        ),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client.clone());

    client
        .stop_container(
            &machine_id("machine_a"),
            MachineContainerStopRpcRequest {
                operation_id: operation_id("op_123"),
                container_id: container_id("ctr_old"),
                expected_identity: managed_identity(),
            },
        )
        .await
        .expect("stop succeeds");

    match next_container_fact_delta(&mut deltas).await {
        MachineContainerFactDelta::ContainerObserved { observation, .. } => {
            assert_eq!(observation.container_id, container_id("ctr_old"));
            assert_eq!(observation.state, ContainerRuntimeState::Exited);
        }
        other => panic!("unexpected delta: {other:?}"),
    }
}

#[tokio::test]
async fn machine_role_service_stops_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    client
        .stop_container(
            &machine_id("machine_a"),
            MachineContainerStopRpcRequest {
                operation_id: operation_id("op_123"),
                container_id: container_id("ctr_failed"),
                expected_identity: managed_identity(),
            },
        )
        .await
        .expect("container stop succeeds");

    assert_eq!(state.stops(), vec![container_id("ctr_failed")]);
}

#[tokio::test]
async fn machine_role_service_restarts_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineContainerRestartRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::ContainerRestart,
        ),
        &MachineContainerRestartRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_running"),
            expected_identity: managed_identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineContainerRestartRpcResponse::Ok(MachineContainerRpcOk {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_running"),
        })
    );
    assert_eq!(state.restarts(), vec![container_id("ctr_running")]);
}

#[tokio::test]
async fn machine_role_service_reports_restart_failure_as_domain_error() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_restart_failure("ctr_running", "permission denied"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineContainerRestartRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::ContainerRestart,
        ),
        &MachineContainerRestartRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_running"),
            expected_identity: managed_identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineContainerRestartRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: MachineContainerRestartDomainError::RestartFailed {
                container_id: container_id("ctr_running"),
                message: failure_message("container restart failed: permission denied"),
                inspect_hint: inspect_hint("ctr_running"),
            },
        }
    );
}

#[tokio::test]
async fn machine_role_service_reports_remove_failure_as_domain_error() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_remove_failure("ctr_old", "busy"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineContainerRemoveRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::ContainerRemove,
        ),
        &MachineContainerRemoveRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_old"),
            expected_identity: managed_identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineContainerRemoveRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: MachineContainerRemoveDomainError::RemoveFailed {
                container_id: container_id("ctr_old"),
                message: failure_message("container remove failed: busy"),
                inspect_hint: inspect_hint("ctr_old"),
            },
        }
    );
}

#[tokio::test]
async fn machine_role_service_reports_stop_failure_as_domain_error() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_stop_failure("ctr_failed", "permission denied"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineContainerStopRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::ContainerStop,
        ),
        &MachineContainerStopRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_failed"),
            expected_identity: managed_identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineContainerStopRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: MachineContainerStopDomainError::StopFailed {
                container_id: container_id("ctr_failed"),
                message: failure_message("container stop failed: permission denied"),
                inspect_hint: inspect_hint("ctr_failed"),
            },
        }
    );
}

#[tokio::test]
async fn machine_role_service_tails_container_logs() {
    let nats = test_nats().await;
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_existing(existing_container("ctr_failed", managed_identity())),
        ready_wireguard_ebpf(),
        RecordingLogReader::new("ctr_failed", "panic: missing DATABASE_URL\n"),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineLogsTailRpcResponse>(
        &nats.client,
        machine_service(&machine_id("machine_a"), MachineServiceEndpoint::LogsTail),
        &MachineLogsTailRpcRequest {
            container_id: container_id("ctr_failed"),
            tail_lines: Some(50),
            since_unix_seconds: None,
            timestamps: false,
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineLogsTailRpcResponse::Ok(MachineLogsTailRpcOk {
            value: crate::roles::machine::protocol::MachineLogsTailResult {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_failed"),
                text: "panic: missing DATABASE_URL\n".to_owned(),
                truncated: false,
            },
        })
    );
}

#[tokio::test]
async fn machine_dataplane_status_service_returns_local_testimony() {
    let nats = test_nats().await;
    let expected = machine_dataplane_status();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(RecordingWireGuardEbpfState::default()),
        idle_logs(),
    )
    .await
    .expect("machine dataplane status service starts");

    let response = request_json::<_, MachineDataplaneStatusRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::DataplaneStatus,
        ),
        &MachineDataplaneStatusRpcRequest {
            mode: ployz_core::network::NetworkStatusMode::ProbePathMtu,
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine dataplane status responds");

    assert_eq!(
        response,
        MachineDataplaneStatusRpcResponse::Ok(MachineDataplaneStatusRpcOk {
            machine_id: machine_id("machine_a"),
            value: expected,
        })
    );
}

#[tokio::test]
async fn machine_public_key_service_answers_empty_query() {
    let nats = test_nats().await;
    let state = RecordingWireGuardEbpfState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(state.clone()),
        idle_logs(),
    )
    .await
    .expect("machine wireguard ebpf service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let response = request_json::<_, MachineDataplanePublicKeyRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::DataplanePublicKey,
        ),
        &MachineDataplanePublicKeyRpcRequest {},
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert!(matches!(
        response,
        MachineDataplanePublicKeyRpcResponse::Ok(response)
            if response.machine_id == self::machine_id("machine_a")
                && response.public_key.as_str() == "test-public-key"
    ));
    assert_eq!(state.prepare_count(), 0);
}

#[tokio::test]
async fn machine_volume_ensure_requires_the_subject_machine_and_runs_once() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a.flush().await.expect("flush subscription");
    let volume = VolumePinState::plain(
        namespace_id("default"),
        VolumeName::try_new("data").expect("volume"),
        machine_id("machine_a"),
    );
    let response = request_json::<_, MachineVolumeEnsureRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::VolumeEnsure,
        ),
        &MachineVolumeEnsureRpcRequest {
            volume: volume.clone(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine responds");
    assert_eq!(
        response,
        MachineVolumeEnsureRpcResponse::Ok(MachineVolumeEnsureRpcOk {
            machine_id: machine_id("machine_a"),
        })
    );
    assert_eq!(state.volume_ensures(), vec![volume]);

    let wrong_machine = VolumePinState::plain(
        namespace_id("default"),
        VolumeName::try_new("other").expect("volume"),
        machine_id("machine_b"),
    );
    let response = request_json::<_, MachineVolumeEnsureRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::VolumeEnsure,
        ),
        &MachineVolumeEnsureRpcRequest {
            volume: wrong_machine,
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine responds");
    assert_eq!(
        response,
        MachineVolumeEnsureRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: VolumeEnsureFailure::MachineMismatch {
                expected_machine_id: machine_id("machine_b"),
                responder_machine_id: machine_id("machine_a"),
            },
        }
    );
    assert_eq!(state.volume_ensures().len(), 1);
}

#[derive(Clone, Default)]
struct RecordingRunnerState {
    inner: Arc<Mutex<RecordingRunnerInner>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordedVolumeRemoveEffect {
    DockerReference(String),
    ProvisionedDataset(ployz_core::deploy::DatasetName),
}

async fn next_container_fact_delta(
    subscriber: &mut async_nats::Subscriber,
) -> MachineContainerFactDelta {
    let message = tokio::time::timeout(Duration::from_secs(1), subscriber.next())
        .await
        .expect("delta arrives")
        .expect("delta subscription stays open");
    serde_json::from_slice(&message.payload).expect("delta decodes")
}

impl RecordingRunnerState {
    fn creates(&self) -> Vec<CreateManagedContainer> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .creates
            .clone()
    }

    fn starts(&self) -> Vec<ContainerId> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .starts
            .clone()
    }

    fn restarts(&self) -> Vec<ContainerId> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .restarts
            .clone()
    }

    fn removes(&self) -> Vec<ContainerId> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .removes
            .clone()
    }

    fn volume_removes(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .volume_removes
            .clone()
    }

    fn dataset_destroys(&self) -> Vec<ployz_core::deploy::DatasetName> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .dataset_destroys
            .clone()
    }

    fn volume_remove_effects(&self) -> Vec<RecordedVolumeRemoveEffect> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .volume_remove_effects
            .clone()
    }

    fn volume_ensures(&self) -> Vec<VolumePinState> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .volume_ensures
            .clone()
    }

    fn stops(&self) -> Vec<ContainerId> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .stops
            .clone()
    }
}

#[derive(Default)]
struct RecordingRunnerInner {
    endpoint_networks: usize,
    creates: Vec<CreateManagedContainer>,
    starts: Vec<ContainerId>,
    restarts: Vec<ContainerId>,
    stops: Vec<ContainerId>,
    removes: Vec<ContainerId>,
    volume_removes: Vec<String>,
    dataset_destroys: Vec<ployz_core::deploy::DatasetName>,
    volume_remove_effects: Vec<RecordedVolumeRemoveEffect>,
    volume_ensures: Vec<VolumePinState>,
}

#[derive(Clone)]
struct RecordingRunner {
    state: RecordingRunnerState,
    existing: Vec<ExistingManagedContainer>,
    next_container: Option<ContainerId>,
    create_failure: Option<String>,
    start_failure: Option<(ContainerId, String)>,
    restart_failure: Option<(ContainerId, String)>,
    stop_failure: Option<(ContainerId, String)>,
    remove_failure: Option<(ContainerId, String)>,
    volume_remove_failure: Option<String>,
    wait_exit_code: i64,
}

impl RecordingRunner {
    fn new(state: RecordingRunnerState) -> Self {
        Self {
            state,
            existing: Vec::new(),
            next_container: None,
            create_failure: None,
            start_failure: None,
            restart_failure: None,
            stop_failure: None,
            remove_failure: None,
            volume_remove_failure: None,
            wait_exit_code: 0,
        }
    }

    fn with_existing(mut self, existing: ExistingManagedContainer) -> Self {
        self.existing.push(existing);
        self
    }

    fn with_next_container(mut self, container_id: &str) -> Self {
        self.next_container = Some(self::container_id(container_id));
        self
    }

    fn with_create_failure(mut self, message: &str) -> Self {
        self.create_failure = Some(message.to_owned());
        self
    }

    fn with_start_failure(mut self, container_id: &str, message: &str) -> Self {
        self.next_container = Some(self::container_id(container_id));
        self.start_failure = Some((self::container_id(container_id), message.to_owned()));
        self
    }

    fn with_existing_start_failure(mut self, container_id: &str, message: &str) -> Self {
        self.existing.push(existing_container_with_state(
            container_id,
            managed_identity(),
            ExistingManagedContainerState::StartableStopped,
        ));
        self.start_failure = Some((self::container_id(container_id), message.to_owned()));
        self
    }

    fn with_remove_failure(mut self, container_id: &str, message: &str) -> Self {
        self.remove_failure = Some((self::container_id(container_id), message.to_owned()));
        self
    }

    fn with_volume_remove_failure(mut self, message: &str) -> Self {
        self.volume_remove_failure = Some(message.to_owned());
        self
    }

    fn with_stop_failure(mut self, container_id: &str, message: &str) -> Self {
        self.stop_failure = Some((self::container_id(container_id), message.to_owned()));
        self
    }

    fn with_restart_failure(mut self, container_id: &str, message: &str) -> Self {
        self.restart_failure = Some((self::container_id(container_id), message.to_owned()));
        self
    }

    fn with_wait_exit_code(mut self, exit_code: i64) -> Self {
        self.wait_exit_code = exit_code;
        self
    }
}

impl crate::roles::machine::runner::MachineImageRemovalRunner for RecordingRunner {
    async fn remove_image(
        &self,
        _image_identity: &ployz_core::image::OciDigest,
    ) -> Result<ployz_core::image::ImageRemoveOutcome, String> {
        Ok(ployz_core::image::ImageRemoveOutcome::AlreadyAbsent)
    }
}

impl MachineContainerRunner for RecordingRunner {
    async fn ensure_volume(
        &self,
        volume: &ployz_core::intent::VolumePinState,
    ) -> Result<(), ployz_core::machine::VolumeEnsureFailure> {
        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .volume_ensures
            .push(volume.clone());
        Ok(())
    }

    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError> {
        Ok(self.existing.clone())
    }

    async fn ensure_endpoint_network(&self) -> Result<(), MachineContainerRunnerError> {
        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .endpoint_networks += 1;
        Ok(())
    }

    async fn ensure_projection_endpoint_network(
        &self,
        _expected_subnet: &ployz_core::network::MachineEndpointSubnet,
    ) -> Result<(), MachineContainerRunnerError> {
        self.ensure_endpoint_network().await
    }

    async fn read_endpoint_network_status(&self) -> ployz_core::network::EndpointBridgeStatus {
        ployz_core::network::EndpointBridgeStatus::Missing
    }

    async fn resolve_registry_image(
        &self,
        reference: &ployz_core::deploy::ImageReference,
        _credential: Option<&ployz_core::deploy::RegistryCredential>,
    ) -> Result<ployz_core::image::OciDigest, MachineContainerRunnerError> {
        Ok(ployz_core::image::OciDigest::sha256(
            reference.as_str().as_bytes(),
        ))
    }

    async fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> Result<ContainerId, MachineContainerRunnerError> {
        if let Some(message) = self.create_failure.clone() {
            return Err(MachineContainerRunnerError::Create { message });
        }

        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .creates
            .push(command);
        self.next_container
            .clone()
            .ok_or_else(|| MachineContainerRunnerError::Create {
                message: "missing next container id".to_owned(),
            })
    }

    async fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), MachineContainerRunnerError> {
        if let Some((failed_container_id, message)) = self.start_failure.clone()
            && failed_container_id == *container_id
        {
            return Err(MachineContainerRunnerError::Start {
                container_id: container_id.clone(),
                message,
            });
        }

        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .starts
            .push(container_id.clone());
        Ok(())
    }

    async fn wait_managed_container(
        &self,
        _container_id: &ContainerId,
    ) -> Result<i64, MachineContainerRunnerError> {
        Ok(self.wait_exit_code)
    }

    async fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        _expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        if let Some((failed_container_id, message)) = self.remove_failure.clone()
            && failed_container_id == *container_id
        {
            return Err(MachineContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message,
            });
        }

        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .removes
            .push(container_id.clone());
        Ok(())
    }

    async fn remove_volume(
        &self,
        docker_volume_name: &str,
    ) -> Result<(), MachineContainerRunnerError> {
        let mut state = self
            .state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned");
        state.volume_removes.push(docker_volume_name.to_owned());
        state
            .volume_remove_effects
            .push(RecordedVolumeRemoveEffect::DockerReference(
                docker_volume_name.to_owned(),
            ));
        if let Some(message) = &self.volume_remove_failure {
            return Err(MachineContainerRunnerError::RemoveVolume {
                docker_volume_name: docker_volume_name.to_owned(),
                message: message.clone(),
            });
        }
        Ok(())
    }

    async fn destroy_provisioned_dataset(
        &self,
        dataset: &ployz_core::deploy::DatasetName,
    ) -> Result<(), ployz_core::storage::StorageEffectFailure> {
        let mut state = self
            .state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned");
        state.dataset_destroys.push(dataset.clone());
        state
            .volume_remove_effects
            .push(RecordedVolumeRemoveEffect::ProvisionedDataset(
                dataset.clone(),
            ));
        Ok(())
    }

    async fn stop_managed_container(
        &self,
        container_id: &ContainerId,
        _expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        if let Some((failed_container_id, message)) = self.stop_failure.clone()
            && failed_container_id == *container_id
        {
            return Err(MachineContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message,
            });
        }

        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .stops
            .push(container_id.clone());
        Ok(())
    }

    async fn restart_managed_container(
        &self,
        container_id: &ContainerId,
        _expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        if let Some((failed_container_id, message)) = self.restart_failure.clone()
            && failed_container_id == *container_id
        {
            return Err(MachineContainerRunnerError::Restart {
                container_id: container_id.clone(),
                message,
            });
        }

        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .restarts
            .push(container_id.clone());
        Ok(())
    }
}

#[derive(Clone, Default)]
struct RecordingWireGuardEbpfState {
    inner: Arc<Mutex<RecordingWireGuardEbpfInner>>,
}

impl RecordingWireGuardEbpfState {
    fn prepare_count(&self) -> usize {
        self.inner
            .lock()
            .expect("recording wireguard ebpf lock is not poisoned")
            .prepare_count
    }
}

#[derive(Default)]
struct RecordingWireGuardEbpfInner {
    prepare_count: usize,
    endpoint_routes: Vec<ployz_core::network::WireGuardEbpfEndpointRoute>,
    peers: Vec<ployz_core::network::WireGuardPeer>,
}

#[derive(Clone)]
struct RecordingWireGuardEbpf {
    state: RecordingWireGuardEbpfState,
    failure: Option<WireGuardEbpfPrepareError>,
}

impl RecordingWireGuardEbpf {
    fn new(state: RecordingWireGuardEbpfState) -> Self {
        Self {
            state,
            failure: None,
        }
    }
}

impl LocalWireGuardEbpfPreparer for RecordingWireGuardEbpf {
    async fn read_ployz_native_mesh_status(
        &self,
        _mode: ployz_core::network::NetworkStatusMode,
    ) -> Result<MachineDataplaneStatus, String> {
        Ok(machine_dataplane_status())
    }

    async fn read_wireguard_public_key(
        &self,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        Ok(WireGuardPublicKey::try_new("test-public-key").expect("test public key is valid"))
    }

    async fn prepare_ployz_native_mesh(
        &self,
        endpoint_routes: &[ployz_core::network::WireGuardEbpfEndpointRoute],
        peers: &[ployz_core::network::WireGuardPeer],
    ) -> Result<PloyzNativeMeshReady, WireGuardEbpfPrepareError> {
        let mut state = self
            .state
            .inner
            .lock()
            .expect("recording wireguard ebpf lock is not poisoned");
        state.prepare_count += 1;
        state.endpoint_routes = endpoint_routes.to_vec();
        state.peers = peers.to_vec();
        drop(state);

        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(ready_components()),
        }
    }

    async fn prepare_wireguard(
        &self,
        endpoint_routes: &[ployz_core::network::WireGuardEbpfEndpointRoute],
        peers: &[ployz_core::network::WireGuardPeer],
    ) -> Result<WireGuardReady, WireGuardEbpfPrepareError> {
        self.prepare_ployz_native_mesh(endpoint_routes, peers)
            .await
            .map(|ready| ready.wireguard)
    }
}

fn machine_dataplane_status() -> MachineDataplaneStatus {
    let mut projection = ployz_core::network::NativeDataplaneProjectionStatus::unavailable(
        ployz_core::operation::FailureMessage::try_new("dataplane projection has not been fetched")
            .expect("failure message"),
    );
    projection.endpoint_bridge = ployz_core::network::EndpointBridgeStatus::Missing;
    MachineDataplaneStatus {
        projection,
        wireguard: WireGuardStatus {
            interface: "ployz-wg0".to_owned(),
            configured_mtu: WireGuardConfiguredMtu::Auto,
            detected_mtu: WireGuardDetectedMtu::Detected { mtu: 1420 },
            interface_mtu: WireGuardInterfaceMtu::Detected { mtu: 1420 },
            peers: Vec::new(),
        },
        ebpf_attachment: EbpfAttachmentStatus::Attached,
    }
}

fn ready_components() -> PloyzNativeMeshReady {
    PloyzNativeMeshReady {
        wireguard: WireGuardReady {
            public_key: WireGuardPublicKey::try_new("test-public-key")
                .expect("test public key is valid"),
            evidence: vec![WireGuardReadyEvidence::Command {
                program: "wg".to_owned(),
                args: vec!["--version".to_owned()],
            }],
        },
        ebpf_forwarding: EbpfForwardingReady {
            evidence: vec![EbpfForwardingReadyEvidence::PloyzTcBytecode {
                path: "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".to_owned(),
                symbols: vec!["ployz_egress".to_owned(), "ployz_ingress".to_owned()],
            }],
        },
    }
}

fn idle_runner() -> RecordingRunner {
    RecordingRunner::new(RecordingRunnerState::default())
}

fn idle_logs() -> RecordingLogReader {
    RecordingLogReader::empty()
}

fn ready_wireguard_ebpf() -> RecordingWireGuardEbpf {
    RecordingWireGuardEbpf::new(RecordingWireGuardEbpfState::default())
}

#[derive(Clone)]
struct RecordingLogReader {
    container_id: Option<ContainerId>,
    text: String,
}

impl RecordingLogReader {
    fn empty() -> Self {
        Self {
            container_id: None,
            text: String::new(),
        }
    }

    fn new(container_id: &str, text: &str) -> Self {
        Self {
            container_id: Some(self::container_id(container_id)),
            text: text.to_owned(),
        }
    }
}

impl MachineLogReader for RecordingLogReader {
    async fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        _query: crate::roles::machine::runner::MachineLogQuery,
    ) -> Result<MachineLogTail, MachineLogReaderError> {
        match &self.container_id {
            Some(expected) if expected == container_id => Ok(MachineLogTail {
                text: self.text.clone(),
                truncated: false,
            }),
            _ => Err(MachineLogReaderError::NotFound {
                container_id: container_id.clone(),
            }),
        }
    }
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    /// Controller principal: the requesting deploy-worker side.
    client: async_nats::Client,
    /// Machine principal: the machine-runtime service side.
    machine_a: async_nats::Client,
}

async fn test_nats() -> TestNats {
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let client = nats.controller.clone();
    let machine_a = nats.machine_client(&machine_id("machine_a")).await;

    TestNats {
        _nats: nats,
        client,
        machine_a,
    }
}

fn run_request() -> MachineContainerRunRpcRequest {
    MachineContainerRunRpcRequest {
        pull: MachineImagePull::Registry {
            credential: None,
            reference: image("registry.example/api:rev_2"),
        },
        runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
        provisioned_volumes: Vec::new(),
        container: managed_container_spec(),
    }
}

fn hook_request() -> MachineContainerRunHookRpcRequest {
    let mut container = managed_container_spec();
    container.step_id = ployz_core::ids::StepId::try_new("pre_start").expect("valid step id");
    container.kind = ManagedContainerKind::Predeploy;
    MachineContainerRunHookRpcRequest {
        pull: MachineImagePull::Registry {
            credential: None,
            reference: image("registry.example/api:rev_2"),
        },
        runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
        provisioned_volumes: Vec::new(),
        container,
        timeout_millis: 1_000,
    }
}

fn inspect_hint(container_id: &str) -> ployz_core::operation::OperatorHint {
    ployz_core::operation::OperatorHint::try_new(format!("ployz container inspect {container_id}"))
        .expect("valid inspect hint")
}

fn managed_container_spec() -> ManagedContainerIdentity {
    containers::identity("svc_api")
        .entry("entry_2")
        .operation("op_123")
        .step("run_1")
        .build()
}

fn existing_container(
    container_id: &str,
    labels: ManagedContainerIdentity,
) -> ExistingManagedContainer {
    existing_container_with_state(
        container_id,
        labels,
        ExistingManagedContainerState::Running {
            ip: None,
            health: ployz_core::machine::runtime::ContainerHealth::None,
            started_at_unix_ms: None,
        },
    )
}

fn existing_container_with_state(
    container_id: &str,
    labels: ManagedContainerIdentity,
    state: ExistingManagedContainerState,
) -> ExistingManagedContainer {
    ExistingManagedContainer {
        container_id: self::container_id(container_id),
        identity: labels,
        state,
        health_status: None,
        resolved_image_identity: None,
        created_at_unix_seconds: None,
    }
}

fn managed_identity() -> ManagedContainerIdentity {
    containers::identity("svc_api")
        .entry("entry_2")
        .operation("op_123")
        .step("run_1")
        .build()
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

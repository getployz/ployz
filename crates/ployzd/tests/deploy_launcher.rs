#[allow(dead_code)]
#[path = "deploy_operation/fixtures.rs"]
mod fixtures;

use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use fixtures::*;
use ployz_core::deploy::{DeployRequest, ReplicaCount};
use ployz_core::ops::{
    DeployOperationFailure, DeployOperationState, OperationLeaseDurationSeconds, OperationStatus,
};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::observations::{AsyncNatsObservationStore, KV_OBS_BUCKET};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationStatusStore, KV_OPS_BUCKET, PLZ_OPS_STREAM,
};
use ployzd::controllers::{DeploySubmitCommand, IdempotencyKey, OperationControllers};
use ployzd::deploy_launcher::{
    DeployLaunchError, DeployLaunchPorts, DeployLaunchStores, run_deploy_operation,
};
use ployzd::deploy_worker::DeployExecutionNodeScope;
use ployzd::operation_lease::OperationLeasePolicy;
use std::time::Duration;

#[tokio::test]
async fn accepted_deploy_launches_from_nats_facts_and_commits_active_state() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");
    let controllers = operation_controllers(&nats.jetstream).await;
    let deploy_request = deploy_request(1);
    let accepted = controllers
        .submit_deploy(deploy_submit_command(deploy_request))
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();

    let outcome = run_deploy_operation(
        accepted,
        DeployExecutionNodeScope::same_nodes(vec![node_id("node_a")]),
        DeployLaunchStores {
            core_state: core_state.clone(),
            observations,
            controllers: controllers.clone(),
        },
        DeployLaunchPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect("accepted deploy launches");

    assert_eq!(outcome.service_id, service_id("svc_api"));
    assert_eq!(outcome.target_revision, revision_id("rev_2"));
    assert_eq!(runtime.requests.len(), 1);
    let [run_request] = runtime.requests.as_slice() else {
        panic!("expected one container run request");
    };
    assert_eq!(run_request.node_id, node_id("node_a"));
    assert_eq!(run_request.container.operation_id, operation_id("op_123"));
    assert_eq!(
        core_state
            .active_service(&service_id("svc_api"))
            .await
            .expect("active state reads")
            .expect("active state committed")
            .active_revision,
        revision_id("rev_2")
    );
    assert!(matches!(
        controllers
            .operation_status(&operation_id("op_123"))
            .await
            .expect("operation status reads"),
        Some(OperationStatus::Deploy {
            state: DeployOperationState::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn health_failure_records_failed_operation_without_committing_active_state() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");
    let controllers = operation_controllers(&nats.jetstream).await;
    let deploy_request = deploy_request(1);
    let accepted = controllers
        .submit_deploy(deploy_submit_command(deploy_request))
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::unhealthy("node_a", "ctr_1");

    let error = run_deploy_operation(
        accepted,
        DeployExecutionNodeScope::same_nodes(vec![node_id("node_a")]),
        DeployLaunchStores {
            core_state: core_state.clone(),
            observations,
            controllers: controllers.clone(),
        },
        DeployLaunchPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("health failure fails deploy");

    assert!(matches!(error, DeployLaunchError::Execute(_)));
    assert!(
        core_state
            .active_service(&service_id("svc_api"))
            .await
            .expect("active state reads")
            .is_none()
    );
    assert!(matches!(
        controllers
            .operation_status(&operation_id("op_123"))
            .await
            .expect("operation status reads"),
        Some(OperationStatus::Deploy {
            state:
                DeployOperationState::Failed {
                    failure:
                        DeployOperationFailure::HealthCheckFailed {
                            retained_artifacts,
                            ..
                        },
                },
            ..
        }) if retained_artifacts == vec![retained_container("node_a", "ctr_1")]
    ));
}

#[tokio::test]
async fn fact_load_failure_marks_accepted_operation_failed() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let controllers = operation_controllers(&nats.jetstream).await;
    let deploy_request = deploy_request(1);
    let accepted = controllers
        .submit_deploy(deploy_submit_command(deploy_request))
        .await
        .expect("deploy operation accepted");
    let missing_observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");
    nats.jetstream
        .delete_key_value(KV_OBS_BUCKET)
        .await
        .expect("delete observations bucket");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        accepted,
        DeployExecutionNodeScope::same_nodes(vec![node_id("node_a")]),
        DeployLaunchStores {
            core_state,
            observations: missing_observations,
            controllers: controllers.clone(),
        },
        DeployLaunchPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("fact load fails");

    assert!(matches!(
        error,
        DeployLaunchError::LoadFacts {
            failure_record_error: None,
            ..
        }
    ));
    assert!(runtime.requests.is_empty());
    assert!(matches!(
        controllers
            .operation_status(&operation_id("op_123"))
            .await
            .expect("operation status reads"),
        Some(OperationStatus::Deploy {
            state:
                DeployOperationState::Failed {
                    failure:
                        DeployOperationFailure::PlanningFailed {
                            service_id: failed_service_id,
                            revision_id: failed_revision_id,
                            ..
                        },
                },
            ..
        }) if failed_service_id == service_id("svc_api")
            && failed_revision_id == revision_id("rev_2")
    ));
}

#[tokio::test]
async fn duplicate_submit_without_ownership_does_not_launch_runtime_side_effects() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");
    let owner_a = operation_controllers_with_owner(&nats.jetstream, "control_a").await;
    let owner_b = operation_controllers_with_owner(&nats.jetstream, "control_b").await;
    let deploy_request = deploy_request(1);
    let first = owner_a
        .submit_deploy(deploy_submit_command(deploy_request.clone()))
        .await
        .expect("first deploy operation accepted");
    let duplicate = owner_b
        .submit_deploy(deploy_submit_command(deploy_request))
        .await
        .expect("duplicate deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        duplicate,
        DeployExecutionNodeScope::same_nodes(vec![node_id("node_a")]),
        DeployLaunchStores {
            core_state,
            observations,
            controllers: owner_b,
        },
        DeployLaunchPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("non-owner cannot launch");

    assert!(matches!(
        error,
        DeployLaunchError::LeaseNotHeld(
            ployzd::deploy_launcher::DeployLaunchLeaseError::NoCurrentLease {
                operation_id: current_operation_id,
                expected_owner,
            }
        ) if current_operation_id == operation_id("op_123")
            && expected_owner == operation_owner_id("control_b")
    ));
    assert_eq!(first.operation_id, operation_id("op_123"));
    assert!(runtime.requests.is_empty());
}

#[tokio::test]
async fn expired_accepted_lease_does_not_launch_runtime_side_effects() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");
    let controllers = operation_controllers_with_policy(
        &nats.jetstream,
        "control_a",
        OperationLeasePolicy::try_new(
            OperationLeaseDurationSeconds::try_new(1).expect("valid lease duration"),
            Duration::from_millis(100),
        )
        .expect("valid lease policy"),
    )
    .await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(deploy_request(1)))
        .await
        .expect("deploy operation accepted");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        accepted,
        DeployExecutionNodeScope::same_nodes(vec![node_id("node_a")]),
        DeployLaunchStores {
            core_state,
            observations,
            controllers,
        },
        DeployLaunchPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("expired accepted lease cannot launch");

    assert!(matches!(
        error,
        DeployLaunchError::LeaseNotHeld(
            ployzd::deploy_launcher::DeployLaunchLeaseError::NoCurrentLease { .. }
        )
    ));
    assert!(runtime.requests.is_empty());
}

struct TestNats {
    _server: nats_server::Server,
    jetstream: jetstream::Context,
}

async fn test_nats() -> TestNats {
    let config = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ployz-nats/tests/configs/jetstream.conf"
    );
    let server = nats_server::run_server(config);
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let jetstream = jetstream::new(client);
    bootstrap_nats(&jetstream).await;

    TestNats {
        _server: server,
        jetstream,
    }
}

async fn bootstrap_nats(jetstream: &jetstream::Context) {
    jetstream
        .create_stream(jetstream::stream::Config {
            name: PLZ_OPS_STREAM.to_owned(),
            subjects: vec!["plz.v1.op.>".to_owned()],
            storage: StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("create PLZ_OPS stream");
    for bucket in [KV_CORE_BUCKET, KV_OBS_BUCKET, KV_OPS_BUCKET] {
        jetstream
            .create_key_value(jetstream::kv::Config {
                bucket: bucket.to_owned(),
                storage: StorageType::Memory,
                ..Default::default()
            })
            .await
            .expect("create KV bucket");
    }
}

async fn operation_controllers(jetstream: &jetstream::Context) -> OperationControllers {
    operation_controllers_with_owner(jetstream, "control_a").await
}

async fn operation_controllers_with_owner(
    jetstream: &jetstream::Context,
    owner_id: &str,
) -> OperationControllers {
    operation_controllers_with_policy(jetstream, owner_id, OperationLeasePolicy::default_policy())
        .await
}

async fn operation_controllers_with_policy(
    jetstream: &jetstream::Context,
    owner_id: &str,
    lease_policy: OperationLeasePolicy,
) -> OperationControllers {
    OperationControllers::with_owner_and_lease_policy(
        AsyncNatsOperationEventLog::new(jetstream.clone()),
        AsyncNatsOperationStatusStore::from_jetstream(jetstream)
            .await
            .expect("open operation status store"),
        operation_owner_id(owner_id),
        lease_policy,
    )
}

fn deploy_submit_command(target: DeployRequest) -> DeploySubmitCommand {
    DeploySubmitCommand {
        operation_id: operation_id("op_123"),
        idempotency_key: IdempotencyKey::try_new("deploy_svc_api_rev_2")
            .expect("valid idempotency key"),
        target,
    }
}

fn deploy_request(replicas: u16) -> DeployRequest {
    DeployRequest {
        service_id: service_id("svc_api"),
        target_revision: revision_id("rev_2"),
        image: image("registry.example/api:rev_2"),
        replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        route: None,
    }
}

fn operation_owner_id(value: &str) -> ployz_core::ids::OperationOwnerId {
    ployz_core::ids::OperationOwnerId::try_new(value).expect("valid operation owner id")
}

#[allow(dead_code)]
#[path = "deploy_operation/fixtures.rs"]
mod fixtures;

use async_nats::jetstream;
use fixtures::*;
use futures_util::StreamExt;
use ployz_core::deploy::{DeployRequest, ReplicaCount};
use ployz_core::install::MachineBootstrapUrl;
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationFailure, DeployOperationState,
    OperationLeaseDurationSeconds, OperationStatus,
};
use ployz_core::subjects::{NodeServiceEndpoint, node_service};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::{AsyncNatsObservationStore, KV_OBS_BUCKET};
use ployz_nats::operations::{AsyncNatsOperationEventLog, AsyncNatsOperationStatusStore};
use ployz_test_support::ids::owner_id as operation_owner_id;
use ployzd::config::DEFAULT_MACHINE_BOOTSTRAP_URL;
use ployzd::controllers::{
    DeploySubmitCommand, IdempotencyKey, MachineAddBootstrapConfig, OperationControllers,
};
use ployzd::deploy_runtime::{
    DeployOperationPorts, DeployOperationRunError, DeployOperationStores, run_deploy_operation,
};
use ployzd::deploy_worker::DeployExecutionNodeScope;
use ployzd::node::client::NatsNodeContainerRuntime;
use ployzd::node::protocol::{
    NodeEnsureEndpointNetworkRpcOk, NodeEnsureEndpointNetworkRpcResponse,
};
use ployzd::operation_lease::OperationLeasePolicy;
use std::time::Duration;

#[tokio::test]
async fn accepted_deploy_runs_from_nats_facts_and_commits_active_state() {
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
        DeployOperationStores {
            core_state: core_state.clone(),
            observations,
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect("accepted deploy runs");

    assert_eq!(outcome.service_id, service_id("svc_api"));
    assert_eq!(outcome.target_revision, revision_id("rev_2"));
    assert_eq!(runtime.requests.len(), 1);
    let [(run_node_id, run_request)] = runtime.requests.as_slice() else {
        panic!("expected one container run request");
    };
    assert_eq!(*run_node_id, node_id("node_a"));
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
            .repository()
            .records()
            .get(&operation_id("op_123"))
            .await
            .expect("operation status reads"),
        Some(OperationStatus::Deploy {
            state: DeployOperationState::Completed {
                outcome: DeployCompletionOutcome::Completed,
            },
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
        DeployOperationStores {
            core_state: core_state.clone(),
            observations,
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("health failure fails deploy");

    assert!(matches!(error, DeployOperationRunError::Execute(_)));
    assert!(
        core_state
            .active_service(&service_id("svc_api"))
            .await
            .expect("active state reads")
            .is_none()
    );
    assert!(matches!(
        controllers
            .repository().records().get(&operation_id("op_123"))
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
async fn missing_node_responder_marks_deploy_failed_without_committing_active_state() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");
    let controllers = operation_controllers(&nats.jetstream).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(deploy_request(1)))
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = NatsNodeContainerRuntime::new(nats.client.clone())
        .with_request_timeout(Duration::from_millis(200));
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        accepted,
        DeployExecutionNodeScope::same_nodes(vec![node_id("node_missing")]),
        DeployOperationStores {
            core_state: core_state.clone(),
            observations,
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("missing node responder fails deploy");

    assert!(matches!(error, DeployOperationRunError::Execute(_)));
    assert!(
        core_state
            .active_service(&service_id("svc_api"))
            .await
            .expect("active state reads")
            .is_none()
    );
    assert!(matches!(
        controllers
            .repository().records().get(&operation_id("op_123"))
            .await
            .expect("operation status reads"),
        Some(OperationStatus::Deploy {
            state:
                DeployOperationState::Failed {
                    failure:
                        DeployOperationFailure::RuntimeUnavailable {
                            node_id: failed_node_id,
                            message,
                            retained_artifacts,
                        },
                },
            ..
        }) if failed_node_id == node_id("node_missing")
            && message.as_str() == "node runtime has no responders"
            && retained_artifacts.is_empty()
    ));
}

#[tokio::test]
async fn node_service_timeout_marks_deploy_failed_without_committing_active_state() {
    let nats = test_nats().await;
    let _endpoint_network =
        start_endpoint_network_subscription(nats.node_slow.clone(), node_id("node_slow")).await;
    let _unresponsive_node =
        start_unresponsive_container_run_subscription(nats.node_slow.clone(), node_id("node_slow"))
            .await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");
    let controllers = operation_controllers(&nats.jetstream).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(deploy_request(1)))
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = NatsNodeContainerRuntime::new(nats.client.clone())
        .with_request_timeout(Duration::from_millis(50));
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        accepted,
        DeployExecutionNodeScope::same_nodes(vec![node_id("node_slow")]),
        DeployOperationStores {
            core_state: core_state.clone(),
            observations,
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("timed out node service fails deploy");

    assert!(matches!(error, DeployOperationRunError::Execute(_)));
    assert!(
        core_state
            .active_service(&service_id("svc_api"))
            .await
            .expect("active state reads")
            .is_none()
    );
    let status = controllers
        .repository()
        .records()
        .get(&operation_id("op_123"))
        .await
        .expect("operation status reads");
    assert!(
        matches!(
            status,
            Some(OperationStatus::Deploy {
                state:
                    DeployOperationState::Failed {
                        failure:
                            DeployOperationFailure::RuntimeUnavailable {
                                node_id: ref failed_node_id,
                                ref message,
                                ref retained_artifacts,
                            },
                    },
                ..
            }) if failed_node_id == &node_id("node_slow")
                && message.as_str() == "node runtime request timed out"
                && retained_artifacts.is_empty()
        ),
        "unexpected operation status: {status:?}"
    );
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
        DeployOperationStores {
            core_state,
            observations: missing_observations,
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
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
        DeployOperationRunError::LoadFacts {
            failure_record_error: None,
            ..
        }
    ));
    assert!(runtime.requests.is_empty());
    assert!(matches!(
        controllers
            .repository().records().get(&operation_id("op_123"))
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
async fn duplicate_submit_without_ownership_does_not_run_runtime_side_effects() {
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
        DeployOperationStores {
            core_state,
            observations,
            controllers: owner_b,
        },
        DeployOperationPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("non-owner cannot run");

    assert!(matches!(
        error,
        DeployOperationRunError::LeaseNotHeld(
            ployzd::operation_lease::OwnerLeaseError::NoCurrentLease {
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
async fn expired_accepted_lease_does_not_run_runtime_side_effects() {
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
        DeployOperationStores {
            core_state,
            observations,
            controllers,
        },
        DeployOperationPorts {
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("expired accepted lease cannot run");

    assert!(matches!(
        error,
        DeployOperationRunError::LeaseNotHeld(
            ployzd::operation_lease::OwnerLeaseError::NoCurrentLease { .. }
        )
    ));
    assert!(runtime.requests.is_empty());
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    /// Controller principal: the deploy-runtime side.
    client: async_nats::Client,
    /// Node principal for the stubbed slow node service.
    node_slow: async_nats::Client,
    jetstream: jetstream::Context,
}

async fn test_nats() -> TestNats {
    let nats = ployz_test_support::nats::TestNats::start_with_nodes(&[node_id("node_slow")]).await;
    nats.bootstrap_resources().await;
    let client = nats.controller.clone();
    let node_slow = nats.node_client(&node_id("node_slow")).await;
    let jetstream = nats.jetstream.clone();

    TestNats {
        _nats: nats,
        client,
        node_slow,
        jetstream,
    }
}

async fn start_unresponsive_container_run_subscription(
    client: async_nats::Client,
    node_id: ployz_core::ids::NodeId,
) -> tokio::task::JoinHandle<()> {
    let subject = node_service(&node_id, NodeServiceEndpoint::ContainerRun);
    let mut subscriber = client
        .subscribe(subject)
        .await
        .expect("subscribe unresponsive node service");
    client
        .flush()
        .await
        .expect("flush unresponsive node service subscription");
    tokio::spawn(async move {
        while subscriber.next().await.is_some() {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    })
}

async fn start_endpoint_network_subscription(
    client: async_nats::Client,
    node_id: ployz_core::ids::NodeId,
) -> tokio::task::JoinHandle<()> {
    let subject = node_service(
        &node_id,
        NodeServiceEndpoint::ContainerEnsureEndpointNetwork,
    );
    let mut subscriber = client
        .subscribe(subject)
        .await
        .expect("subscribe endpoint network service");
    client
        .flush()
        .await
        .expect("flush endpoint network service subscription");
    tokio::spawn(async move {
        while let Some(message) = subscriber.next().await {
            if let Some(reply) = message.reply {
                let response = serde_json::to_vec(&NodeEnsureEndpointNetworkRpcResponse::Ok(
                    NodeEnsureEndpointNetworkRpcOk {
                        node_id: node_id.clone(),
                    },
                ))
                .expect("endpoint network response serializes");
                let _ = client.publish(reply, response.into()).await;
            }
        }
    })
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
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
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

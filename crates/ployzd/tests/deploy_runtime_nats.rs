use std::sync::Arc;
use std::sync::atomic::AtomicBool;
#[allow(dead_code)]
#[path = "deploy_operation/fixtures.rs"]
mod fixtures;

use async_nats::jetstream;
use fixtures::*;
use futures_util::StreamExt;
use ployz_core::deploy::{DeployRequest, DeployServiceSpec, ReplicaCount};
use ployz_core::install::MachineBootstrapUrl;
use ployz_core::machine_runtime::{
    MachineContainerObservationSnapshot, MachineFactsRole, MachineFactsSnapshot,
};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationFailure, DeployOperationState,
    MachineSubstrateVersions, OperationStatus,
};
use ployz_core::state::MachineLifecycle;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::operations::{AsyncNatsOperationEventLog, AsyncNatsOperationStatusStore};
use ployz_test_support::ids::idempotency_key;
use ployzd::config::DEFAULT_MACHINE_BOOTSTRAP_URL;
use ployzd::controllers::{
    DeploySubmitCommand, MachineAddBootstrapConfig, OperationControllers, SubmitCommandError,
};
use ployzd::deploy_runtime::{
    DeployOperationPorts, DeployOperationRunError, DeployOperationStores, run_deploy_operation,
};
use ployzd::deploy_worker::DeployExecutionMachineScope;
use ployzd::intent::{NatsIntentReader, RunningIntentRuntime, start_intent_runtime};
use ployzd::machine_runtime::client::{NatsMachineContainerRuntime, NatsMachineFactsReader};
use ployzd::machine_runtime::protocol::{
    MachineEnsureEndpointNetworkRpcOk, MachineEnsureEndpointNetworkRpcResponse,
    MachineFactsGetRpcOk, MachineFactsGetRpcResponse,
};
use std::time::Duration;

#[tokio::test]
async fn accepted_deploy_runs_from_nats_facts_and_commits_active_state() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let _facts = start_facts_subscription(nats.machine_a.clone(), machine_id("machine_a")).await;
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
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
        DeployExecutionMachineScope::same_machines(vec![machine_id("machine_a")]),
        DeployOperationStores {
            core_state: core_state.clone(),
            controllers: controllers.clone(),
            namespace_lock_lost: Arc::new(AtomicBool::new(false)),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect("accepted deploy runs");
    assert_eq!(
        outcome.namespace_revision_id,
        target_namespace_revision_id(1)
    );
    assert_eq!(runtime.requests.len(), 1);
    let [(run_machine_id, run_request)] = runtime.requests.as_slice() else {
        panic!("expected one container run request");
    };
    assert_eq!(*run_machine_id, machine_id("machine_a"));
    assert_eq!(run_request.container.operation_id, operation_id("op_123"));
    assert_eq!(
        core_state
            .serving_target_entry(&namespace_id("default"), &service_id("svc_api"))
            .await
            .expect("active state reads")
            .expect("active state committed")
            .namespace_revision_entry_id,
        target_namespace_revision_entry_id()
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
    let _facts = start_facts_subscription(nats.machine_a.clone(), machine_id("machine_a")).await;
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(&nats.jetstream).await;
    let deploy_request = deploy_request(1);
    let accepted = controllers
        .submit_deploy(deploy_submit_command(deploy_request))
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::unhealthy("machine_a", "ctr_1");

    let error = run_deploy_operation(
        accepted,
        DeployExecutionMachineScope::same_machines(vec![machine_id("machine_a")]),
        DeployOperationStores {
            core_state: core_state.clone(),
            controllers: controllers.clone(),
            namespace_lock_lost: Arc::new(AtomicBool::new(false)),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("health failure fails deploy");

    assert!(matches!(error, DeployOperationRunError::Execute(_)));
    assert!(
        core_state
            .serving_target_entry(&namespace_id("default"), &service_id("svc_api"))
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
        }) if retained_artifacts == vec![retained_container("machine_a", "ctr_1")]
    ));
}

#[tokio::test]
async fn missing_machine_responder_marks_deploy_failed_without_committing_active_state() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let facts_reader = facts_reader(&nats.client, Duration::from_millis(200));
    let intent_reader = intent_reader(&nats.client, Duration::from_millis(200));
    let controllers = operation_controllers(&nats.jetstream).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(deploy_request(1)))
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = NatsMachineContainerRuntime::new(nats.client.clone())
        .with_request_timeout(Duration::from_millis(200));
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        accepted,
        DeployExecutionMachineScope::same_machines(vec![machine_id("machine_missing")]),
        DeployOperationStores {
            core_state: core_state.clone(),
            controllers: controllers.clone(),
            namespace_lock_lost: Arc::new(AtomicBool::new(false)),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("missing machine responder fails deploy");

    assert!(matches!(error, DeployOperationRunError::Execute(_)));
    assert!(
        core_state
            .serving_target_entry(&namespace_id("default"), &service_id("svc_api"))
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
                        DeployOperationFailure::NoUsableMachines { reasons },
                },
            ..
        }) if reasons == vec![ployz_core::ops::UnusableMachine {
            machine_id: machine_id("machine_missing"),
            reason: ployz_core::state::MachineUsabilityReason::FactsUnavailable,
        }]
    ));
}

#[tokio::test]
async fn machine_service_timeout_marks_deploy_failed_without_committing_active_state() {
    let nats = test_nats().await;
    let _facts =
        start_facts_subscription(nats.machine_slow.clone(), machine_id("machine_slow")).await;
    let _endpoint_network =
        start_endpoint_network_subscription(nats.machine_slow.clone(), machine_id("machine_slow"))
            .await;
    let _unresponsive_machine = start_unresponsive_container_run_subscription(
        nats.machine_slow.clone(),
        machine_id("machine_slow"),
    )
    .await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(&nats.jetstream).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(deploy_request(1)))
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = NatsMachineContainerRuntime::new(nats.client.clone())
        .with_request_timeout(Duration::from_millis(50));
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        accepted,
        DeployExecutionMachineScope::same_machines(vec![machine_id("machine_slow")]),
        DeployOperationStores {
            core_state: core_state.clone(),
            controllers: controllers.clone(),
            namespace_lock_lost: Arc::new(AtomicBool::new(false)),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("timed out machine service fails deploy");

    assert!(matches!(error, DeployOperationRunError::Execute(_)));
    assert!(
        core_state
            .serving_target_entry(&namespace_id("default"), &service_id("svc_api"))
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
                                machine_id: ref failed_machine_id,
                                ref message,
                                ref retained_artifacts,
                            },
                    },
                ..
            }) if failed_machine_id == &machine_id("machine_slow")
                && message.as_str() == "machine runtime request timed out"
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
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    nats.jetstream
        .delete_key_value(KV_CORE_BUCKET)
        .await
        .expect("delete core state bucket");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        accepted,
        DeployExecutionMachineScope::same_machines(vec![machine_id("machine_a")]),
        DeployOperationStores {
            core_state,
            controllers: controllers.clone(),
            namespace_lock_lost: Arc::new(AtomicBool::new(false)),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
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
                            namespace_revision_id: failed_namespace_revision_id,
                            ..
                        },
                },
            ..
        }) if failed_service_id == service_id("svc_api")
            && failed_namespace_revision_id == target_namespace_revision_id(1)
    ));
}

#[tokio::test]
async fn deploy_submit_rejects_busy_namespace_without_creating_second_operation() {
    let nats = test_nats().await;
    let controllers = operation_controllers(&nats.jetstream).await;
    controllers
        .submit_deploy(DeploySubmitCommand {
            operation_id: operation_id("op_first"),
            idempotency_key: idempotency_key("idem_first"),
            target: deploy_request(1),
        })
        .await
        .expect("first deploy operation accepted");

    let error = controllers
        .submit_deploy(DeploySubmitCommand {
            operation_id: operation_id("op_second"),
            idempotency_key: idempotency_key("idem_second"),
            target: deploy_request(1),
        })
        .await
        .expect_err("second deploy is rejected while namespace is locked");

    assert!(matches!(
        error,
        SubmitCommandError::NamespaceBusy {
            namespace_id: locked_namespace_id,
            owner,
        } if locked_namespace_id == namespace_id("default")
            && owner == operation_id("op_first")
    ));
    assert!(
        controllers
            .repository()
            .records()
            .get(&operation_id("op_second"))
            .await
            .expect("operation status reads")
            .is_none()
    );
}

#[tokio::test]
async fn deploy_submit_retry_with_same_idempotency_key_adopts_original_operation() {
    let nats = test_nats().await;
    let controllers = operation_controllers(&nats.jetstream).await;
    let first = controllers
        .submit_deploy(DeploySubmitCommand {
            operation_id: operation_id("op_first"),
            idempotency_key: idempotency_key("idem_deploy"),
            target: deploy_request(1),
        })
        .await
        .expect("first deploy operation accepted");

    let retry = controllers
        .submit_deploy(DeploySubmitCommand {
            operation_id: operation_id("op_retry_candidate"),
            idempotency_key: idempotency_key("idem_deploy"),
            target: deploy_request(1),
        })
        .await
        .expect("retry deploy operation accepted");

    assert_eq!(first.operation_id, operation_id("op_first"));
    assert_eq!(retry.operation_id, operation_id("op_first"));
    assert_eq!(retry.start_sequence, first.start_sequence);
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    _intent: RunningIntentRuntime,
    /// Controller principal: the deploy-runtime side.
    client: async_nats::Client,
    /// Machine principal for facts in normal deploy tests.
    machine_a: async_nats::Client,
    /// Machine principal for the stubbed slow machine service.
    machine_slow: async_nats::Client,
    jetstream: jetstream::Context,
}

async fn test_nats() -> TestNats {
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        machine_id("machine_a"),
        machine_id("machine_slow"),
    ])
    .await;
    nats.bootstrap_resources().await;
    let client = nats.controller.clone();
    let machine_a = nats.machine_client(&machine_id("machine_a")).await;
    let machine_slow = nats.machine_client(&machine_id("machine_slow")).await;
    let jetstream = nats.jetstream.clone();
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let lifecycle_dir = tempfile::tempdir().expect("lifecycle dir");
    let intent = start_intent_runtime(
        client.clone(),
        core_state,
        lifecycle_dir.path().join("machine-lifecycles.json"),
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    TestNats {
        _nats: nats,
        _intent: intent,
        client,
        machine_a,
        machine_slow,
        jetstream,
    }
}

async fn start_unresponsive_container_run_subscription(
    client: async_nats::Client,
    machine_id: ployz_core::ids::MachineId,
) -> tokio::task::JoinHandle<()> {
    let subject = machine_service(&machine_id, MachineServiceEndpoint::ContainerRun);
    let mut subscriber = client
        .subscribe(subject)
        .await
        .expect("subscribe unresponsive machine service");
    client
        .flush()
        .await
        .expect("flush unresponsive machine service subscription");
    tokio::spawn(async move {
        while subscriber.next().await.is_some() {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    })
}

async fn start_facts_subscription(
    client: async_nats::Client,
    machine_id: ployz_core::ids::MachineId,
) -> tokio::task::JoinHandle<()> {
    let subject = machine_service(&machine_id, MachineServiceEndpoint::FactsGet);
    let mut subscriber = client
        .subscribe(subject)
        .await
        .expect("subscribe facts service");
    client
        .flush()
        .await
        .expect("flush facts service subscription");
    tokio::spawn(async move {
        while let Some(message) = subscriber.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            let facts = empty_machine_facts(&machine_id);
            let response =
                serde_json::to_vec(&MachineFactsGetRpcResponse::Ok(MachineFactsGetRpcOk {
                    facts,
                }))
                .expect("facts response serializes");
            let _ = client.publish(reply, response.into()).await;
        }
    })
}

fn facts_reader(client: &async_nats::Client, timeout: Duration) -> NatsMachineFactsReader {
    NatsMachineFactsReader::new(client.clone()).with_request_timeout(timeout)
}

fn intent_reader(client: &async_nats::Client, timeout: Duration) -> NatsIntentReader {
    NatsIntentReader::new(client.clone()).with_request_timeout(timeout)
}

fn empty_machine_facts(machine_id: &ployz_core::ids::MachineId) -> MachineFactsSnapshot {
    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        MachineContainerObservationSnapshot::try_new(machine_id.clone(), [])
            .expect("empty machine snapshot is valid"),
        None,
        vec![MachineFactsRole::Machine],
        MachineLifecycle::Active,
        MachineSubstrateVersions::default(),
        Vec::new(),
        1,
    )
    .expect("empty machine facts are valid")
}

async fn start_endpoint_network_subscription(
    client: async_nats::Client,
    machine_id: ployz_core::ids::MachineId,
) -> tokio::task::JoinHandle<()> {
    let subject = machine_service(
        &machine_id,
        MachineServiceEndpoint::ContainerEnsureEndpointNetwork,
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
                let response = serde_json::to_vec(&MachineEnsureEndpointNetworkRpcResponse::Ok(
                    MachineEnsureEndpointNetworkRpcOk {
                        machine_id: machine_id.clone(),
                    },
                ))
                .expect("endpoint network response serializes");
                let _ = client.publish(reply, response.into()).await;
            }
        }
    })
}

async fn operation_controllers(jetstream: &jetstream::Context) -> OperationControllers {
    OperationControllers::new(
        AsyncNatsOperationEventLog::new(jetstream.clone()),
        AsyncNatsOperationStatusStore::from_jetstream(jetstream)
            .await
            .expect("open operation status store"),
        AsyncNatsCoreStateStore::from_jetstream(jetstream)
            .await
            .expect("open core state store"),
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
    )
}

fn deploy_submit_command(target: DeployRequest) -> DeploySubmitCommand {
    DeploySubmitCommand {
        operation_id: operation_id("op_123"),
        idempotency_key: idempotency_key("idem_deploy_123"),
        target,
    }
}

fn deploy_request(replicas: u16) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: image("registry.example/api:rev_2"),
            replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            routes: Vec::new(),
        }],
    }
}

#[allow(dead_code)]
#[path = "deploy_operation/fixtures.rs"]
mod fixtures;
#[path = "deploy_runtime_nats/managed_certificate.rs"]
mod managed_certificate;

use fixtures::*;
use futures_util::StreamExt;
use ployz_core::deploy::{
    DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec, ReplicaCount,
};
use ployz_core::install::MachineBootstrapUrl;
use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationFailure, DeployOperationState, OperationStatus,
};
use ployz_core::subjects::{INTENT_CHANGED, MachineServiceEndpoint, machine_service};
use ployz_test_support::ids::idempotency_key;
use ployzd::config::DEFAULT_MACHINE_BOOTSTRAP_URL;
use ployzd::intent::lease_intent::LeaseIntentStore;
use ployzd::intent::machine_roster::MachineRosterStore;
use ployzd::intent::namespace_intent::NamespaceIntentStore;
use ployzd::intent::service::{NatsIntentReader, RunningIntentService, start_intent_service};
use ployzd::lease::{LeaseClient, LeaseWorkerUrl};
use ployzd::operation_api::admission::{
    DeploySubmitCommand, MachineAddBootstrapConfig, OperationControllers, SubmitCommandError,
};
use ployzd::operations::deploy::driver::{
    DeployOperationPorts, DeployOperationRunError, DeployOperationStores, run_deploy_operation,
};
use ployzd::operations::deploy::{DeployMachineCandidates, ManagedCertificateWaitPolicy};
use ployzd::operations::log::{OperationRepository, SubmitOperationError};
use ployzd::roles::machine::client::{NatsMachineContainerRuntime, NatsMachineFactsReader};
use ployzd::roles::machine::protocol::{
    MachineEnsureEndpointNetworkRpcOk, MachineEnsureEndpointNetworkRpcResponse,
    MachineFactsGetRpcOk, MachineFactsGetRpcResponse,
};
use std::time::Duration;

#[tokio::test]
async fn accepted_deploy_runs_from_nats_facts_and_commits_active_state() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(nats.machine_a.clone(), machine_id("machine_a")).await;
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let deploy_request = deploy_request(1);
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request).await)
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut intent_changed = nats
        .machine_a
        .subscribe(INTENT_CHANGED)
        .await
        .expect("subscribe intent changes");
    let resolved_request = resolved_deploy_request(1);
    let resolved_entry_id = resolved_request
        .service_requests()
        .into_iter()
        .next()
        .expect("resolved fixture has one service")
        .namespace_revision_entry_id;

    let outcome = run_deploy_operation(
        accepted,
        DeployMachineCandidates::same_machines(vec![machine_id("machine_a")]),
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            lease_intent: nats.lease_intent.clone(),
            lease_client: LeaseClient::new(LeaseWorkerUrl::default_worker()),
            managed_certificate_wait: ManagedCertificateWaitPolicy::production(),
            controllers: controllers.clone(),
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
        resolved_request.namespace_revision_id()
    );
    tokio::time::timeout(Duration::from_secs(1), intent_changed.next())
        .await
        .expect("intent change is published")
        .expect("intent change message exists");
    assert_eq!(runtime.requests.len(), 1);
    let [(run_machine_id, run_request)] = runtime.requests.as_slice() else {
        panic!("expected one container run request");
    };
    assert_eq!(*run_machine_id, machine_id("machine_a"));
    assert_eq!(run_request.container.operation_id, operation_id("op_123"));
    assert_eq!(
        nats.namespace_intent
            .load()
            .await
            .expect("namespace intent reads")
            .serving_target_entries
            .into_iter()
            .find(|entry| {
                entry.namespace_id == namespace_id("default")
                    && entry.service_id == service_id("svc_api")
            })
            .expect("serving target committed")
            .namespace_revision_entry_id,
        resolved_entry_id
    );
    assert!(matches!(
        controllers
            .repository()
            .get(&operation_id("op_123"))
            .await
            .expect("status reads"),
        Some(OperationStatus::Deploy {
            state: DeployOperationState::Completed {
                outcome: DeployCompletionOutcome::Completed,
            },
            ..
        })
    ));
}

#[tokio::test]
async fn idempotent_completed_deploy_retry_releases_namespace_lock() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(nats.machine_a.clone(), machine_id("machine_a")).await;
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request(1)).await)
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();

    run_deploy_operation(
        accepted,
        DeployMachineCandidates::same_machines(vec![machine_id("machine_a")]),
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            lease_intent: nats.lease_intent.clone(),
            lease_client: LeaseClient::new(LeaseWorkerUrl::default_worker()),
            managed_certificate_wait: ManagedCertificateWaitPolicy::production(),
            controllers: controllers.clone(),
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
    .expect("deploy completes");
    controllers
        .release_namespace(&namespace_id("default"), &operation_id("op_123"))
        .await;

    let retry = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request(1)).await)
        .await
        .expect("completed deploy retry adopts existing operation");
    assert!(
        !retry.submission.should_start_execution,
        "completed retry must not spawn execution"
    );

    let next = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_next"),
            idempotency_key: idempotency_key("idem_deploy_next"),
            reservation_id: reserve_deploy(&controllers).await,
            target: deploy_request(1),
        })
        .await
        .expect("next deploy can acquire the namespace");
    assert!(next.submission.should_start_execution);
}

#[tokio::test]
async fn health_failure_records_failed_operation_without_committing_active_state() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(nats.machine_a.clone(), machine_id("machine_a")).await;
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let deploy_request = deploy_request(1);
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request).await)
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::unhealthy("machine_a", "ctr_1");

    let error = run_deploy_operation(
        accepted,
        DeployMachineCandidates::same_machines(vec![machine_id("machine_a")]),
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            lease_intent: nats.lease_intent.clone(),
            lease_client: LeaseClient::new(LeaseWorkerUrl::default_worker()),
            managed_certificate_wait: ManagedCertificateWaitPolicy::production(),
            controllers: controllers.clone(),
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
        nats.namespace_intent
            .load()
            .await
            .expect("namespace intent reads")
            .serving_target_entries
            .is_empty()
    );
    assert!(matches!(
        controllers
            .repository()
            .get(&operation_id("op_123"))
            .await
            .expect("status reads"),
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
async fn auto_dns_without_lease_fails_before_runtime_work_with_guidance() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(nats.machine_a.clone(), machine_id("machine_a")).await;
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let mut request = deploy_request(1);
    let [service] = request.services.as_mut_slice() else {
        panic!("fixture has one service");
    };
    service.routes.push(DeployRoute {
        target: DeployRouteTarget::AutoHostname {
            port: route_port(443),
        },
        endpoint_port: route_port(8080),
    });
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, request).await)
        .await
        .expect("deploy operation accepted");
    let mut dataplane = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_should_not_start"]);
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        accepted,
        DeployMachineCandidates::same_machines(vec![machine_id("machine_a")]),
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            lease_intent: nats.lease_intent.clone(),
            lease_client: LeaseClient::new(LeaseWorkerUrl::default_worker()),
            managed_certificate_wait: ManagedCertificateWaitPolicy::production(),
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            dataplane: &mut dataplane,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("lease-less auto DNS deploy fails");
    let status = controllers
        .repository()
        .get(&operation_id("op_123"))
        .await
        .expect("status reads");

    assert!(matches!(
        error,
        DeployOperationRunError::AutoDnsWithoutLease {
            failure_record_error: None
        }
    ));
    assert!(runtime.requests.is_empty());
    assert!(matches!(
        status,
        Some(OperationStatus::Deploy {
            state: DeployOperationState::Failed {
                failure: DeployOperationFailure::AutoDnsWithoutLease { message, .. },
            },
            ..
        }) if message.as_str().contains("re-run init with --public-url auto")
    ));
}

#[tokio::test]
async fn missing_machine_responder_marks_deploy_failed_without_committing_active_state() {
    let nats = test_nats().await;
    let facts_reader = facts_reader(&nats.client, Duration::from_millis(200));
    let intent_reader = intent_reader(&nats.client, Duration::from_millis(200));
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, resolved_deploy_request(1)).await)
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = NatsMachineContainerRuntime::new(nats.client.clone())
        .with_request_timeout(Duration::from_millis(200));
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        accepted,
        DeployMachineCandidates::same_machines(vec![machine_id("machine_missing")]),
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            lease_intent: nats.lease_intent.clone(),
            lease_client: LeaseClient::new(LeaseWorkerUrl::default_worker()),
            managed_certificate_wait: ManagedCertificateWaitPolicy::production(),
            controllers: controllers.clone(),
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
        nats.namespace_intent
            .load()
            .await
            .expect("namespace intent reads")
            .serving_target_entries
            .is_empty()
    );
    assert!(matches!(
        controllers
            .repository()
            .get(&operation_id("op_123"))
            .await
            .expect("status reads"),
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
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, resolved_deploy_request(1)).await)
        .await
        .expect("deploy operation accepted");
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = NatsMachineContainerRuntime::new(nats.client.clone())
        .with_request_timeout(Duration::from_millis(50));
    let mut health = RecordingHealth::healthy();

    let error = run_deploy_operation(
        accepted,
        DeployMachineCandidates::same_machines(vec![machine_id("machine_slow")]),
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            lease_intent: nats.lease_intent.clone(),
            lease_client: LeaseClient::new(LeaseWorkerUrl::default_worker()),
            managed_certificate_wait: ManagedCertificateWaitPolicy::production(),
            controllers: controllers.clone(),
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
        nats.namespace_intent
            .load()
            .await
            .expect("namespace intent reads")
            .serving_target_entries
            .is_empty()
    );
    let status = controllers
        .repository()
        .get(&operation_id("op_123"))
        .await
        .expect("status reads");
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
async fn deploy_submit_rejects_busy_namespace_without_creating_second_operation() {
    let nats = test_nats().await;
    let controllers = operation_controllers(nats.client.clone()).await;
    controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_first"),
            idempotency_key: idempotency_key("idem_first"),
            reservation_id: reserve_deploy(&controllers).await,
            target: deploy_request(1),
        })
        .await
        .expect("first deploy operation accepted");

    let error = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_second"),
            idempotency_key: idempotency_key("idem_second"),
            reservation_id: reserve_deploy(&controllers).await,
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
            .get(&operation_id("op_second"))
            .await
            .expect("status reads")
            .is_none()
    );
}

#[tokio::test]
async fn older_reservation_is_stale_while_newer_deploy_holds_namespace_lock() {
    let nats = test_nats().await;
    let controllers = operation_controllers(nats.client.clone()).await;
    let older = reserve_deploy(&controllers).await;
    let newer = reserve_deploy(&controllers).await;
    controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_newer"),
            idempotency_key: idempotency_key("idem_newer"),
            reservation_id: newer,
            target: deploy_request(1),
        })
        .await
        .expect("newer deploy operation accepted");

    let error = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_older"),
            idempotency_key: idempotency_key("idem_older"),
            reservation_id: older,
            target: deploy_request(1),
        })
        .await
        .expect_err("older reservation is stale after newer admission");

    assert!(matches!(
        error,
        SubmitCommandError::Submit(SubmitOperationError::StaleDeployReservation {
            namespace_id: stale_namespace_id,
            reservation_id,
            last_committed_reservation_id,
        }) if stale_namespace_id == namespace_id("default")
            && reservation_id == older
            && last_committed_reservation_id == newer
    ));
}

#[tokio::test]
async fn deploy_submit_retry_with_same_idempotency_key_adopts_original_operation() {
    let nats = test_nats().await;
    let controllers = operation_controllers(nats.client.clone()).await;
    let first = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_first"),
            idempotency_key: idempotency_key("idem_deploy"),
            reservation_id: reserve_deploy(&controllers).await,
            target: deploy_request(1),
        })
        .await
        .expect("first deploy operation accepted");

    let retry = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_retry_candidate"),
            idempotency_key: idempotency_key("idem_deploy"),
            reservation_id: reserve_deploy(&controllers).await,
            target: deploy_request(1),
        })
        .await
        .expect("retry deploy operation accepted");

    assert_eq!(first.submission.operation_id, operation_id("op_first"));
    assert_eq!(retry.submission.operation_id, operation_id("op_first"));
    assert_eq!(
        retry.submission.start_sequence,
        first.submission.start_sequence
    );
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    _intent: RunningIntentService,
    _intent_dir: tempfile::TempDir,
    namespace_intent: NamespaceIntentStore,
    lease_intent: LeaseIntentStore,
    /// Controller principal: the deploy-runtime side.
    client: async_nats::Client,
    /// Machine principal for facts in normal deploy tests.
    machine_a: async_nats::Client,
    /// Machine principal for the stubbed slow machine service.
    machine_slow: async_nats::Client,
}

async fn test_nats() -> TestNats {
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        machine_id("machine_a"),
        machine_id("machine_slow"),
    ])
    .await;
    let client = nats.controller.clone();
    let machine_a = nats.machine_client(&machine_id("machine_a")).await;
    let machine_slow = nats.machine_client(&machine_id("machine_slow")).await;
    let lifecycle_dir = tempfile::tempdir().expect("lifecycle dir");
    let namespace_intent = NamespaceIntentStore::new(
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    let machine_roster = MachineRosterStore::new(
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    let intent_core_store = ployzd::core_store::CoreStore::open_in_memory()
        .await
        .expect("open intent core store");
    let lease_intent = LeaseIntentStore::new(intent_core_store.clone());
    let intent = start_intent_service(
        client.clone(),
        machine_id("machine_a"),
        machine_roster,
        namespace_intent.clone(),
        intent_core_store,
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    TestNats {
        _nats: nats,
        _intent: intent,
        _intent_dir: lifecycle_dir,
        namespace_intent,
        lease_intent,
        client,
        machine_a,
        machine_slow,
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
        test_disk_space(),
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("empty machine facts are valid")
}

fn test_disk_space() -> ployz_core::machine_runtime::MachineDiskSpace {
    ployz_test_support::fixtures::test_disk_space()
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

async fn operation_controllers(client: async_nats::Client) -> OperationControllers {
    let evidence_dir = tempfile::tempdir()
        .expect("operation evidence temp dir")
        .keep();
    let core_store = ployzd::core_store::CoreStore::open(evidence_dir.join("ployz-core.db"))
        .await
        .expect("open core store");
    OperationControllers::new(
        OperationRepository::open(core_store, client),
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
    )
}

async fn deploy_submit_command(
    controllers: &OperationControllers,
    target: DeployRequest,
) -> DeploySubmitCommand {
    DeploySubmitCommand {
        registry_credentials: std::collections::BTreeMap::new(),
        operation_id: operation_id("op_123"),
        idempotency_key: idempotency_key("idem_deploy_123"),
        reservation_id: reserve_deploy(controllers).await,
        target,
    }
}

async fn reserve_deploy(
    controllers: &OperationControllers,
) -> ployz_core::deploy::DeployReservationId {
    controllers
        .reserve_deploy(&namespace_id("default"))
        .await
        .expect("deploy reservation is issued")
        .reservation_id
}

fn deploy_request(replicas: u16) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: image("registry.example/api:rev_2"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn resolved_deploy_request(replicas: u16) -> DeployRequest {
    let mut request = deploy_request(replicas);
    let [service] = request.services.as_mut_slice() else {
        panic!("fixture has one service");
    };
    let digest = ployz_core::image::OciDigest::sha256(service.image.as_str().as_bytes());
    service.image = service
        .image
        .with_digest(&digest)
        .expect("fixture image accepts a digest");
    request
}

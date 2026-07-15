use super::fixtures::*;
use crate::certificate::{CertificateManager, CertificateManagerConfig};
use crate::config::DEFAULT_MACHINE_BOOTSTRAP_URL;
use crate::control::intent::ingress_intent::{IngressProjectionStore, PloyzDnsTargetStore};
use crate::control::intent::machine_roster::MachineRosterStore;
use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::service::{
    NatsIntentReader, RunningIntentService, start_intent_service,
};
use crate::control::operation_evidence::{OperationRepository, SubmitOperationError};
use crate::control::operations::deploy::driver::{
    DeployOperationDriver, DeployOperationPorts, DeployOperationRunError, DeployOperationStores,
    run_deploy_operation,
};
use crate::control::role_client::machine::{NatsMachineContainerRuntime, NatsMachineFactsReader};
use crate::control::sequencer::{
    DeploySubmitCommand, MachineAddBootstrapConfig, OperationControllers, SubmitCommandError,
};
use crate::control::store::CoreStore;
use crate::roles::machine::protocol::{MachineFactsGetRpcOk, MachineFactsGetRpcResponse};
use crate::tasks::TaskRegistry;
use futures_util::StreamExt;
use ployz_core::deploy::{DeployRequest, DeployServiceSpec, ReplicaCount};
use ployz_core::install::MachineBootstrapUrl;
use ployz_core::intent::ActiveMachineState;
use ployz_core::machine::MachineLifecycle;
use ployz_core::machine::MachineName;
use ployz_core::machine::runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
use ployz_core::operation::{
    DeployCompletionOutcome, DeployOperationFailure, DeployOperationState,
    OperationInterruptionCause, OperationStatus,
};
use ployz_nats::subjects::{INTENT_CHANGED, MachineServiceEndpoint, machine_service};
use ployz_test_support::ids::idempotency_key;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn accepted_deploy_runs_from_nats_facts_and_commits_active_state() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(
        nats.machine_a.clone(),
        nats.client.clone(),
        machine_id("machine_a"),
    )
    .await;
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let deploy_request = deploy_request(1);
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request).await)
        .await
        .expect("deploy operation accepted");
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();
    let mut intent_changed = nats
        .machine_a
        .subscribe(INTENT_CHANGED)
        .await
        .expect("subscribe intent changes");
    let resolved_request = resolved_deploy_request(1);
    let resolved_entry_id =
        ployz_core::deploy::VolumeDeclaredDeployRequest::try_new(resolved_request.clone())
            .expect("request normalizes")
            .services()
            .first()
            .expect("resolved fixture has one service")
            .namespace_revision_entry_id(&namespace_id("default"));

    let outcome = run_deploy_operation(
        accepted,
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            ployz_dns_target: nats.ployz_dns_target.clone(),
            ingress_projection: nats.ingress_projection.clone(),
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
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
    let status = controllers
        .repository()
        .get(&operation_id("op_123"))
        .await
        .expect("status reads");
    assert!(
        matches!(
            status,
            Some(OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
                ..
            })
        ),
        "unexpected operation status: {status:?}"
    );
}

#[tokio::test]
async fn interrupted_deploy_retry_gathers_and_reuses_retained_runtime_work() {
    let nats = test_nats().await;
    let (machine_a_facts_task, machine_a_facts) = start_dynamic_facts_subscription(
        nats.machine_a.clone(),
        nats.client.clone(),
        machine_id("machine_a"),
    )
    .await;
    let (machine_slow_facts_task, machine_slow_facts) = start_dynamic_facts_subscription(
        nats.machine_slow.clone(),
        nats.client.clone(),
        machine_id("machine_slow"),
    )
    .await;
    let _facts_tasks = (machine_a_facts_task, machine_slow_facts_task);
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request(2)).await)
        .await
        .expect("first deploy operation accepted");
    let first_run_hang = Arc::new(tokio::sync::Notify::new());
    let mut first_runtime = RecordingRuntime::hanging_after_first_container(
        "ctr_retained",
        Arc::clone(&first_run_hang),
    );
    let mut first_health = RecordingHealth::healthy();
    let mut first_certificates = RecordingCertificates::successful();

    {
        let first_run = run_deploy_operation(
            accepted,
            DeployOperationStores {
                intent_change_client: nats.client.clone(),
                namespace_intent: nats.namespace_intent.clone(),
                ployz_dns_target: nats.ployz_dns_target.clone(),
                ingress_projection: nats.ingress_projection.clone(),
                controllers: controllers.clone(),
            },
            DeployOperationPorts {
                facts_reader: &facts_reader,
                intent_reader: &intent_reader,
                machine_runtime: &mut first_runtime,
                health_checker: &mut first_health,
                certificate_provisioner: &mut first_certificates,
            },
            Duration::from_secs(5),
        );
        tokio::pin!(first_run);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                () = first_run_hang.notified() => {}
                outcome = &mut first_run => panic!("first deploy settled before interruption: {outcome:?}"),
            }
        })
        .await
        .expect("first deploy reaches controlled interruption seam");
    }

    let [(retained_machine_id, retained_request)] = first_runtime.requests.as_slice() else {
        panic!("first deploy created exactly one retained container");
    };
    assert_eq!(
        retained_request.container.operation_id,
        operation_id("op_123")
    );
    let retained_observation = ployz_core::machine::runtime::ManagedContainerObservation {
        machine_id: retained_machine_id.clone(),
        container_id: container_id("ctr_retained"),
        identity: retained_request.container.clone(),
        state: ployz_core::machine::runtime::ContainerRuntimeState::running_unroutable(),
        health_status: None,
        resolved_image_identity: None,
        created_at_unix_seconds: None,
    };
    let retained_facts = machine_facts(retained_machine_id.clone(), [retained_observation]);
    let target_facts = if retained_machine_id == &machine_id("machine_a") {
        &machine_a_facts
    } else {
        &machine_slow_facts
    };
    *target_facts
        .lock()
        .expect("dynamic facts lock is not poisoned") = retained_facts;

    let repository = controllers.repository().clone();
    let summary = repository
        .record_interrupted_operations(OperationInterruptionCause::PriorCoreProcessLoss)
        .await
        .expect("process loss records terminal interruption evidence");
    assert_eq!(summary.recorded, 1);
    assert!(matches!(
        repository
            .get(&operation_id("op_123"))
            .await
            .expect("first status reads"),
        Some(OperationStatus::Deploy {
            state: DeployOperationState::Interrupted { evidence },
            ..
        }) if evidence.cause() == OperationInterruptionCause::PriorCoreProcessLoss
    ));

    drop(controllers);
    let restarted_controllers = OperationControllers::new(
        repository,
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
    );
    let retry = restarted_controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_retry"),
            idempotency_key: idempotency_key("idem_deploy_retry"),
            reservation_id: reserve_deploy(&restarted_controllers).await,
            target: normalized_deploy_request(2),
        })
        .await
        .expect("retry deploy accepted by restarted sequencer");
    let mut retry_runtime = RecordingRuntime::with_containers(["ctr_missing"]);
    let mut retry_health = RecordingHealth::healthy();
    let mut retry_certificates = RecordingCertificates::successful();

    let outcome = run_deploy_operation(
        retry,
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            ployz_dns_target: nats.ployz_dns_target.clone(),
            ingress_projection: nats.ingress_projection.clone(),
            controllers: restarted_controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            machine_runtime: &mut retry_runtime,
            health_checker: &mut retry_health,
            certificate_provisioner: &mut retry_certificates,
        },
        Duration::from_secs(5),
    )
    .await
    .expect("retry converges from observed retained work");

    assert_eq!(
        outcome
            .containers
            .iter()
            .map(|container| container.container_id.clone())
            .collect::<Vec<_>>(),
        vec![container_id("ctr_retained"), container_id("ctr_missing")]
    );
    let [(retry_machine_id, retry_request)] = retry_runtime.requests.as_slice() else {
        panic!("retry creates only the missing replica");
    };
    assert_eq!(
        retry_request.container.operation_id,
        operation_id("op_retry")
    );
    assert_eq!(
        retry_health.checked,
        vec![vec![
            DeployContainerForAssert::new(retained_machine_id.as_str(), "ctr_retained"),
            DeployContainerForAssert::new(retry_machine_id.as_str(), "ctr_missing"),
        ]]
    );
    let serving = nats
        .namespace_intent
        .load()
        .await
        .expect("namespace intent reads")
        .serving_target_entries
        .into_iter()
        .find(|entry| entry.service_id == service_id("svc_api"))
        .expect("retry commits serving target");
    assert_eq!(
        serving.desired_replicas,
        ReplicaCount::try_new(2).expect("replica count")
    );
    assert!(matches!(
        restarted_controllers
            .repository()
            .get(&operation_id("op_retry"))
            .await
            .expect("retry status reads"),
        Some(OperationStatus::Deploy {
            state: DeployOperationState::Completed { .. },
            ..
        })
    ));
}

#[tokio::test]
async fn interrupted_scale_up_replica_is_regated_under_a_promoted_entry() {
    let nats = test_nats().await;
    let (machine_a_facts_task, machine_a_facts) = start_dynamic_facts_subscription(
        nats.machine_a.clone(),
        nats.client.clone(),
        machine_id("machine_a"),
    )
    .await;
    let (machine_slow_facts_task, machine_slow_facts) = start_dynamic_facts_subscription(
        nats.machine_slow.clone(),
        nats.client.clone(),
        machine_id("machine_slow"),
    )
    .await;
    let _facts_tasks = (machine_a_facts_task, machine_slow_facts_task);
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;

    let initial = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_initial"),
            idempotency_key: idempotency_key("idem_initial"),
            reservation_id: reserve_deploy(&controllers).await,
            target: normalized_deploy_request(1),
        })
        .await
        .expect("initial deploy accepted");
    let mut initial_runtime = RecordingRuntime::with_containers(["ctr_promoted"]);
    let mut initial_health = RecordingHealth::healthy();
    run_deploy_operation(
        initial,
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            ployz_dns_target: nats.ployz_dns_target.clone(),
            ingress_projection: nats.ingress_projection.clone(),
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            machine_runtime: &mut initial_runtime,
            health_checker: &mut initial_health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
        },
        Duration::from_secs(5),
    )
    .await
    .expect("initial deploy promotes one replica");
    controllers
        .release_namespace(&namespace_id("default"), &operation_id("op_initial"))
        .await;
    let [(promoted_machine_id, promoted_request)] = initial_runtime.requests.as_slice() else {
        panic!("initial deploy creates one container");
    };
    let promoted_observation = ployz_core::machine::runtime::ManagedContainerObservation {
        machine_id: promoted_machine_id.clone(),
        container_id: container_id("ctr_promoted"),
        identity: promoted_request.container.clone(),
        state: ployz_core::machine::runtime::ContainerRuntimeState::running_unroutable(),
        health_status: None,
        resolved_image_identity: None,
        created_at_unix_seconds: None,
    };
    let promoted_facts = if promoted_machine_id == &machine_id("machine_a") {
        &machine_a_facts
    } else {
        &machine_slow_facts
    };
    *promoted_facts
        .lock()
        .expect("dynamic facts lock is not poisoned") =
        machine_facts(promoted_machine_id.clone(), [promoted_observation.clone()]);

    let scale_up = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_scale_up"),
            idempotency_key: idempotency_key("idem_scale_up"),
            reservation_id: reserve_deploy(&controllers).await,
            target: normalized_deploy_request(2),
        })
        .await
        .expect("scale-up deploy accepted");
    let health_reached = Arc::new(tokio::sync::Notify::new());
    let mut scale_runtime = RecordingRuntime::with_containers(["ctr_interrupted"]);
    let mut scale_health = NotifyingHangingHealth::new(Arc::clone(&health_reached));
    let mut scale_certificates = RecordingCertificates::successful();
    {
        let scale_run = run_deploy_operation(
            scale_up,
            DeployOperationStores {
                intent_change_client: nats.client.clone(),
                namespace_intent: nats.namespace_intent.clone(),
                ployz_dns_target: nats.ployz_dns_target.clone(),
                ingress_projection: nats.ingress_projection.clone(),
                controllers: controllers.clone(),
            },
            DeployOperationPorts {
                facts_reader: &facts_reader,
                intent_reader: &intent_reader,
                machine_runtime: &mut scale_runtime,
                health_checker: &mut scale_health,
                certificate_provisioner: &mut scale_certificates,
            },
            Duration::from_secs(5),
        );
        tokio::pin!(scale_run);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                () = health_reached.notified() => {}
                outcome = &mut scale_run => panic!("scale-up settled before interruption: {outcome:?}"),
            }
        })
        .await
        .expect("scale-up reaches its creation health gate");
    }
    let [(interrupted_machine_id, interrupted_request)] = scale_runtime.requests.as_slice() else {
        panic!("scale-up creates one additional container");
    };
    let interrupted_observation = ployz_core::machine::runtime::ManagedContainerObservation {
        machine_id: interrupted_machine_id.clone(),
        container_id: container_id("ctr_interrupted"),
        identity: interrupted_request.container.clone(),
        state: ployz_core::machine::runtime::ContainerRuntimeState::running_unroutable(),
        health_status: None,
        resolved_image_identity: None,
        created_at_unix_seconds: None,
    };
    let interrupted_facts = if interrupted_machine_id == &machine_id("machine_a") {
        &machine_a_facts
    } else {
        &machine_slow_facts
    };
    *interrupted_facts
        .lock()
        .expect("dynamic facts lock is not poisoned") =
        if interrupted_machine_id == promoted_machine_id {
            machine_facts(
                interrupted_machine_id.clone(),
                [promoted_observation, interrupted_observation],
            )
        } else {
            machine_facts(interrupted_machine_id.clone(), [interrupted_observation])
        };

    let repository = controllers.repository().clone();
    let summary = repository
        .record_interrupted_operations(OperationInterruptionCause::PriorCoreProcessLoss)
        .await
        .expect("scale-up interruption records");
    assert_eq!(summary.recorded, 1);
    drop(controllers);
    let restarted_controllers = OperationControllers::new(
        repository,
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
    );
    let retry = restarted_controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_scale_retry"),
            idempotency_key: idempotency_key("idem_scale_retry"),
            reservation_id: reserve_deploy(&restarted_controllers).await,
            target: normalized_deploy_request(2),
        })
        .await
        .expect("scale-up retry accepted");
    let mut retry_runtime = RecordingRuntime::with_containers([]);
    let mut retry_health = RecordingHealth::healthy();
    let outcome = run_deploy_operation(
        retry,
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            ployz_dns_target: nats.ployz_dns_target.clone(),
            ingress_projection: nats.ingress_projection.clone(),
            controllers: restarted_controllers,
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            machine_runtime: &mut retry_runtime,
            health_checker: &mut retry_health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
        },
        Duration::from_secs(5),
    )
    .await
    .expect("scale-up retry promotes both observed replicas");

    assert!(retry_runtime.requests.is_empty());
    assert_eq!(
        retry_health.checked,
        vec![vec![DeployContainerForAssert::new(
            interrupted_machine_id.as_str(),
            "ctr_interrupted",
        )]]
    );
    assert_eq!(outcome.containers.len(), 2);
    let serving = nats
        .namespace_intent
        .load()
        .await
        .expect("namespace intent reads")
        .serving_target_entries
        .into_iter()
        .find(|entry| entry.service_id == service_id("svc_api"))
        .expect("retry commits serving target");
    assert_eq!(
        serving.desired_replicas,
        ReplicaCount::try_new(2).expect("replica count")
    );
}

#[tokio::test]
async fn idempotent_completed_deploy_retry_releases_namespace_lock() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(
        nats.machine_a.clone(),
        nats.client.clone(),
        machine_id("machine_a"),
    )
    .await;
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request(1)).await)
        .await
        .expect("deploy operation accepted");
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();

    run_deploy_operation(
        accepted,
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            ployz_dns_target: nats.ployz_dns_target.clone(),
            ingress_projection: nats.ingress_projection.clone(),
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
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
            target: normalized_deploy_request(1),
        })
        .await
        .expect("next deploy can acquire the namespace");
    assert!(next.submission.should_start_execution);
}

#[tokio::test]
async fn health_failure_records_failed_operation_without_committing_active_state() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(
        nats.machine_a.clone(),
        nats.client.clone(),
        machine_id("machine_a"),
    )
    .await;
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let deploy_request = deploy_request(1);
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request).await)
        .await
        .expect("deploy operation accepted");
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::unhealthy("machine_a", "ctr_1");
    let mut certificates = RecordingCertificates::successful();

    let error = run_deploy_operation(
        accepted,
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            ployz_dns_target: nats.ployz_dns_target.clone(),
            ingress_projection: nats.ingress_projection.clone(),
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
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
    let health_status = controllers
        .repository()
        .get(&operation_id("op_123"))
        .await
        .expect("status reads");
    assert!(matches!(
        &health_status,
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
        }) if *retained_artifacts == vec![retained_container("machine_a", "ctr_1")]
    ));
}

#[tokio::test]
async fn duplicate_driver_execution_does_not_release_the_original_namespace_lock() {
    let nats = test_nats().await;
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request(1)).await)
        .await
        .expect("deploy operation accepted");
    controllers
        .repository()
        .record_deploy_transition(
            &operation_id("op_123"),
            ployz_core::operation::DeployTransition::Planning,
        )
        .await
        .expect("deploy already started");
    let tasks = TaskRegistry::default();
    let driver = DeployOperationDriver::new(
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            ployz_dns_target: nats.ployz_dns_target.clone(),
            ingress_projection: nats.ingress_projection.clone(),
            controllers: controllers.clone(),
        },
        CertificateManager::new(
            CoreStore::open_in_memory()
                .await
                .expect("open certificate core store"),
            nats.client.clone(),
            CertificateManagerConfig::for_core_db(Path::new("ployz-core.db")),
        ),
        Duration::from_secs(5),
        tasks.spawner(),
    );

    let result = driver.run(accepted).await;
    let second = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_second"),
            idempotency_key: idempotency_key("idem_second"),
            reservation_id: reserve_deploy(&controllers).await,
            target: normalized_deploy_request(1),
        })
        .await
        .expect_err("original deploy still owns the namespace lock");

    assert!(matches!(
        result,
        Err(DeployOperationRunError::AlreadyStarted)
    ));
    assert!(matches!(
        second,
        SubmitCommandError::NamespaceBusy { owner, .. }
            if owner == operation_id("op_123")
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
    let mut runtime = NatsMachineContainerRuntime::new(nats.client.clone())
        .with_request_timeout(Duration::from_millis(200));
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();

    let error = run_deploy_operation(
        accepted,
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            ployz_dns_target: nats.ployz_dns_target.clone(),
            ingress_projection: nats.ingress_projection.clone(),
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
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
    let missing_status = controllers
        .repository()
        .get(&operation_id("op_123"))
        .await
        .expect("status reads");
    assert!(
        matches!(
            &missing_status,
            Some(OperationStatus::Deploy {
                state:
                    DeployOperationState::Failed {
                        failure:
                            DeployOperationFailure::NoUsableMachines { reasons },
                    },
                ..
            }) if *reasons == vec![
                ployz_core::operation::UnusableMachine {
                    machine_id: machine_id("machine_a"),
                    reason: ployz_core::machine::MachineUsabilityReason::FactsUnavailable,
                },
                ployz_core::operation::UnusableMachine {
                    machine_id: machine_id("machine_slow"),
                    reason: ployz_core::machine::MachineUsabilityReason::FactsUnavailable,
                },
            ]
        ),
        "unexpected missing-machine status: {missing_status:?}"
    );
}

#[tokio::test]
async fn machine_service_timeout_marks_deploy_failed_without_committing_active_state() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(
        nats.machine_slow.clone(),
        nats.client.clone(),
        machine_id("machine_slow"),
    )
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
    let mut runtime = NatsMachineContainerRuntime::new(nats.client.clone())
        .with_request_timeout(Duration::from_millis(50));
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();

    let error = run_deploy_operation(
        accepted,
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            ployz_dns_target: nats.ployz_dns_target.clone(),
            ingress_projection: nats.ingress_projection.clone(),
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
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
            target: normalized_deploy_request(1),
        })
        .await
        .expect("first deploy operation accepted");

    let error = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_second"),
            idempotency_key: idempotency_key("idem_second"),
            reservation_id: reserve_deploy(&controllers).await,
            target: normalized_deploy_request(1),
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
            target: normalized_deploy_request(1),
        })
        .await
        .expect("newer deploy operation accepted");

    let error = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_older"),
            idempotency_key: idempotency_key("idem_older"),
            reservation_id: older,
            target: normalized_deploy_request(1),
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
            target: normalized_deploy_request(1),
        })
        .await
        .expect("first deploy operation accepted");

    let retry = controllers
        .submit_deploy(DeploySubmitCommand {
            registry_credentials: std::collections::BTreeMap::new(),
            operation_id: operation_id("op_retry_candidate"),
            idempotency_key: idempotency_key("idem_deploy"),
            reservation_id: reserve_deploy(&controllers).await,
            target: normalized_deploy_request(1),
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
    ployz_dns_target: PloyzDnsTargetStore,
    ingress_projection: IngressProjectionStore,
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
        crate::control::store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    let intent_core_store = crate::control::store::CoreStore::open_in_memory()
        .await
        .expect("open intent core store");
    crate::tests::support::intent::initialize_disabled_ingress(&intent_core_store).await;
    let machine_roster = MachineRosterStore::new(intent_core_store.clone());
    machine_roster
        .replace_active_machine(&active_machine("machine_a", "10.198.1.0/24"))
        .await
        .expect("machine a enters roster");
    machine_roster
        .replace_active_machine(&active_machine("machine_slow", "10.198.2.0/24"))
        .await
        .expect("slow machine enters roster");
    let ployz_dns_target = PloyzDnsTargetStore::new(intent_core_store.clone());
    let ingress_projection = IngressProjectionStore::new(intent_core_store.clone());
    let intent = start_intent_service(
        client.clone(),
        machine_id("machine_a"),
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
        ployz_dns_target,
        ingress_projection,
        client,
        machine_a,
        machine_slow,
    }
}

fn active_machine(machine: &str, subnet: &str) -> ActiveMachineState {
    ActiveMachineState {
        machine_id: machine_id(machine),
        name: MachineName::try_new(machine).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
        roles: ployz_core::machine::roles::InstallRolePolicy::install_all(),
        lifecycle: MachineLifecycle::Active,
        control_endpoints: Vec::new(),
        mesh_endpoints: vec!["192.0.2.1:51820".parse().expect("mesh endpoint")],
        endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new(subnet)
            .expect("endpoint subnet"),
        wireguard_public_key: ployz_core::network::WireGuardPublicKey::try_new(format!(
            "public-{machine}"
        ))
        .expect("wireguard key"),
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
    intent_client: async_nats::Client,
    machine_id: ployz_core::ids::MachineId,
) -> tokio::task::JoinHandle<()> {
    start_dynamic_facts_subscription(client, intent_client, machine_id)
        .await
        .0
}

async fn start_dynamic_facts_subscription(
    client: async_nats::Client,
    intent_client: async_nats::Client,
    machine_id: ployz_core::ids::MachineId,
) -> (
    tokio::task::JoinHandle<()>,
    Arc<Mutex<MachineFactsSnapshot>>,
) {
    crate::tests::support::dataplane::start_applied_status_responder(
        client.clone(),
        intent_client,
        machine_id.clone(),
    )
    .await;
    let subject = machine_service(&machine_id, MachineServiceEndpoint::FactsGet);
    let mut subscriber = client
        .subscribe(subject)
        .await
        .expect("subscribe facts service");
    client
        .flush()
        .await
        .expect("flush facts service subscription");
    let facts = Arc::new(Mutex::new(empty_machine_facts(&machine_id)));
    let served_facts = Arc::clone(&facts);
    let task = tokio::spawn(async move {
        while let Some(message) = subscriber.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            let facts = served_facts
                .lock()
                .expect("dynamic facts lock is not poisoned")
                .clone();
            let response =
                serde_json::to_vec(&MachineFactsGetRpcResponse::Ok(MachineFactsGetRpcOk {
                    facts,
                }))
                .expect("facts response serializes");
            let _ = client.publish(reply, response.into()).await;
        }
    });
    (task, facts)
}

fn facts_reader(client: &async_nats::Client, timeout: Duration) -> NatsMachineFactsReader {
    NatsMachineFactsReader::new(client.clone()).with_request_timeout(timeout)
}

fn intent_reader(client: &async_nats::Client, timeout: Duration) -> NatsIntentReader {
    NatsIntentReader::new(client.clone()).with_request_timeout(timeout)
}

fn empty_machine_facts(machine_id: &ployz_core::ids::MachineId) -> MachineFactsSnapshot {
    machine_facts(machine_id.clone(), [])
}

fn machine_facts(
    machine_id: ployz_core::ids::MachineId,
    containers: impl IntoIterator<Item = ployz_core::machine::runtime::ManagedContainerObservation>,
) -> MachineFactsSnapshot {
    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        MachineContainerObservationSnapshot::try_new(machine_id, containers)
            .expect("machine snapshot is valid"),
        None,
        test_disk_space(),
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("empty machine facts are valid")
}

fn test_disk_space() -> ployz_core::machine::runtime::MachineDiskSpace {
    ployz_test_support::fixtures::test_disk_space()
}

async fn operation_controllers(client: async_nats::Client) -> OperationControllers {
    let evidence_dir = tempfile::tempdir()
        .expect("operation evidence temp dir")
        .keep();
    let core_store = crate::control::store::CoreStore::open(evidence_dir.join("ployz-core.db"))
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
        target: ployz_core::deploy::VolumeDeclaredDeployRequest::try_new(target)
            .expect("deploy request normalizes"),
    }
}

fn normalized_deploy_request(replicas: u16) -> ployz_core::deploy::VolumeDeclaredDeployRequest {
    ployz_core::deploy::VolumeDeclaredDeployRequest::try_new(deploy_request(replicas))
        .expect("deploy request normalizes")
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
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
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

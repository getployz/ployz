use std::error::Error;
use std::time::Duration;

use async_nats::jetstream;
use ployz_core::dataplane::{
    EbpfForwardingReady, EbpfForwardingReadyEvidence, PloyzNativeMeshMachineReady,
    PloyzNativeMeshPrepareReport, PloyzNativeMeshReady, WireGuardPublicKey, WireGuardReady,
    WireGuardReadyEvidence,
};
use ployz_core::deploy::{
    DeployPlanningInput, DeployRequest, DeployRoute, DeployServiceRequest, DeployServiceSpec,
    ImageReference, ReplicaCount, plan_namespace_deploy,
};
use ployz_core::ids::OperationId;
use ployz_core::install::MachineBootstrapUrl;
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, DeployRunningStage, DeployTransition,
    EventSequence, OperationEvent, OperationEventReplayCursor, OperationEventReplayRequest,
    OperationStatus, RouteTarget,
};
use ployz_core::state::{MachinePublicIpObservation, RouteBindingState};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    DeployOperationSubmission,
};
use ployz_sdk_types::{DeploySubmitRequest, OpsStatusRequest};
use ployzctl::api_client::OperationApiClient;
use ployzd::controllers::MachineAddBootstrapConfig;
use ployzd::deploy_worker::{
    MachineContainerRuntime, MachineContainerRuntimeError, MachineRuntimeUnavailableReason,
};
use ployzd::gateway_process_runtime::start_gateway_process_runtime_with_client;
use ployzd::machine_runtime::client::NatsMachineContainerRuntime;
use ployzd::machine_runtime::protocol::MachineContainerRunRpcRequest;
use ployzd::machine_runtime::service::start_machine_runtime_service;

mod support;

use ployz_test_support::ops::wait_for_terminal_status;
use support::machine_runtime::{ObservingContainerRunner, ReadyWireGuardEbpf};

use ployz_test_support::containers;
use ployz_test_support::ids::{
    event_replay_limit, event_sequence, machine_id, namespace_id, namespace_revision_entry_id,
    namespace_revision_id, operation_id, route_hostname, route_port, service_id,
};
use support::http::{TestUpstream, free_loopback_port, http_get_with_host};
use support::nats::TestNats;

#[tokio::test]
async fn e2e_operations_over_real_nats() -> Result<(), Box<dyn Error + Send + Sync>> {
    e2e_repository_submit_and_transition_over_real_nats().await?;
    e2e_deploy_submit_service_accepts_operation_over_real_nats().await?;

    Ok(())
}

async fn e2e_repository_submit_and_transition_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[]).await;
    let client = nats.controller_client();
    let jetstream = jetstream::new(client.clone());
    bootstrap_nats_resources(&client, &jetstream).await?;
    let event_log = AsyncNatsOperationEventLog::new(jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&jetstream)
        .await
        .expect("open operation status store");
    let repository = AsyncNatsOperationRepository::new(event_log.clone(), status_store.clone());

    let accepted = repository
        .submit_deploy(DeployOperationSubmission {
            operation_id: operation_id("op_123"),
            target: deploy_target("svc_api"),
        })
        .await
        .expect("submit deploy over real nats");
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("record planning transition over real nats");

    assert_eq!(accepted.operation_id, operation_id("op_123"));
    assert_eq!(accepted.start_sequence, event_sequence(1));
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_123"))
            .await
            .expect("operation status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Planning,
            last_event_sequence: event_sequence(2),
        })
    );
    assert_eq!(
        operation_replay_page(&repository, accepted.start_sequence)
            .await
            .events
            .into_iter()
            .map(|event| event.event)
            .collect::<Vec<_>>(),
        vec![
            OperationEvent::DeploySubmitted {
                operation_id: operation_id("op_123"),
                target: deploy_target("svc_api"),
            },
            OperationEvent::DeployPlanningStarted {
                operation_id: operation_id("op_123"),
            },
        ]
    );
    assert_eq!(
        operation_replay_page(&repository, accepted.start_sequence)
            .await
            .cursor,
        OperationEventReplayCursor::CaughtUp
    );

    Ok(())
}

async fn e2e_deploy_submit_service_accepts_operation_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[]).await;
    let client = nats.controller_client();
    let config = nats
        .control_config(machine_id("core_1"))
        .with_machine_bootstrap(machine_bootstrap_config());
    let _runtime =
        ployzd::control_runtime::start_control_runtime_with_client(client.clone(), &config).await?;
    let api = OperationApiClient::new(nats.user_client());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_api_123"),
        target: deploy_target("svc_api"),
    };

    let accepted = api.deploy_submit(&request).await?;

    assert_eq!(accepted.operation_id, operation_id("op_api_123"));
    assert_eq!(accepted.watch_subject, "plz.v1.op.op_api_123.>".to_owned());
    assert_eq!(accepted.start_sequence, event_sequence(1));
    // The control runtime starts executing the accepted deploy immediately,
    // so status may have advanced past Accepted by the time we read it. The
    // acceptance contract is: status is reachable for the id and the first
    // durable event is the submission.
    let status_request = OpsStatusRequest {
        operation_id: operation_id("op_api_123"),
    };
    let status = api.ops_status(&status_request).await?;
    let OperationStatus::Deploy {
        id,
        service_id: status_service_id,
        ..
    } = status.status
    else {
        panic!("submitted deploy should report a deploy status");
    };
    assert_eq!(id, operation_id("op_api_123"));
    assert_eq!(status_service_id, service_id("svc_api"));

    let watch_request = OperationEventReplayRequest {
        operation_id: operation_id("op_api_123"),
        start_sequence: event_sequence(1),
        limit: event_replay_limit(10),
    };
    let page = api.ops_watch(&watch_request).await?;
    let [first, ..] = page.events.as_slice() else {
        panic!("watch from sequence 1 should replay the submission event");
    };
    assert_eq!(
        first.event,
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_api_123"),
            target: deploy_target("svc_api"),
        }
    );

    Ok(())
}

#[tokio::test]
async fn e2e_control_and_machine_complete_deploy_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let client = nats.controller_client();
    let jetstream = jetstream::new(client.clone());
    let config = nats
        .control_config(machine_id("core_1"))
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2))
        .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime =
        ployzd::control_runtime::start_control_runtime_with_client(client.clone(), &config).await?;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let observations =
        AsyncNatsObservationStore::from_jetstream(&jetstream::new(machine_client.clone()))
            .await
            .expect("open observation store");
    let runner = ObservingContainerRunner::new(machine_id("machine_a"), observations.clone());
    let machine_runtime = start_machine_runtime_service(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner,
    )
    .await?;
    let api = OperationApiClient::new(nats.user_client());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_e2e_run"),
        target: deploy_target("svc_api"),
    };

    let accepted = api.deploy_submit(&request).await?;

    assert_eq!(accepted.operation_id, operation_id("op_e2e_run"));
    let status =
        wait_for_terminal_status(&api, &operation_id("op_e2e_run"), Duration::from_secs(4)).await;
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
        "expected deploy to complete, got {status:?}"
    );
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    assert_eq!(
        core_state
            .serving_target_entry(&namespace_id("default"), &service_id("svc_api"))
            .await
            .expect("serving target entry reads")
            .expect("serving target committed")
            .namespace_revision_entry_id,
        namespace_revision_entry_id("rev_2")
    );
    assert_eq!(
        operation_events(&api, operation_id("op_e2e_run"), accepted.start_sequence).await?,
        vec![
            OperationEvent::DeploySubmitted {
                operation_id: operation_id("op_e2e_run"),
                target: deploy_target("svc_api"),
            },
            OperationEvent::DeployPlanningStarted {
                operation_id: operation_id("op_e2e_run"),
            },
            OperationEvent::DeployPlanCreated {
                operation_id: operation_id("op_e2e_run"),
                plan: plan_namespace_deploy(
                    namespace_id("default"),
                    namespace_revision_id("rev_2"),
                    vec![DeployPlanningInput {
                        request: deploy_service_target("svc_api"),
                        eligible_machines: vec![machine_id("machine_a")],
                        existing_replicas: Vec::new(),
                        cleanup_candidates: Vec::new(),
                    }],
                    Vec::new(),
                )
                .expect("single-machine deploy plan is valid"),
            },
            OperationEvent::DeployRunning {
                operation_id: operation_id("op_e2e_run"),
                stage: DeployRunningStage::PreparingDataplane,
            },
            OperationEvent::DeployDataplanePrepared {
                operation_id: operation_id("op_e2e_run"),
                report: PloyzNativeMeshPrepareReport {
                    machines: vec![PloyzNativeMeshMachineReady {
                        machine_id: machine_id("machine_a"),
                        ready: PloyzNativeMeshReady {
                            wireguard: WireGuardReady {
                                public_key: wireguard_public_key("test-public-key"),
                                evidence: vec![WireGuardReadyEvidence::Command {
                                    program: "wg".to_owned(),
                                    args: vec!["--version".to_owned()],
                                }],
                            },
                            ebpf_forwarding: EbpfForwardingReady {
                                evidence: vec![EbpfForwardingReadyEvidence::PloyzTcBytecode {
                                    path: "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".to_owned(),
                                    symbols: vec![
                                        "ployz_egress".to_owned(),
                                        "ployz_ingress".to_owned(),
                                    ],
                                }],
                            },
                        },
                    }],
                },
            },
            OperationEvent::DeployRunning {
                operation_id: operation_id("op_e2e_run"),
                stage: DeployRunningStage::StartingContainers,
            },
            OperationEvent::DeployContainerStarted {
                operation_id: operation_id("op_e2e_run"),
                machine_id: machine_id("machine_a"),
                container_id: ployz_core::ids::ContainerId::try_new("ctr_1")
                    .expect("valid container id"),
            },
            OperationEvent::DeployRunning {
                operation_id: operation_id("op_e2e_run"),
                stage: DeployRunningStage::WaitingForHealth,
            },
            OperationEvent::DeployHealthCheckStarted {
                operation_id: operation_id("op_e2e_run"),
            },
            OperationEvent::DeployRunning {
                operation_id: operation_id("op_e2e_run"),
                stage: DeployRunningStage::ServingTargetCommit,
            },
            OperationEvent::DeployCompleted {
                operation_id: operation_id("op_e2e_run"),
                outcome: DeployCompletionOutcome::Completed,
            },
        ]
    );
    assert_eq!(
        observations
            .machine_snapshot(&machine_id("machine_a"))
            .await
            .expect("machine observations read")
            .expect("machine snapshot exists")
            .containers()
            .len(),
        1
    );

    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_routed_deploy_serves_http_through_gateway() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let client = nats.controller_client();
    let config = nats
        .control_config(machine_id("core_1"))
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2))
        .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime =
        ployzd::control_runtime::start_control_runtime_with_client(client.clone(), &config).await?;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let observations =
        AsyncNatsObservationStore::from_jetstream(&jetstream::new(machine_client.clone()))
            .await
            .expect("open observation store");
    observations
        .replace_machine_public_ip(&machine_public_ip("machine_a", 7))
        .await
        .expect("machine public ip stores");
    let runner = ObservingContainerRunner::new(machine_id("machine_a"), observations);
    let machine_runtime = start_machine_runtime_service(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner,
    )
    .await?;
    let gateway_runtime = start_gateway_process_runtime_with_client(
        machine_client.clone(),
        Duration::from_millis(10),
        "127.0.0.1:0".parse().expect("valid gateway listen addr"),
        machine_id("machine_a"),
    )
    .await?;
    let upstream = TestUpstream::start().await;
    let api = OperationApiClient::new(nats.user_client());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_e2e_route"),
        target: deploy_target_with_route(
            "svc_api",
            "api.example.com",
            gateway_runtime.listen_addr().port(),
            upstream.port(),
        ),
    };

    let accepted = api.deploy_submit(&request).await?;

    let status =
        wait_for_terminal_status(&api, &operation_id("op_e2e_route"), Duration::from_secs(4)).await;
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
        "expected routed deploy to complete, got {status:?}"
    );
    assert_eq!(accepted.operation_id, operation_id("op_e2e_route"));
    wait_for_gateway_route(&gateway_runtime).await;

    assert_smoke_response(
        &http_get_with_host(gateway_runtime.listen_addr(), "api.example.com").await?,
    );
    let upstream_request = upstream.request().await;
    assert!(upstream_request.starts_with("GET /smoke HTTP/1.1\r\n"));
    assert!(upstream_request.contains("\r\nHost: api.example.com\r\n"));

    gateway_runtime.shutdown().await;
    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_gateway_serves_route_after_machine_runtime_shutdown()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let client = nats.controller_client();
    let config = nats
        .control_config(machine_id("core_1"))
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2))
        .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime =
        ployzd::control_runtime::start_control_runtime_with_client(client.clone(), &config).await?;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let observations =
        AsyncNatsObservationStore::from_jetstream(&jetstream::new(machine_client.clone()))
            .await
            .expect("open observation store");
    observations
        .replace_machine_public_ip(&machine_public_ip("machine_a", 7))
        .await
        .expect("machine public ip stores");
    let runner = ObservingContainerRunner::new(machine_id("machine_a"), observations);
    let machine_runtime = start_machine_runtime_service(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner,
    )
    .await?;
    let gateway_runtime = start_gateway_process_runtime_with_client(
        machine_client.clone(),
        Duration::from_millis(10),
        "127.0.0.1:0".parse().expect("valid gateway listen addr"),
        machine_id("machine_a"),
    )
    .await?;
    let upstream = TestUpstream::start_with_expected_requests(2).await;
    let api = OperationApiClient::new(nats.user_client());
    let route_port = route_port(gateway_runtime.listen_addr().port());
    let route_host = format!("machine-down.local:{}", route_port.get());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_e2e_machine_runtime_down_route"),
        target: deploy_target_with_route(
            "svc_api",
            "machine-down.local",
            route_port.get(),
            upstream.port(),
        ),
    };

    api.deploy_submit(&request).await?;

    let status = wait_for_terminal_status(
        &api,
        &operation_id("op_e2e_machine_runtime_down_route"),
        Duration::from_secs(4),
    )
    .await;
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
        "expected routed deploy to complete, got {status:?}"
    );
    wait_for_gateway_upstream(&gateway_runtime, "127.0.0.1", upstream.port()).await;
    assert_smoke_response(&http_get_with_host(gateway_runtime.listen_addr(), &route_host).await?);

    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
    let mut machine_rpc = NatsMachineContainerRuntime::new(client.clone())
        .with_request_timeout(Duration::from_millis(200));
    assert_eq!(
        machine_rpc
            .run_container(&machine_id("machine_a"), machine_rpc_probe_request())
            .await
            .expect_err("machine service is unavailable after machine runtime shutdown"),
        MachineContainerRuntimeError::Unavailable {
            machine_id: machine_id("machine_a"),
            reason: MachineRuntimeUnavailableReason::NoResponders,
        }
    );
    assert_smoke_response(&http_get_with_host(gateway_runtime.listen_addr(), &route_host).await?);
    assert_eq!(upstream.requests().await.len(), 2);

    gateway_runtime.shutdown().await;
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_gateway_serves_and_applies_route_changes_after_control_shutdown()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let client = nats.controller_client();
    let jetstream = jetstream::new(client.clone());
    let config = nats
        .control_config(machine_id("core_1"))
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2))
        .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime =
        ployzd::control_runtime::start_control_runtime_with_client(client.clone(), &config).await?;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let observations =
        AsyncNatsObservationStore::from_jetstream(&jetstream::new(machine_client.clone()))
            .await
            .expect("open observation store");
    observations
        .replace_machine_public_ip(&machine_public_ip("machine_a", 7))
        .await
        .expect("machine public ip stores");
    let runner = ObservingContainerRunner::new(machine_id("machine_a"), observations.clone());
    let machine_runtime = start_machine_runtime_service(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner,
    )
    .await?;
    let gateway_runtime = start_gateway_process_runtime_with_client(
        machine_client.clone(),
        Duration::from_millis(10),
        "127.0.0.1:0".parse().expect("valid gateway listen addr"),
        machine_id("machine_a"),
    )
    .await?;
    let first_upstream = TestUpstream::start().await;
    let first_upstream_port = first_upstream.port();
    let api = OperationApiClient::new(nats.user_client());
    let route_hostname = route_hostname("control-down.local");
    let route_port = route_port(gateway_runtime.listen_addr().port());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_e2e_control_down_route"),
        target: deploy_target_with_route(
            "svc_api",
            route_hostname.as_str(),
            route_port.get(),
            first_upstream_port,
        ),
    };

    api.deploy_submit(&request).await?;

    let status = wait_for_terminal_status(
        &api,
        &operation_id("op_e2e_control_down_route"),
        Duration::from_secs(4),
    )
    .await;
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
        "expected routed deploy to complete, got {status:?}"
    );
    wait_for_gateway_upstream(&gateway_runtime, "127.0.0.1", first_upstream_port).await;
    assert_smoke_response(
        &http_get_with_host(
            gateway_runtime.listen_addr(),
            &format!("control-down.local:{}", route_port.get()),
        )
        .await?,
    );
    let first_request = first_upstream.request().await;
    assert!(first_request.starts_with("GET /smoke HTTP/1.1\r\n"));
    assert!(
        first_request
            .contains(&("Host: control-down.local:".to_owned() + &route_port.get().to_string()))
    );

    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    let second_upstream = TestUpstream::start().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    routes
        .replace_route_binding(&RouteBindingState {
            namespace_id: namespace_id("default"),
            target: RouteTarget::new(route_hostname.clone(), route_port),
            endpoint_port: self::route_port(second_upstream.port()),
            service_id: service_id("svc_api"),
        })
        .await
        .expect("route can change without control runtime");
    observations
        .replace_machine_containers(&containers::snapshot(
            "machine_a",
            [
                containers::observation("machine_a", "ctr_after_control_down")
                    .entry("rev_2")
                    .operation("op_e2e_control_down_route")
                    .step("step_after_control_down")
                    .running_at(endpoint_ip("127.0.0.1")),
            ],
        ))
        .await
        .expect("observation can change without control runtime");

    wait_for_gateway_upstream(&gateway_runtime, "127.0.0.1", second_upstream.port()).await;
    assert_smoke_response(
        &http_get_with_host(
            gateway_runtime.listen_addr(),
            &format!("control-down.local:{}", route_port.get()),
        )
        .await?,
    );
    let second_request = second_upstream.request().await;
    assert!(second_request.starts_with("GET /smoke HTTP/1.1\r\n"));
    assert!(
        second_request
            .contains(&("Host: control-down.local:".to_owned() + &route_port.get().to_string()))
    );

    gateway_runtime.shutdown().await;
    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_two_machine_routed_deploy_serves_through_both_gateways()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[machine_id("core_1"), machine_id("edge_2")]).await;
    let client = nats.controller_client();
    let route_port = free_loopback_port().await?;
    let config = nats
        .control_config(machine_id("core_1"))
        .with_deploy_machines(vec![machine_id("core_1"), machine_id("edge_2")])
        .with_deploy_step_timeout(Duration::from_secs(2))
        .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime =
        ployzd::control_runtime::start_control_runtime_with_client(client.clone(), &config).await?;
    let core_machine_client = nats.machine_client(&machine_id("core_1")).await;
    let edge_machine_client = nats.machine_client(&machine_id("edge_2")).await;
    let observations =
        AsyncNatsObservationStore::from_jetstream(&jetstream::new(core_machine_client.clone()))
            .await
            .expect("open core observation store");
    let edge_observations =
        AsyncNatsObservationStore::from_jetstream(&jetstream::new(edge_machine_client.clone()))
            .await
            .expect("open edge observation store");
    observations
        .replace_machine_public_ip(&machine_public_ip("core_1", 1))
        .await
        .expect("core public ip stores");
    edge_observations
        .replace_machine_public_ip(&machine_public_ip("edge_2", 2))
        .await
        .expect("edge public ip stores");
    let core_runner = ObservingContainerRunner::new(machine_id("core_1"), observations.clone());
    let edge_runner =
        ObservingContainerRunner::new(machine_id("edge_2"), edge_observations.clone());
    let core_machine_runtime = start_machine_runtime_service(
        core_machine_client.clone(),
        machine_id("core_1"),
        core_runner.clone(),
        ReadyWireGuardEbpf,
        core_runner,
    )
    .await?;
    let edge_machine_runtime = start_machine_runtime_service(
        edge_machine_client.clone(),
        machine_id("edge_2"),
        edge_runner.clone(),
        ReadyWireGuardEbpf,
        edge_runner,
    )
    .await?;
    let core_gateway_runtime = start_gateway_process_runtime_with_client(
        core_machine_client.clone(),
        Duration::from_millis(10),
        format!("127.0.0.1:{route_port}").parse()?,
        machine_id("core_1"),
    )
    .await?;
    let edge_gateway_runtime = start_gateway_process_runtime_with_client(
        edge_machine_client.clone(),
        Duration::from_millis(10),
        format!("[::1]:{route_port}").parse()?,
        machine_id("edge_2"),
    )
    .await?;
    let upstream = TestUpstream::start_with_expected_requests(2).await;
    let api = OperationApiClient::new(nats.user_client());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_e2e_two_machine_route"),
        target: deploy_target_with_route("svc_api", "smoke.local", route_port, upstream.port()),
    };

    let accepted = api.deploy_submit(&request).await?;

    let status = wait_for_terminal_status(
        &api,
        &operation_id("op_e2e_two_machine_route"),
        Duration::from_secs(4),
    )
    .await;
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
        "expected two-machine routed deploy to complete, got {status:?}"
    );
    assert_eq!(
        operation_events(
            &api,
            operation_id("op_e2e_two_machine_route"),
            accepted.start_sequence,
        )
        .await?
        .into_iter()
        .filter_map(|event| {
            let OperationEvent::DeployDataplanePrepared { report, .. } = event else {
                return None;
            };
            Some(
                report
                    .machines
                    .into_iter()
                    .map(|machine| machine.machine_id.clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>(),
        vec![vec![machine_id("core_1"), machine_id("edge_2")]]
    );
    wait_for_gateway_route(&core_gateway_runtime).await;
    wait_for_gateway_route(&edge_gateway_runtime).await;
    assert_eq!(
        observations
            .machine_snapshot(&machine_id("core_1"))
            .await
            .expect("core observations read")
            .expect("core snapshot exists")
            .containers()
            .len(),
        1
    );
    assert_smoke_response(
        &http_get_with_host(
            core_gateway_runtime.listen_addr(),
            &format!("smoke.local:{route_port}"),
        )
        .await?,
    );
    assert_smoke_response(
        &http_get_with_host(
            edge_gateway_runtime.listen_addr(),
            &format!("smoke.local:{route_port}"),
        )
        .await?,
    );
    assert_eq!(upstream.requests().await.len(), 2);

    edge_gateway_runtime.shutdown().await;
    core_gateway_runtime.shutdown().await;
    edge_machine_runtime
        .shutdown()
        .await
        .expect("edge machine runtime shuts down");
    core_machine_runtime
        .shutdown()
        .await
        .expect("core machine runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

async fn operation_replay_page(
    repository: &AsyncNatsOperationRepository,
    start_sequence: EventSequence,
) -> ployz_core::ops::OperationEventReplayPage {
    repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_123"),
            start_sequence,
            limit: event_replay_limit(10),
        })
        .await
        .expect("operation event replay succeeds")
}

async fn operation_events(
    api: &OperationApiClient,
    operation_id: OperationId,
    start_sequence: EventSequence,
) -> Result<Vec<OperationEvent>, Box<dyn Error + Send + Sync>> {
    let page = api
        .ops_watch(&OperationEventReplayRequest {
            operation_id,
            start_sequence,
            limit: event_replay_limit(32),
        })
        .await?;
    assert_eq!(page.cursor, OperationEventReplayCursor::Terminal);

    Ok(page.events.into_iter().map(|event| event.event).collect())
}

async fn bootstrap_nats_resources(
    client: &async_nats::Client,
    jetstream: &jetstream::Context,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let plan = ployz_nats::bootstrap::BootstrapPlan::for_single_server_client(client)?;
    ployz_nats::bootstrap::assure_nats_resources(jetstream, &plan)
        .await
        .map_err(Into::into)
}

fn wireguard_public_key(value: &str) -> WireGuardPublicKey {
    WireGuardPublicKey::try_new(value).expect("valid wireguard public key")
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        namespace_revision_id: namespace_revision_id("rev_2"),
        services: vec![DeployServiceSpec {
            service_id: self::service_id(service_id),
            image: image("ghcr.io/acme/api:rev-2"),
            replicas: replicas(1),
            routes: Vec::new(),
        }],
    }
}

fn deploy_service_target(service_id: &str) -> DeployServiceRequest {
    deploy_target(service_id)
        .service_requests()
        .into_iter()
        .next()
        .expect("deploy target has one service")
}

fn deploy_target_with_route(
    service_id: &str,
    hostname: &str,
    route_port: u16,
    endpoint_port: u16,
) -> DeployRequest {
    let mut target = deploy_target(service_id);
    let [service] = target.services.as_mut_slice() else {
        panic!("deploy target has one service");
    };
    service.routes = vec![DeployRoute {
        target: RouteTarget::new(route_hostname(hostname), self::route_port(route_port)),
        endpoint_port: self::route_port(endpoint_port),
    }];
    target
}

fn machine_rpc_probe_request() -> MachineContainerRunRpcRequest {
    MachineContainerRunRpcRequest {
        image: image("ghcr.io/acme/api:probe"),
        container: containers::identity("svc_probe")
            .entry("rev_probe")
            .operation("op_probe")
            .step("step_probe")
            .build(),
    }
}

async fn wait_for_gateway_route(
    runtime: &ployzd::gateway_process_runtime::RunningGatewayProcessRuntime,
) {
    for _ in 0..200 {
        if runtime
            .served_projection()
            .is_some_and(|projection| !projection.routes.is_empty())
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    panic!("gateway route did not become visible");
}

async fn wait_for_gateway_upstream(
    runtime: &ployzd::gateway_process_runtime::RunningGatewayProcessRuntime,
    endpoint_ip: &str,
    endpoint_port: u16,
) {
    for _ in 0..200 {
        if runtime.served_projection().is_some_and(|projection| {
            projection.routes.iter().any(|route| {
                route.upstreams.iter().any(|upstream| {
                    upstream.address.ip().to_string() == endpoint_ip
                        && upstream.address.port() == endpoint_port
                })
            })
        }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    panic!("gateway upstream did not become visible");
}

fn machine_public_ip(machine_id: &str, last_octet: u8) -> MachinePublicIpObservation {
    MachinePublicIpObservation {
        machine_id: self::machine_id(machine_id),
        public_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, last_octet)),
    }
}

fn endpoint_ip(ip: &str) -> std::net::IpAddr {
    ip.parse().expect("valid endpoint ip")
}

fn assert_smoke_response(response: &str) {
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("\r\n\r\nsmoke"));
}

fn machine_bootstrap_config() -> MachineAddBootstrapConfig {
    MachineAddBootstrapConfig::new(
        MachineBootstrapUrl::try_new(ployz_core::install::DEFAULT_MACHINE_BOOTSTRAP_URL)
            .expect("valid bootstrap url"),
    )
    .with_join_material(
        ployz_test_support::fixtures::machine_join_template(),
        ployz_core::install::MachineJoinSecretDelivery {
            nats_credentials: ployz_core::nats_config::NatsUserSeed::try_new(
                "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM",
            )
            .expect("valid seed"),
        },
    )
}

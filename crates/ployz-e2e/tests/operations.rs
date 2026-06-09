use std::error::Error;
use std::time::Duration;

use async_nats::jetstream;
use ployz_core::dataplane::{
    EbpfForwardingReady, EbpfForwardingReadyEvidence, WireGuardEbpfNodeReady,
    WireGuardEbpfPrepareReport, WireGuardEbpfReady, WireGuardPublicKey, WireGuardReady,
    WireGuardReadyEvidence,
};
use ployz_core::deploy::{
    DeployPlanningInput, DeployRequest, DeployRoute, ImageReference, ReplicaCount,
    plan_service_deploy,
};
use ployz_core::ids::{
    ContainerId, NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId, StepId,
};
use ployz_core::install::{MachineBootstrapUrl, MachineJoinTemplate};
use ployz_core::node::{
    ContainerEndpoint, ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, DeployRunningStage, DeployTransition,
    EventSequence, OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationIdempotencyKey, OperationLeaseExpiresAt,
    OperationOwnershipStatus, OperationStatus, RouteHostname, RoutePort, RouteTarget,
};
use ployz_core::state::{
    ActiveRouteCommitRequest, ExpectedActiveRoute, ExpectedActiveRouteRevision,
    NodePublicIpObservation,
};
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    DeployOperationSubmission, OperationLeaseClaim,
};
use ployz_sdk_types::{DeploySubmitRequest, OpsStatusRequest};
use ployz_test_support::node::{ObservingContainerRunner, ReadyWireGuardEbpf};
use ployzctl::api_client::OperationApiClient;
use ployzd::config::ControlProcessConfig;
use ployzd::controllers::{MachineAddBootstrapConfig, OperationControllers};
use ployzd::gateway_process_runtime::start_gateway_process_runtime_with_client;
use ployzd::nats_process::NatsServerRuntime;
use ployzd::node_runtime::start_node_runtime_with_ports;

mod support;

use support::http::{TestUpstream, free_loopback_port, http_get_with_host};
use support::nats::TestNats;
use support::nats::start_edge_nats_tunnel;

#[tokio::test]
async fn e2e_operations_over_real_nats() -> Result<(), Box<dyn Error + Send + Sync>> {
    e2e_repository_submit_and_transition_over_real_nats().await?;
    e2e_deploy_submit_service_accepts_operation_over_real_nats().await?;

    Ok(())
}

async fn e2e_repository_submit_and_transition_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_jetstream().await?;
    let client = async_nats::connect(nats.url()).await?;
    let jetstream = jetstream::new(client.clone());
    bootstrap_nats_resources(&client, &jetstream).await?;
    let event_log = AsyncNatsOperationEventLog::new(jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&jetstream)
        .await
        .expect("open operation status store");
    let repository = AsyncNatsOperationRepository::new(event_log.clone(), status_store.clone());

    let accepted = repository
        .submit_deploy(
            DeployOperationSubmission {
                operation_id: operation_id("op_123"),
                target: deploy_target("svc_api"),
                idempotency_key: idempotency_key("idem_1"),
            },
            test_lease_claim(),
        )
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
            .operation_status(&operation_id("op_123"))
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
    let nats = TestNats::start_jetstream().await?;
    let client = async_nats::connect(nats.url()).await?;
    let jetstream = jetstream::new(client.clone());
    bootstrap_nats_resources(&client, &jetstream).await?;
    let event_log = AsyncNatsOperationEventLog::new(jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&jetstream)
        .await
        .expect("open operation status store");
    let repository = AsyncNatsOperationRepository::new(event_log.clone(), status_store.clone());
    let controllers = OperationControllers::with_owner(
        event_log,
        status_store,
        OperationOwnerId::try_new("control").expect("valid owner id"),
        machine_bootstrap_config(),
    );
    let _runtime = ployzd::api_runtime::start_operation_api_service(client.clone(), controllers)
        .await
        .expect("api service starts");
    let api = OperationApiClient::new(client.clone());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_api_123"),
        target: deploy_target("svc_api"),
        idempotency_key: idempotency_key("idem_api_1"),
    };

    let accepted = api.deploy_submit(&request).await?;

    assert_eq!(accepted.operation_id, operation_id("op_api_123"));
    let lease = accepted.owner_lease;
    assert_eq!(lease.operation_id, operation_id("op_api_123"));
    assert_eq!(
        lease.owner_id,
        OperationOwnerId::try_new("control").expect("valid owner id")
    );
    assert!(lease.expires_at.unix_seconds() > 0);
    assert_eq!(accepted.watch_subject, "plz.v1.op.op_api_123.>".to_owned());
    assert_eq!(accepted.start_sequence, event_sequence(1));
    assert_eq!(
        repository
            .operation_status(&operation_id("op_api_123"))
            .await
            .expect("operation status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_api_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Accepted,
            last_event_sequence: event_sequence(1),
        })
    );
    let status_request = OpsStatusRequest {
        operation_id: operation_id("op_api_123"),
    };
    let status = api.ops_status(&status_request).await?;
    assert_eq!(
        status.status,
        OperationStatus::Deploy {
            id: operation_id("op_api_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Accepted,
            last_event_sequence: event_sequence(1),
        }
    );
    let OperationOwnershipStatus::Owned { lease } = status.ownership else {
        panic!("accepted operation should expose active ownership");
    };
    assert_eq!(lease.operation_id, operation_id("op_api_123"));
    assert_eq!(
        lease.owner_id,
        OperationOwnerId::try_new("control").expect("valid owner id")
    );

    let watch_request = OperationEventReplayRequest {
        operation_id: operation_id("op_api_123"),
        start_sequence: event_sequence(1),
        limit: event_replay_limit(10),
    };
    let page = api.ops_watch(&watch_request).await?;
    assert_eq!(
        page.events
            .into_iter()
            .map(|event| event.event)
            .collect::<Vec<_>>(),
        vec![OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_api_123"),
            target: deploy_target("svc_api"),
        }]
    );
    assert_eq!(page.cursor, OperationEventReplayCursor::CaughtUp);

    Ok(())
}

#[tokio::test]
async fn e2e_control_and_node_complete_deploy_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_jetstream().await?;
    let client = async_nats::connect(nats.url()).await?;
    let jetstream = jetstream::new(client.clone());
    let config = ControlProcessConfig::new(
        NatsServerRuntime::External(nats_client_url(nats.url())),
        node_id("core_1"),
    )
    .with_deploy_nodes(vec![node_id("node_a")])
    .with_deploy_step_timeout(Duration::from_secs(2))
    .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime =
        ployzd::control_runtime::start_control_runtime_with_client(client.clone(), &config).await?;
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
        .await
        .expect("open observation store");
    let runner = ObservingContainerRunner::new(node_id("node_a"), observations.clone());
    let node_runtime = start_node_runtime_with_ports(
        client.clone(),
        node_id("node_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner,
    )
    .await?;
    let api = OperationApiClient::new(client.clone());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_e2e_run"),
        target: deploy_target("svc_api"),
        idempotency_key: idempotency_key("idem_e2e_run"),
    };

    let accepted = api.deploy_submit(&request).await?;

    assert_eq!(accepted.operation_id, operation_id("op_e2e_run"));
    let status = wait_for_terminal_deploy_status(&api, operation_id("op_e2e_run")).await;
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
            .active_service(&service_id("svc_api"))
            .await
            .expect("active service reads")
            .expect("active service committed")
            .active_revision,
        revision_id("rev_2")
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
                plan: plan_service_deploy(DeployPlanningInput {
                    request: deploy_target("svc_api"),
                    eligible_nodes: vec![node_id("node_a")],
                    existing_replicas: Vec::new(),
                    cleanup_candidates: Vec::new(),
                })
                .expect("single-node deploy plan is valid"),
            },
            OperationEvent::DeployRunning {
                operation_id: operation_id("op_e2e_run"),
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
            OperationEvent::DeployWireGuardEbpfPrepared {
                operation_id: operation_id("op_e2e_run"),
                report: WireGuardEbpfPrepareReport {
                    nodes: vec![WireGuardEbpfNodeReady::new(
                        node_id("node_a"),
                        WireGuardEbpfReady {
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
                    )],
                },
            },
            OperationEvent::DeployRunning {
                operation_id: operation_id("op_e2e_run"),
                stage: DeployRunningStage::StartingContainers,
            },
            OperationEvent::DeployContainerStarted {
                operation_id: operation_id("op_e2e_run"),
                node_id: node_id("node_a"),
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
                stage: DeployRunningStage::ActiveServiceCommit,
            },
            OperationEvent::DeployCompleted {
                operation_id: operation_id("op_e2e_run"),
                outcome: DeployCompletionOutcome::Completed,
            },
        ]
    );
    assert_eq!(
        observations
            .node_snapshot(&node_id("node_a"))
            .await
            .expect("node observations read")
            .expect("node snapshot exists")
            .containers()
            .len(),
        1
    );

    node_runtime
        .shutdown()
        .await
        .expect("node runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_routed_deploy_serves_http_through_gateway() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let nats = TestNats::start_jetstream().await?;
    let client = async_nats::connect(nats.url()).await?;
    let jetstream = jetstream::new(client.clone());
    let config = ControlProcessConfig::new(
        NatsServerRuntime::External(nats_client_url(nats.url())),
        node_id("core_1"),
    )
    .with_deploy_nodes(vec![node_id("node_a")])
    .with_deploy_step_timeout(Duration::from_secs(2))
    .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime =
        ployzd::control_runtime::start_control_runtime_with_client(client.clone(), &config).await?;
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
        .await
        .expect("open observation store");
    observations
        .replace_node_public_ip(&node_public_ip("node_a", 7))
        .await
        .expect("node public ip stores");
    let runner = ObservingContainerRunner::new(node_id("node_a"), observations);
    let node_runtime = start_node_runtime_with_ports(
        client.clone(),
        node_id("node_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner,
    )
    .await?;
    let gateway_runtime = start_gateway_process_runtime_with_client(
        client.clone(),
        Duration::from_millis(10),
        "127.0.0.1:0".parse().expect("valid gateway listen addr"),
        node_id("node_a"),
    )
    .await?;
    let upstream = TestUpstream::start().await;
    let api = OperationApiClient::new(client.clone());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_e2e_route"),
        target: deploy_target_with_route(
            "svc_api",
            "api.example.com",
            gateway_runtime.listen_addr().port(),
            upstream.port(),
        ),
        idempotency_key: idempotency_key("idem_e2e_route"),
    };

    let accepted = api.deploy_submit(&request).await?;

    let status = wait_for_terminal_deploy_status(&api, operation_id("op_e2e_route")).await;
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

    assert_eq!(
        http_get_with_host(gateway_runtime.listen_addr(), "api.example.com").await?,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    assert_eq!(
        upstream.request().await,
        "GET /smoke HTTP/1.1\r\nHost: api.example.com\r\n\r\n"
    );

    gateway_runtime.shutdown().await;
    node_runtime
        .shutdown()
        .await
        .expect("node runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_gateway_serves_and_applies_route_changes_after_control_shutdown()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_jetstream().await?;
    let client = async_nats::connect(nats.url()).await?;
    let jetstream = jetstream::new(client.clone());
    let config = ControlProcessConfig::new(
        NatsServerRuntime::External(nats_client_url(nats.url())),
        node_id("core_1"),
    )
    .with_deploy_nodes(vec![node_id("node_a")])
    .with_deploy_step_timeout(Duration::from_secs(2))
    .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime =
        ployzd::control_runtime::start_control_runtime_with_client(client.clone(), &config).await?;
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
        .await
        .expect("open observation store");
    observations
        .replace_node_public_ip(&node_public_ip("node_a", 7))
        .await
        .expect("node public ip stores");
    let runner = ObservingContainerRunner::new(node_id("node_a"), observations.clone());
    let node_runtime = start_node_runtime_with_ports(
        client.clone(),
        node_id("node_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner,
    )
    .await?;
    let gateway_runtime = start_gateway_process_runtime_with_client(
        client.clone(),
        Duration::from_millis(10),
        "127.0.0.1:0".parse().expect("valid gateway listen addr"),
        node_id("node_a"),
    )
    .await?;
    let first_upstream = TestUpstream::start().await;
    let first_upstream_port = first_upstream.port();
    let api = OperationApiClient::new(client.clone());
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
        idempotency_key: idempotency_key("idem_e2e_control_down_route"),
    };

    api.deploy_submit(&request).await?;

    let status =
        wait_for_terminal_deploy_status(&api, operation_id("op_e2e_control_down_route")).await;
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
    assert_eq!(
        http_get_with_host(
            gateway_runtime.listen_addr(),
            &format!("control-down.local:{}", route_port.get()),
        )
        .await?,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    assert_eq!(
        first_upstream.request().await,
        "GET /smoke HTTP/1.1\r\nHost: control-down.local:".to_owned()
            + &route_port.get().to_string()
            + "\r\n\r\n"
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
        .commit_active_route(&ActiveRouteCommitRequest {
            target: RouteTarget::try_new(route_hostname.clone(), route_port),
            endpoint_port: self::route_port(second_upstream.port()),
            expected_current: ExpectedActiveRoute::ServiceRevision(ExpectedActiveRouteRevision {
                service_id: service_id("svc_api"),
                revision_id: revision_id("rev_2"),
                endpoint_port: self::route_port(first_upstream_port),
            }),
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_2"),
        })
        .await
        .expect("route can change without control runtime");
    observations
        .replace_node_containers(
            &NodeContainerObservationSnapshot::try_new(
                node_id("node_a"),
                [ManagedContainerObservation {
                    node_id: node_id("node_a"),
                    container_id: container_id("ctr_after_control_down"),
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_2"),
                    operation_id: operation_id("op_e2e_control_down_route"),
                    step_id: step_id("step_after_control_down"),
                    kind: ManagedContainerKind::Service,
                    state: ContainerRuntimeState::running_at(endpoint(
                        "127.0.0.1",
                        second_upstream.port(),
                    )),
                }],
            )
            .expect("manual observation matches node"),
        )
        .await
        .expect("observation can change without control runtime");

    wait_for_gateway_upstream(&gateway_runtime, "127.0.0.1", second_upstream.port()).await;
    assert_eq!(
        http_get_with_host(
            gateway_runtime.listen_addr(),
            &format!("control-down.local:{}", route_port.get()),
        )
        .await?,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    assert_eq!(
        second_upstream.request().await,
        "GET /smoke HTTP/1.1\r\nHost: control-down.local:".to_owned()
            + &route_port.get().to_string()
            + "\r\n\r\n"
    );

    gateway_runtime.shutdown().await;
    node_runtime
        .shutdown()
        .await
        .expect("node runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_two_node_routed_deploy_serves_through_both_gateways()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_jetstream().await?;
    let client = async_nats::connect(nats.url()).await?;
    let jetstream = jetstream::new(client.clone());
    let route_port = free_loopback_port().await?;
    let config = ControlProcessConfig::new(
        NatsServerRuntime::External(nats_client_url(nats.url())),
        node_id("core_1"),
    )
    .with_deploy_nodes(vec![node_id("core_1"), node_id("edge_2")])
    .with_deploy_step_timeout(Duration::from_secs(2))
    .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime =
        ployzd::control_runtime::start_control_runtime_with_client(client.clone(), &config).await?;
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
        .await
        .expect("open observation store");
    observations
        .replace_node_public_ip(&node_public_ip("core_1", 1))
        .await
        .expect("core public ip stores");
    observations
        .replace_node_public_ip(&node_public_ip("edge_2", 2))
        .await
        .expect("edge public ip stores");
    let core_runner = ObservingContainerRunner::new(node_id("core_1"), observations.clone());
    let edge_runner = ObservingContainerRunner::new(node_id("edge_2"), observations.clone());
    let core_node_runtime = start_node_runtime_with_ports(
        client.clone(),
        node_id("core_1"),
        core_runner.clone(),
        ReadyWireGuardEbpf,
        core_runner,
    )
    .await?;
    let edge_node_runtime = start_node_runtime_with_ports(
        client.clone(),
        node_id("edge_2"),
        edge_runner.clone(),
        ReadyWireGuardEbpf,
        edge_runner,
    )
    .await?;
    let core_gateway_runtime = start_gateway_process_runtime_with_client(
        client.clone(),
        Duration::from_millis(10),
        format!("127.0.0.1:{route_port}").parse()?,
        node_id("core_1"),
    )
    .await?;
    let edge_gateway_runtime = start_gateway_process_runtime_with_client(
        client.clone(),
        Duration::from_millis(10),
        format!("[::1]:{route_port}").parse()?,
        node_id("edge_2"),
    )
    .await?;
    let upstream = TestUpstream::start_with_expected_requests(2).await;
    let api = OperationApiClient::new(client.clone());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_e2e_two_node_route"),
        target: deploy_target_with_route("svc_api", "smoke.local", route_port, upstream.port()),
        idempotency_key: idempotency_key("idem_e2e_two_node_route"),
    };

    let accepted = api.deploy_submit(&request).await?;

    let status = wait_for_terminal_deploy_status(&api, operation_id("op_e2e_two_node_route")).await;
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
        "expected two-node routed deploy to complete, got {status:?}"
    );
    assert_eq!(
        operation_events(
            &api,
            operation_id("op_e2e_two_node_route"),
            accepted.start_sequence,
        )
        .await?
        .into_iter()
        .filter_map(|event| match event {
            OperationEvent::DeployWireGuardEbpfPrepared { report, .. } => Some(
                report
                    .nodes
                    .into_iter()
                    .map(|node| node.node_id().clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .collect::<Vec<_>>(),
        vec![vec![node_id("core_1"), node_id("edge_2")]]
    );
    wait_for_gateway_route(&core_gateway_runtime).await;
    wait_for_gateway_route(&edge_gateway_runtime).await;
    assert_eq!(
        observations
            .node_snapshot(&node_id("core_1"))
            .await
            .expect("core observations read")
            .expect("core snapshot exists")
            .containers()
            .len(),
        1
    );
    assert_eq!(
        http_get_with_host(
            core_gateway_runtime.listen_addr(),
            &format!("smoke.local:{route_port}"),
        )
        .await?,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    assert_eq!(
        http_get_with_host(
            edge_gateway_runtime.listen_addr(),
            &format!("smoke.local:{route_port}"),
        )
        .await?,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    assert_eq!(upstream.requests().await.len(), 2);

    edge_gateway_runtime.shutdown().await;
    core_gateway_runtime.shutdown().await;
    edge_node_runtime
        .shutdown()
        .await
        .expect("edge node runtime shuts down");
    core_node_runtime
        .shutdown()
        .await
        .expect("core node runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_edge_node_and_gateway_use_nats_over_iroh_tunnel()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_jetstream().await?;
    let core_client = async_nats::connect(nats.url()).await?;
    let edge_nats = start_edge_nats_tunnel(node_id("core_1"), nats.socket_addr()).await?;
    let edge_client = async_nats::connect(edge_nats.url()).await?;
    let jetstream = jetstream::new(core_client.clone());
    let route_port = free_loopback_port().await?;
    let config = ControlProcessConfig::new(
        NatsServerRuntime::External(nats_client_url(nats.url())),
        node_id("core_1"),
    )
    .with_deploy_nodes(vec![node_id("edge_2")])
    .with_deploy_step_timeout(Duration::from_secs(2))
    .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime =
        ployzd::control_runtime::start_control_runtime_with_client(core_client.clone(), &config)
            .await?;
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
        .await
        .expect("open observation store");
    observations
        .replace_node_public_ip(&node_public_ip("core_1", 1))
        .await
        .expect("core public ip stores");
    observations
        .replace_node_public_ip(&node_public_ip("edge_2", 2))
        .await
        .expect("edge public ip stores");
    let core_runner = ObservingContainerRunner::new(node_id("core_1"), observations.clone());
    let edge_runner = ObservingContainerRunner::new(node_id("edge_2"), observations.clone());
    let core_node_runtime = start_node_runtime_with_ports(
        core_client.clone(),
        node_id("core_1"),
        core_runner.clone(),
        ReadyWireGuardEbpf,
        core_runner,
    )
    .await?;
    let edge_node_runtime = start_node_runtime_with_ports(
        edge_client.clone(),
        node_id("edge_2"),
        edge_runner.clone(),
        ReadyWireGuardEbpf,
        edge_runner,
    )
    .await?;
    let core_gateway_runtime = start_gateway_process_runtime_with_client(
        core_client.clone(),
        Duration::from_millis(10),
        format!("127.0.0.1:{route_port}").parse()?,
        node_id("core_1"),
    )
    .await?;
    let edge_gateway_runtime = start_gateway_process_runtime_with_client(
        edge_client.clone(),
        Duration::from_millis(10),
        format!("[::1]:{route_port}").parse()?,
        node_id("edge_2"),
    )
    .await?;
    let upstream = TestUpstream::start_with_expected_requests(2).await;
    let api = OperationApiClient::new(core_client.clone());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_e2e_iroh_edge_route"),
        target: deploy_target_with_route("svc_api", "smoke.local", route_port, upstream.port()),
        idempotency_key: idempotency_key("idem_e2e_iroh_edge_route"),
    };

    api.deploy_submit(&request).await?;

    let status =
        wait_for_terminal_deploy_status(&api, operation_id("op_e2e_iroh_edge_route")).await;
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
        "expected iroh-routed edge deploy to complete, got {status:?}"
    );
    wait_for_gateway_route(&core_gateway_runtime).await;
    wait_for_gateway_route(&edge_gateway_runtime).await;
    assert_eq!(
        observations
            .node_snapshot(&node_id("edge_2"))
            .await
            .expect("edge observations read")
            .expect("edge snapshot exists")
            .containers()
            .len(),
        1
    );
    assert_eq!(
        http_get_with_host(
            core_gateway_runtime.listen_addr(),
            &format!("smoke.local:{route_port}"),
        )
        .await?,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    assert_eq!(
        http_get_with_host(
            edge_gateway_runtime.listen_addr(),
            &format!("smoke.local:{route_port}"),
        )
        .await?,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    assert_eq!(upstream.requests().await.len(), 2);

    edge_gateway_runtime.shutdown().await;
    core_gateway_runtime.shutdown().await;
    edge_node_runtime
        .shutdown()
        .await
        .expect("edge node runtime shuts down");
    core_node_runtime
        .shutdown()
        .await
        .expect("core node runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
    edge_nats.shutdown().await;

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

async fn wait_for_terminal_deploy_status(
    api: &OperationApiClient,
    operation_id: OperationId,
) -> OperationStatus {
    for _ in 0..80 {
        let status = api
            .ops_status(&OpsStatusRequest {
                operation_id: operation_id.clone(),
            })
            .await
            .expect("status is readable")
            .status;
        let OperationStatus::Deploy { state, .. } = &status else {
            panic!("expected deploy status");
        };
        if state.is_terminal() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    panic!("deploy did not reach terminal status");
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

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn wireguard_public_key(value: &str) -> WireGuardPublicKey {
    WireGuardPublicKey::try_new(value).expect("valid wireguard public key")
}

fn nats_client_url(value: &str) -> NatsClientUrl {
    NatsClientUrl::try_new(value).expect("valid nats client url")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        service_id: self::service_id(service_id),
        target_revision: revision_id("rev_2"),
        image: image("ghcr.io/acme/api:rev-2"),
        replicas: replicas(1),
        route: None,
    }
}

fn deploy_target_with_route(
    service_id: &str,
    hostname: &str,
    route_port: u16,
    endpoint_port: u16,
) -> DeployRequest {
    DeployRequest {
        route: Some(DeployRoute {
            target: RouteTarget::try_new(route_hostname(hostname), self::route_port(route_port)),
            endpoint_port: self::route_port(endpoint_port),
        }),
        ..deploy_target(service_id)
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
                    upstream.endpoint.ip.to_string() == endpoint_ip
                        && upstream.endpoint.port.get() == endpoint_port
                })
            })
        }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    panic!("gateway upstream did not become visible");
}

fn route_hostname(value: &str) -> RouteHostname {
    RouteHostname::try_new(value).expect("valid route hostname")
}

fn route_port(value: u16) -> RoutePort {
    RoutePort::try_new(value).expect("valid route port")
}

fn node_public_ip(node_id: &str, last_octet: u8) -> NodePublicIpObservation {
    NodePublicIpObservation {
        node_id: self::node_id(node_id),
        public_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, last_octet)),
    }
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
}

fn endpoint(ip: &str, port: u16) -> ContainerEndpoint {
    ContainerEndpoint {
        ip: ip.parse().expect("valid endpoint ip"),
        port: route_port(port),
    }
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn event_replay_limit(value: u16) -> OperationEventReplayLimit {
    OperationEventReplayLimit::try_new(value).expect("valid event replay limit")
}

fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}

fn test_lease_claim() -> OperationLeaseClaim {
    OperationLeaseClaim::try_new(
        OperationOwnerId::try_new("control").expect("valid owner id"),
        lease_time(100),
        lease_time(160),
    )
    .expect("valid lease claim")
}

fn machine_bootstrap_config() -> MachineAddBootstrapConfig {
    MachineAddBootstrapConfig::new(
        MachineBootstrapUrl::try_new("https://get.ployz.dev/ployz.sh")
            .expect("valid bootstrap url"),
    )
    .with_join_template(machine_join_template())
}

fn machine_join_template() -> MachineJoinTemplate {
    serde_json::from_str(
        r#"{
  "join_bundle": {
    "material": {
      "cluster_name": "prod",
      "runtime_nats_url": "nats://127.0.0.1:7422",
      "trusted_nats": {
        "server_id": "server_1",
        "config_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      },
      "core_iroh": {
        "node_id": "core_1",
        "public_key": "core-public-key",
      "direct_addresses": [],
      "relay_url": null
      },
      "ployzd": {
        "version": "0.1.0",
        "source": "/tmp/ployzd",
        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "install_path": "/usr/local/bin/ployzd"
      },
      "ebpf_bytecode": {
        "version": "0.1.0",
        "source": "/tmp/ployz-ebpf-tc",
        "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
      },
      "ebpf_ctl": {
        "version": "0.1.0",
        "source": "/tmp/ployz-ebpf-ctl",
        "sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        "install_path": "/usr/local/bin/ployz-ebpf-ctl"
      }
    }
  },
  "secret_delivery": {
    "nats_credentials": "user-jwt-and-seed",
    "core_iroh_ticket": "core-ticket"
  }
}
"#,
    )
    .expect("test join template is valid")
}

fn lease_time(value: u64) -> OperationLeaseExpiresAt {
    OperationLeaseExpiresAt::try_new(value).expect("valid lease time")
}

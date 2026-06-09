use std::error::Error;
use std::net::{Ipv4Addr, SocketAddr};
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
use ployz_core::ids::{NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId};
use ployz_core::install::{MachineBootstrapUrl, MachineJoinTemplate};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, DeployRunningStage, DeployTransition,
    EventSequence, OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationIdempotencyKey, OperationLeaseExpiresAt,
    OperationOwnershipStatus, OperationStatus, RouteHostname, RoutePort, RouteTarget,
};
use ployz_core::state::NodePublicIpObservation;
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    DeployOperationSubmission, OperationLeaseClaim,
};
use ployz_sdk_types::{DeploySubmitRequest, OpsStatusRequest};
use ployz_test_support::node::{ObservingContainerRunner, ReadyWireGuardEbpf};
use ployz_transport::iroh_endpoint::IrohEndpoint;
use ployzctl::api_client::OperationApiClient;
use ployzd::config::{ControlProcessConfig, TunnelProcessConfig};
use ployzd::controllers::{MachineAddBootstrapConfig, OperationControllers};
use ployzd::gateway_process_runtime::start_gateway_process_runtime_with_client;
use ployzd::iroh_tunnel::{PreparedTunnelService, start_tunnel_runtime_with_iroh_bind_addr};
use ployzd::nats_process::NatsServerRuntime;
use ployzd::node_runtime::start_node_runtime_with_ports;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod support;

use support::nats::TestNats;

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

    let mut client = TcpStream::connect(gateway_runtime.listen_addr()).await?;
    client
        .write_all(b"GET /smoke HTTP/1.1\r\nHost: api.example.com\r\n\r\n")
        .await?;
    client.shutdown().await?;
    let mut response = String::new();
    client.read_to_string(&mut response).await?;

    assert_eq!(
        response,
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
    let edge_nats = start_edge_nats_tunnel(nats_socket(nats.url())).await?;
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

fn nats_socket(value: &str) -> SocketAddr {
    let authority = value
        .strip_prefix("nats://")
        .expect("test nats url has nats scheme")
        .trim_end_matches('/');
    let authority = authority
        .strip_prefix("localhost:")
        .map_or_else(|| authority.to_owned(), |port| format!("127.0.0.1:{port}"));
    authority
        .parse()
        .unwrap_or_else(|error| panic!("test nats url {value:?} has socket authority: {error}"))
}

struct EdgeNatsTunnel {
    url: String,
    edge: ployzd::iroh_tunnel::RunningTunnelRuntime,
    core: ployzd::iroh_tunnel::RunningTunnelRuntime,
}

impl EdgeNatsTunnel {
    fn url(&self) -> &str {
        &self.url
    }

    async fn shutdown(self) {
        self.edge.shutdown().await;
        self.core.shutdown().await;
    }
}

async fn start_edge_nats_tunnel(
    core_nats_socket: SocketAddr,
) -> Result<EdgeNatsTunnel, Box<dyn Error + Send + Sync>> {
    let core_config = TunnelProcessConfig::new(PreparedTunnelService::core(
        "e2e-nats-tunnel-core",
        core_nats_socket,
    ));
    let core = start_tunnel_runtime_with_iroh_bind_addr(
        &core_config,
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
    )
    .await?;
    let core_iroh_addr = core
        .endpoint_direct_addresses()
        .into_iter()
        .find(|addr| addr.ip().is_loopback())
        .expect("core tunnel exposes a loopback iroh address");
    let edge_config = TunnelProcessConfig::new(PreparedTunnelService::edge(
        "e2e-nats-tunnel-edge",
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        IrohEndpoint::new(node_id("core_1"), core.endpoint_public_key())
            .with_direct_address(core_iroh_addr),
    ));
    let edge = start_tunnel_runtime_with_iroh_bind_addr(
        &edge_config,
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
    )
    .await?;
    let listen_addr = edge.listen_addr().expect("edge tunnel listens locally");
    let url = format!("nats://{listen_addr}");

    Ok(EdgeNatsTunnel { url, edge, core })
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

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn event_replay_limit(value: u16) -> OperationEventReplayLimit {
    OperationEventReplayLimit::try_new(value).expect("valid event replay limit")
}

fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}

struct TestUpstream {
    addr: std::net::SocketAddr,
    requests: tokio::sync::mpsc::Receiver<String>,
    expected_requests: usize,
}

impl TestUpstream {
    async fn start() -> Self {
        Self::start_with_expected_requests(1).await
    }

    async fn start_with_expected_requests(expected_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let addr = listener.local_addr().expect("upstream local addr");
        let (request_tx, request_rx) = tokio::sync::mpsc::channel(expected_requests);
        tokio::spawn(async move {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().await.expect("accept upstream");
                let mut request = Vec::new();
                read_until_http_head(&mut stream, &mut request).await;
                let _ = request_tx
                    .send(String::from_utf8(request).expect("request is utf8"))
                    .await;
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke")
                    .await
                    .expect("write upstream response");
            }
        });

        Self {
            addr,
            requests: request_rx,
            expected_requests,
        }
    }

    fn port(&self) -> u16 {
        self.addr.port()
    }

    async fn request(self) -> String {
        let [request] = self.requests().await.try_into().unwrap_or_else(|_| {
            panic!("expected one upstream request");
        });
        request
    }

    async fn requests(mut self) -> Vec<String> {
        let mut requests = Vec::with_capacity(self.expected_requests);
        for _ in 0..self.expected_requests {
            requests.push(
                self.requests
                    .recv()
                    .await
                    .expect("upstream receives request"),
            );
        }
        requests
    }
}

async fn http_get_with_host(
    addr: std::net::SocketAddr,
    host: &str,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut client = TcpStream::connect(addr).await?;
    client
        .write_all(format!("GET /smoke HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
        .await?;
    client.shutdown().await?;
    let mut response = String::new();
    client.read_to_string(&mut response).await?;
    Ok(response)
}

async fn free_loopback_port() -> Result<u16, std::io::Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

async fn read_until_http_head(stream: &mut TcpStream, request: &mut Vec<u8>) {
    let mut chunk = [0; 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .expect("read upstream request");
        assert!(read > 0, "client closed before complete HTTP head");
        request.extend_from_slice(
            chunk
                .get(..read)
                .expect("read byte count is within buffer length"),
        );
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return;
        }
    }
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

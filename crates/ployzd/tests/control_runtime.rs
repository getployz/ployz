//! Control runtime against a secured (TLS + NKey-authorized) NATS server:
//! bootstrap, operation API serving, deploy submit/commit, routed deploys
//! through the gateway, and drift refusal.
//!
//! Machine-add credential minting has its own suite in
//! `machine_add_mint.rs`; the shared fixture lives in `support::control`.

use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use ployz_core::deploy::{DeployRequest, DeployRoute, ImageReference, ReplicaCount};
use ployz_core::install::MachineBootstrapUrl;
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, OperationStatus, RouteTarget,
};
use ployz_core::security::NatsPrincipal;
use ployz_core::state::{
    ActiveMachineState, ActiveServiceCommitRequest, ExpectedActiveService, GatewayServingStatus,
    GatewayStatusObservation, NodePublicIpObservation,
};
use ployz_nats::connect::connect_authenticated;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_sdk_types::{
    DeploySubmitRequest, MachineAddGateway, MachineAddRequest, MachineInspectRequest,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineListRequest, ServiceInspectRequest,
    ServiceListRequest,
};
use ployz_test_support::ops::wait_for_terminal_status;
use ployzd::controllers::MachineAddBootstrapConfig;
use ployzd::gateway_process_runtime::start_gateway_process_runtime_with_client;
use ployzd::node_service_runtime::start_node_runtime_service;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use support::node::{ObservingContainerRunner, ReadyWireGuardEbpf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

mod support;

use ployz_test_support::ids::{
    event_sequence, idempotency_key, node_id, operation_id, revision_id, route_hostname,
    route_port, service_id,
};
use support::control::{TestNats, machine_join_template, redeem_when_ready};

#[tokio::test]
async fn control_runtime_bootstraps_nats_and_serves_operation_api() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    let accepted = api
        .deploy_submit(&DeploySubmitRequest {
            operation_id: operation_id("op_control_runtime"),
            target: deploy_target("svc_api"),
            idempotency_key: idempotency_key("idem_control_runtime"),
        })
        .await
        .expect("operation API accepts deploy");

    assert_eq!(accepted.operation_id, operation_id("op_control_runtime"));
    assert_eq!(accepted.start_sequence, event_sequence(1));
    nats.connected
        .jetstream
        .get_key_value("KV_CORE")
        .await
        .expect("control runtime created KV_CORE");
    nats.connected
        .jetstream
        .get_key_value("KV_OPS")
        .await
        .expect("control runtime created KV_OPS");
    nats.connected
        .jetstream
        .get_stream("PLZ_OPS")
        .await
        .expect("control runtime created PLZ_OPS");
    nats.connected
        .jetstream
        .get_object_store("PLZ_BUNDLES")
        .await
        .expect("control runtime created PLZ_BUNDLES");

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_does_not_mutate_machine_state_on_startup() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");

    assert!(
        core_state
            .active_machines()
            .await
            .expect("active machines read")
            .is_empty()
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_uses_configured_machine_bootstrap_url() {
    let nats = TestNats::start().await;
    let config = nats.control_config().with_machine_bootstrap(
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new("https://example.test/ployz.sh")
                .expect("valid bootstrap url"),
        )
        .with_join_template(machine_join_template(&nats)),
    );
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    let accepted = api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id("op_machine"),
            idempotency_key: idempotency_key("idem_machine"),
            node_id: node_id("node_2"),
            name: ployz_sdk_types::MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: MachineAddGateway::Skip,
        })
        .await
        .expect("machine add succeeds");

    assert_eq!(
        accepted.bootstrap_url.as_str(),
        "https://example.test/ployz.sh"
    );
    let retry = api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id("op_machine_retry"),
            idempotency_key: idempotency_key("idem_machine"),
            node_id: node_id("node_2"),
            name: ployz_sdk_types::MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: MachineAddGateway::Skip,
        })
        .await
        .expect("machine add retry succeeds");
    assert_eq!(retry.accepted.operation_id, operation_id("op_machine"));
    assert_eq!(retry.join_token, accepted.join_token);

    let redeemed = redeem_when_ready(&api, &accepted.join_token).await;
    assert_eq!(redeemed.node_id, node_id("node_2"));

    api.machine_join_report(&MachineJoinReportRequest {
        join_token: accepted.join_token.clone(),
        outcome: MachineJoinReportOutcome::Completed,
    })
    .await
    .expect("join completion reports");
    // The minted per-machine seed is a working Node credential: connect
    // with it and publish this machine's observations.
    let minted_seed = ployz_core::nats_config::NatsUserSeed::try_new(
        redeemed.secret_delivery.nats_credentials.secret(),
    )
    .expect("minted material is a valid seed");
    let minted_client = connect_authenticated(
        &nats.server().config_with_seed(
            NatsPrincipal::Node {
                node_id: node_id("node_2"),
            },
            minted_seed,
        ),
        ployz_test_support::nats::TEST_NATS_CONNECT_TIMEOUT,
    )
    .await
    .expect("minted node credential connects");
    let node_jetstream = jetstream::new(minted_client);
    let observations = AsyncNatsObservationStore::from_jetstream(&node_jetstream)
        .await
        .expect("open observations");
    observations
        .replace_node_public_ip(&NodePublicIpObservation {
            node_id: node_id("node_2"),
            public_ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2)),
        })
        .await
        .expect("public ip stores");
    observations
        .replace_gateway_status(&GatewayStatusObservation {
            node_id: node_id("node_2"),
            listen_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            serving: GatewayServingStatus::Current,
            route_count: 0,
        })
        .await
        .expect("gateway status stores");

    let inspected = api
        .machine_inspect(&MachineInspectRequest {
            node_id: node_id("node_2"),
        })
        .await
        .expect("machine inspects");
    assert_eq!(inspected.active.node_id, node_id("node_2"));
    assert_eq!(inspected.active.name.as_str(), "edge_2");
    assert_eq!(
        inspected
            .public_ip
            .as_ref()
            .expect("public ip exists")
            .public_ip,
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2))
    );
    assert_eq!(
        inspected
            .gateway
            .as_ref()
            .expect("gateway status exists")
            .serving,
        GatewayServingStatus::Current
    );
    let machines = api
        .machine_list(&MachineListRequest {})
        .await
        .expect("machines list")
        .machines;
    assert_eq!(machines, vec![inspected]);

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_serves_active_service_queries() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");
    core_state
        .commit_active_service(&ActiveServiceCommitRequest {
            service_id: service_id("svc_api"),
            expected_current: ExpectedActiveService::Absent,
            target_revision: revision_id("rev_2"),
        })
        .await
        .expect("service state stores");
    let api = nats.api();

    let listed = api
        .service_list(&ServiceListRequest {})
        .await
        .expect("services list");
    let [service] = listed.services.as_slice() else {
        panic!("expected one listed service, got {:?}", listed.services);
    };
    assert_eq!(service.active.service_id, service_id("svc_api"));
    assert_eq!(service.active.active_revision, revision_id("rev_2"));

    let inspected = api
        .service_inspect(&ServiceInspectRequest {
            service_id: service_id("svc_api"),
        })
        .await
        .expect("service inspects");
    assert_eq!(inspected.active.service_id, service_id("svc_api"));
    assert_eq!(inspected.active.active_revision, revision_id("rev_2"));

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_refuses_machine_add_without_join_template() {
    let nats = TestNats::start().await;
    let result = ployzd::control_runtime::start_control_runtime_with_client_and_reload(
        nats.connected.controller.clone(),
        &nats.control_config_without_join_template(),
        nats.reload_runner(),
    )
    .await;

    assert!(matches!(
        result,
        Err(ployzd::control_runtime::ControlRuntimeError::MissingMachineJoinTemplate)
    ));
}

#[tokio::test]
async fn control_runtime_runs_deploy_submit_and_commits_active_state() {
    let nats = TestNats::start_with_nodes(&[node_id("node_a")]).await;
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect_err("control has not bootstrapped observations yet");
    assert!(matches!(
        observations,
        ployz_nats::observations::ObservationStoreError::OpenBucket { .. }
    ));

    let config = nats
        .control_config()
        .with_deploy_nodes(vec![node_id("node_a")])
        .with_deploy_step_timeout(Duration::from_secs(2));
    let runtime = nats.start_control(&config).await;
    let node_client = nats.node_client(&node_id("node_a")).await;
    let node_jetstream = jetstream::new(node_client.clone());
    let observations = AsyncNatsObservationStore::from_jetstream(&node_jetstream)
        .await
        .expect("open observations");
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");
    core_state
        .replace_active_machine(&active_machine("node_a"))
        .await
        .expect("active machine stores");
    let node_runtime = start_node_runtime_service(
        node_client.clone(),
        node_id("node_a"),
        ObservingContainerRunner::new(node_id("node_a"), observations.clone()),
        ReadyWireGuardEbpf,
        ObservingContainerRunner::new(node_id("node_a"), observations.clone()),
    )
    .await
    .expect("node runtime starts");
    let api = nats.api();
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_run"),
        target: deploy_target("svc_api"),
        idempotency_key: idempotency_key("idem_run"),
    };

    let accepted = api
        .deploy_submit(&request)
        .await
        .expect("operation API accepts deploy");

    assert_eq!(accepted.operation_id, operation_id("op_run"));
    let status =
        wait_for_terminal_status(&api, &operation_id("op_run"), Duration::from_secs(4)).await;
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
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");
    assert_eq!(
        core_state
            .active_service(&service_id("svc_api"))
            .await
            .expect("active service reads")
            .expect("active service committed")
            .active_revision,
        revision_id("rev_2")
    );
    let duplicate = api
        .deploy_submit(&request)
        .await
        .expect("duplicate operation API submit returns original operation");
    assert_eq!(duplicate.operation_id, operation_id("op_run"));
    tokio::time::sleep(Duration::from_millis(200)).await;
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
    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_routed_deploy_serves_through_gateway() {
    let nats = TestNats::start_with_nodes(&[node_id("node_a"), node_id("node_gateway")]).await;
    let config = nats
        .control_config()
        .with_deploy_nodes(vec![node_id("node_a")])
        .with_deploy_step_timeout(Duration::from_secs(2));
    let runtime = nats.start_control(&config).await;
    let node_client = nats.node_client(&node_id("node_a")).await;
    let node_jetstream = jetstream::new(node_client.clone());
    let observations = AsyncNatsObservationStore::from_jetstream(&node_jetstream)
        .await
        .expect("open observations");
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");
    core_state
        .replace_active_machine(&active_machine("node_a"))
        .await
        .expect("active machine stores");
    observations
        .replace_node_public_ip(&NodePublicIpObservation {
            node_id: node_id("node_a"),
            public_ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
        })
        .await
        .expect("node public ip stores");
    let node_runtime = start_node_runtime_service(
        node_client.clone(),
        node_id("node_a"),
        ObservingContainerRunner::new(node_id("node_a"), observations.clone()),
        ReadyWireGuardEbpf,
        ObservingContainerRunner::new(node_id("node_a"), observations.clone()),
    )
    .await
    .expect("node runtime starts");
    let gateway_client = nats.node_client(&node_id("node_gateway")).await;
    let gateway = start_gateway_process_runtime_with_client(
        gateway_client,
        Duration::from_millis(10),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        node_id("node_gateway"),
    )
    .await
    .expect("gateway runtime starts");
    let upstream = support::TestHttpUpstream::start("smoke").await;
    let api = nats.api();

    let accepted = api
        .deploy_submit(&DeploySubmitRequest {
            operation_id: operation_id("op_routed"),
            target: deploy_target_with_route(
                "svc_api",
                gateway.listen_addr().port(),
                upstream.port(),
            ),
            idempotency_key: idempotency_key("idem_routed"),
        })
        .await
        .expect("operation API accepts routed deploy");

    assert_eq!(accepted.operation_id, operation_id("op_routed"));
    let status =
        wait_for_terminal_status(&api, &operation_id("op_routed"), Duration::from_secs(4)).await;
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
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ready = gateway.served_projection().is_some_and(|projection| {
            matches!(
                projection.routes.as_slice(),
                [route] if !route.upstreams.is_empty()
            )
        });
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "gateway never served the routed projection; health: {:?}",
            gateway.health()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut client = TcpStream::connect(gateway.listen_addr())
        .await
        .expect("connect gateway");
    client
        .write_all(b"GET /smoke HTTP/1.1\r\nHost: api.example.com\r\n\r\n")
        .await
        .expect("write gateway request");
    client.shutdown().await.expect("finish gateway request");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read gateway response");

    assert_eq!(
        response,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    assert_eq!(
        upstream.request().await,
        "GET /smoke HTTP/1.1\r\nHost: api.example.com\r\n\r\n"
    );

    gateway.shutdown().await;
    node_runtime
        .shutdown()
        .await
        .expect("node runtime shuts down");
    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_refuses_bootstrap_resource_drift() {
    let nats = TestNats::start().await;
    nats.connected
        .jetstream
        .create_stream(jetstream::stream::Config {
            name: "PLZ_OPS".to_owned(),
            subjects: vec!["wrong.>".to_owned()],
            storage: StorageType::File,
            ..Default::default()
        })
        .await
        .expect("create drifted PLZ_OPS stream");

    let config = nats.control_config();
    let error = match ployzd::control_runtime::start_control_runtime_with_client_and_reload(
        nats.connected.controller.clone(),
        &config,
        nats.reload_runner(),
    )
    .await
    {
        Ok(runtime) => {
            runtime.shutdown().await.expect("unexpected runtime stops");
            panic!("control runtime should refuse drift");
        }
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ployzd::control_runtime::ControlRuntimeError::AssureBootstrap(
            ployz_nats::bootstrap::BootstrapAssuranceError::RefuseResource { .. }
        )
    ));
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

fn active_machine(value: &str) -> ActiveMachineState {
    ActiveMachineState {
        node_id: node_id(value),
        name: ployz_sdk_types::MachineName::try_new(value).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
    }
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
    gateway_port: u16,
    endpoint_port: u16,
) -> DeployRequest {
    DeployRequest {
        route: Some(DeployRoute {
            target: RouteTarget::new(route_hostname("api.example.com"), route_port(gateway_port)),
            endpoint_port: route_port(endpoint_port),
        }),
        ..deploy_target(service_id)
    }
}

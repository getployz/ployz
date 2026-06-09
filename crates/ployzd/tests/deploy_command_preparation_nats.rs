use async_nats::jetstream;
use ployz_core::dataplane::{DEFAULT_WIREGUARD_LISTEN_PORT, WireGuardPeerEndpoint};
use ployz_core::deploy::{
    DeployCleanupContainer, DeployRequest, DeployRoute, ImageReference, ReplicaCount,
};
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::machine::MachineName;
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_core::state::{
    ActiveMachineState, ActiveServiceCommitRequest, ActiveServiceState, ActiveServiceStateKey,
    ExpectedActiveService, NodePublicIpObservation,
};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::observations::{AsyncNatsObservationStore, KV_OBS_BUCKET};
use ployzd::deploy_worker::{
    ActiveServiceReadFailure, DeployExecutionNodeScope, DeployFactLoadError,
    load_deploy_execution_facts_from_nats, prepare_deploy_execution_command,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

#[tokio::test]
async fn nats_preparation_loads_active_state_and_observed_target_replicas() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");

    core_state
        .commit_active_service(&ActiveServiceCommitRequest {
            service_id: service_id("svc_api"),
            expected_current: ExpectedActiveService::Absent,
            target_revision: revision_id("rev_1"),
        })
        .await
        .expect("active service stores");
    observations
        .replace_node_containers(&node_snapshot(
            "node_a",
            [managed_observation(
                "node_a",
                "ctr_target",
                "svc_api",
                "rev_2",
                ContainerRuntimeState::running_unroutable(),
            )],
        ))
        .await
        .expect("node_a observations store");
    observations
        .replace_node_containers(&node_snapshot(
            "node_b",
            [
                managed_observation(
                    "node_b",
                    "ctr_old",
                    "svc_api",
                    "rev_1",
                    ContainerRuntimeState::running_unroutable(),
                ),
                managed_observation(
                    "node_b",
                    "ctr_stopped",
                    "svc_api",
                    "rev_2",
                    ContainerRuntimeState::Exited,
                ),
            ],
        ))
        .await
        .expect("node_b observations store");
    observations
        .replace_node_public_ip(&node_public_ip("node_a", 1))
        .await
        .expect("node_a public ip stores");
    observations
        .replace_node_public_ip(&node_public_ip("node_b", 2))
        .await
        .expect("node_b public ip stores");

    let command = prepare_command_from_nats(
        operation_id("op_123"),
        deploy_request(),
        DeployExecutionNodeScope::same_nodes(vec![
            node_id("node_a"),
            node_id("node_b"),
            node_id("node_missing"),
        ]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(
        command.expected_active(),
        &ExpectedActiveService::Revision(revision_id("rev_1"))
    );
    assert_eq!(
        command.existing_replicas(),
        [ployz_core::deploy::ExistingServiceReplica {
            node_id: node_id("node_a"),
            container_id: container_id("ctr_target")
        }]
    );
    assert_eq!(
        command.cleanup_candidates(),
        [
            cleanup_container("node_a", "ctr_target", "rev_2"),
            cleanup_container("node_b", "ctr_old", "rev_1"),
        ]
    );
    assert_eq!(
        command.eligible_nodes(),
        [
            node_id("node_a"),
            node_id("node_b"),
            node_id("node_missing")
        ]
    );
    assert_eq!(command.step_timeout(), Duration::from_secs(7));
    assert_eq!(
        command.wireguard_peer_endpoints(),
        [
            wireguard_peer_endpoint("node_a", 1),
            wireguard_peer_endpoint("node_b", 2)
        ]
    );
}

#[tokio::test]
async fn nats_preparation_uses_active_machines_as_deploy_scope() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    core_state
        .replace_active_machine(&active_machine("edge_2"))
        .await
        .expect("active edge stores");
    observations
        .replace_node_containers(&node_snapshot(
            "edge_2",
            [managed_observation(
                "edge_2",
                "ctr_target",
                "svc_api",
                "rev_2",
                ContainerRuntimeState::running_unroutable(),
            )],
        ))
        .await
        .expect("edge observations store");
    observations
        .replace_node_public_ip(&node_public_ip("edge_2", 7))
        .await
        .expect("edge public ip stores");

    let command = prepare_command_from_nats(
        operation_id("op_123"),
        deploy_request(),
        DeployExecutionNodeScope::same_nodes(vec![node_id("core_1")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(command.eligible_nodes(), [node_id("edge_2")]);
    assert_eq!(
        command.existing_replicas(),
        [ployz_core::deploy::ExistingServiceReplica {
            node_id: node_id("edge_2"),
            container_id: container_id("ctr_target")
        }]
    );
    assert_eq!(
        command.wireguard_peer_endpoints(),
        [wireguard_peer_endpoint("edge_2", 7)]
    );
}

#[tokio::test]
async fn routed_nats_preparation_uses_configured_node_as_dataplane_fallback() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    core_state
        .replace_active_machine(&active_machine("edge_2"))
        .await
        .expect("active edge stores");
    observations
        .replace_node_public_ip(&node_public_ip("core_1", 1))
        .await
        .expect("core public ip stores");
    observations
        .replace_node_public_ip(&node_public_ip("edge_2", 2))
        .await
        .expect("edge public ip stores");

    let command = prepare_command_from_nats(
        operation_id("op_123"),
        routed_deploy_request(),
        DeployExecutionNodeScope::same_nodes(vec![node_id("core_1")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(command.eligible_nodes(), [node_id("edge_2")]);
    assert_eq!(command.dataplane_nodes(), [node_id("core_1")]);
    assert_eq!(
        command.wireguard_peer_endpoints(),
        [
            wireguard_peer_endpoint("core_1", 1),
            wireguard_peer_endpoint("edge_2", 2),
        ]
    );
}

#[tokio::test]
async fn routed_nats_preparation_fails_when_dataplane_public_ip_is_missing() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    core_state
        .replace_active_machine(&active_machine("edge_2"))
        .await
        .expect("active edge stores");
    observations
        .replace_node_public_ip(&node_public_ip("edge_2", 2))
        .await
        .expect("edge public ip stores");

    let error = load_deploy_execution_facts_from_nats(
        &routed_deploy_request(),
        DeployExecutionNodeScope::same_nodes(vec![node_id("core_1")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await
    .expect_err("missing configured dataplane public ip is rejected");

    assert!(matches!(
        error,
        DeployFactLoadError::MissingNodePublicIpObservation { node_id }
            if node_id == self::node_id("core_1")
    ));
}

#[tokio::test]
async fn nats_preparation_uses_absent_active_state_when_service_is_new() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");

    let command = prepare_command_from_nats(
        operation_id("op_123"),
        deploy_request(),
        DeployExecutionNodeScope::same_nodes(vec![node_id("node_a")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(command.expected_active(), &ExpectedActiveService::Absent);
    assert!(command.existing_replicas().is_empty());
}

#[tokio::test]
async fn nats_preparation_preserves_typed_active_state_read_failure() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let key = ActiveServiceStateKey::from_service_id(&service_id("svc_api"));
    let wrong_service_state = ActiveServiceState {
        service_id: service_id("svc_worker"),
        active_revision: revision_id("rev_1"),
    };
    nats.jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open KV_CORE")
        .put(
            key.as_str(),
            serde_json::to_vec(&wrong_service_state)
                .expect("active service state encodes")
                .into(),
        )
        .await
        .expect("corrupt active state stores");

    let request = deploy_request();
    let error = load_deploy_execution_facts_from_nats(
        &request,
        DeployExecutionNodeScope::same_nodes(vec![node_id("node_a")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await
    .expect_err("wrong active service payload is rejected");

    assert!(matches!(
        error,
        DeployFactLoadError::ActiveServiceRead {
            service_id,
            failure:
                ActiveServiceReadFailure::CorruptState {
                    key: corrupt_key,
                    expected_service_id,
                    actual_service_id,
                },
        } if service_id == self::service_id("svc_api")
            && corrupt_key == key.as_str()
            && expected_service_id == self::service_id("svc_api")
            && actual_service_id == self::service_id("svc_worker")
    ));
}

#[tokio::test]
async fn nats_preparation_preserves_decode_failure_message() {
    let nats = test_nats().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let request = deploy_request();
    let key = ActiveServiceStateKey::from_service_id(&request.service_id);
    nats.jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open KV_CORE")
        .put(key.as_str(), br#"{"service_id":"svc_api""#.to_vec().into())
        .await
        .expect("malformed active state stores");

    let error = load_deploy_execution_facts_from_nats(
        &request,
        DeployExecutionNodeScope::same_nodes(vec![node_id("node_a")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await
    .expect_err("malformed active service payload is rejected");

    assert!(matches!(
        error,
        DeployFactLoadError::ActiveServiceRead {
            failure: ActiveServiceReadFailure::Decode { message },
            ..
        } if !message.is_empty()
    ));
}

async fn prepare_command_from_nats(
    operation_id: OperationId,
    request: DeployRequest,
    node_scope: DeployExecutionNodeScope,
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
    step_timeout: Duration,
) -> ployzd::deploy_worker::DeployExecutionCommand {
    let facts = load_deploy_execution_facts_from_nats(
        &request,
        node_scope,
        core_state,
        observations,
        step_timeout,
    )
    .await
    .expect("deploy facts load from nats");
    prepare_deploy_execution_command(operation_id, request, facts)
        .expect("deploy command prepares from loaded facts")
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
    jetstream
        .create_key_value(jetstream::kv::Config {
            bucket: KV_CORE_BUCKET.to_owned(),
            ..Default::default()
        })
        .await
        .expect("create KV_CORE bucket");
    jetstream
        .create_key_value(jetstream::kv::Config {
            bucket: KV_OBS_BUCKET.to_owned(),
            ..Default::default()
        })
        .await
        .expect("create KV_OBS bucket");

    TestNats {
        _server: server,
        jetstream,
    }
}

fn deploy_request() -> DeployRequest {
    DeployRequest {
        service_id: service_id("svc_api"),
        target_revision: revision_id("rev_2"),
        image: ImageReference::try_new("registry.example/api:rev_2")
            .expect("valid image reference"),
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
        route: None,
    }
}

fn routed_deploy_request() -> DeployRequest {
    DeployRequest {
        route: Some(DeployRoute {
            target: RouteTarget {
                hostname: RouteHostname::try_new("smoke.local").expect("valid route hostname"),
                port: route_port(8080),
            },
            endpoint_port: route_port(80),
        }),
        ..deploy_request()
    }
}

fn node_snapshot(
    node_id: &str,
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> NodeContainerObservationSnapshot {
    NodeContainerObservationSnapshot::try_new(self::node_id(node_id), containers)
        .expect("valid node observation snapshot")
}

fn managed_observation(
    node_id: &str,
    container_id: &str,
    service_id: &str,
    revision_id: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: self::node_id(node_id),
        container_id: self::container_id(container_id),
        service_id: self::service_id(service_id),
        revision_id: self::revision_id(revision_id),
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
        state,
    }
}

fn active_machine(node_id: &str) -> ActiveMachineState {
    ActiveMachineState {
        node_id: self::node_id(node_id),
        name: MachineName::try_new(node_id).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
    }
}

fn node_public_ip(node_id: &str, last_octet: u8) -> NodePublicIpObservation {
    NodePublicIpObservation {
        node_id: self::node_id(node_id),
        public_ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, last_octet)),
    }
}

fn wireguard_peer_endpoint(node_id: &str, last_octet: u8) -> WireGuardPeerEndpoint {
    WireGuardPeerEndpoint::new(
        self::node_id(node_id),
        SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, last_octet)),
            DEFAULT_WIREGUARD_LISTEN_PORT,
        ),
    )
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn route_port(value: u16) -> RoutePort {
    RoutePort::try_new(value).expect("valid route port")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn cleanup_container(
    node_id: &str,
    container_id: &str,
    revision_id: &str,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        node_id: self::node_id(node_id),
        container_id: self::container_id(container_id),
        service_id: service_id("svc_api"),
        revision_id: self::revision_id(revision_id),
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
    }
}

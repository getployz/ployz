use ployz_core::dataplane::{
    DEFAULT_WIREGUARD_LISTEN_PORT, EbpfForwardingReady, EbpfForwardingReadyEvidence,
    WireGuardEbpfComponent, WireGuardEbpfNodeReady, WireGuardEbpfPrepareError,
    WireGuardEbpfPrepareRequest, WireGuardEbpfReady, WireGuardPeerEndpoint, WireGuardPublicKey,
    WireGuardReady, WireGuardReadyEvidence,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, NodeId};
use ployz_core::node::ManagedContainerKind;
use ployz_core::subjects::{NodeServiceEndpoint, node_service};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, start_nats_service};
use ployz_test_support::ids::{
    container_id, failure_message, node_id, operation_id, revision_id, service_id, step_id,
};
use ployzd::deploy_worker::{
    NodeContainerRuntime, NodeContainerRuntimeError, NodeRuntimeUnavailableReason,
    WireGuardEbpfPreparer,
};
use ployzd::docker::labels::ManagedContainerLabels;
use ployzd::node::client::{NatsNodeContainerRuntime, NatsNodeWireGuardEbpfPreparer};
use ployzd::node::protocol::{
    NodeContainerRemoveDomainError, NodeContainerRemoveRpcRequest, NodeContainerRemoveRpcResponse,
    NodeContainerRpcOk, NodeContainerRunDomainError, NodeContainerRunRpcOk,
    NodeContainerRunRpcRequest, NodeContainerRunRpcResponse, NodeContainerRunSpec,
    NodeRunContainerOutcome, NodeWireGuardEbpfPreparePhase, NodeWireGuardEbpfPrepareRpcRequest,
    NodeWireGuardEbpfPrepareRpcResponse,
};
use ployzd::services::node_runtime_service;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn nats_node_runtime_calls_container_run_service() {
    let nats = test_nats().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let _service = start_container_run_service(nats.node_a.clone(), &node_id("node_a"), {
        let received = Arc::clone(&received);
        move |request| {
            received
                .lock()
                .expect("received request lock is not poisoned")
                .push(request);
            NodeRunContainerResult::Ok {
                outcome: NodeRunContainerOutcome::Created {
                    container_id: container_id("ctr_123"),
                },
            }
        }
    })
    .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let outcome = runtime
        .run_container(&node_id("node_a"), run_request())
        .await
        .expect("node container run succeeds");

    assert_eq!(
        outcome,
        NodeRunContainerOutcome::Created {
            container_id: container_id("ctr_123")
        }
    );
    assert_eq!(
        received
            .lock()
            .expect("received request lock is not poisoned")
            .as_slice(),
        [NodeContainerRunRpcRequest {
            image: image("registry.example/api:rev_2"),
            endpoint: None,
            container: managed_container_spec()
        }]
    );
}

#[tokio::test]
async fn nats_node_runtime_maps_missing_responder_to_request_failure() {
    let nats = test_nats().await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(&node_id("node_missing"), run_request())
        .await
        .expect_err("missing node responder fails");

    assert_eq!(
        error,
        NodeContainerRuntimeError::Unavailable {
            node_id: node_id("node_missing"),
            reason: NodeRuntimeUnavailableReason::NoResponders,
        }
    );
}

#[tokio::test]
async fn nats_node_runtime_maps_service_error_headers() {
    let nats = test_nats().await;
    let _service =
        start_container_run_raw_service(nats.node_a.clone(), node_id("node_a"), |_request| {
            NatsServiceResponse::transport_error(
                ployz_nats::service_runtime::NatsServiceError::bad_request("bad container request"),
            )
        })
        .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(&node_id("node_a"), run_request())
        .await
        .expect_err("service error header fails");

    assert_eq!(
        error,
        NodeContainerRuntimeError::Unavailable {
            node_id: node_id("node_a"),
            reason: NodeRuntimeUnavailableReason::ServiceBadRequest {
                message: "bad container request".to_owned(),
            },
        }
    );
}

#[tokio::test]
async fn nats_node_runtime_reports_invalid_response_payload() {
    let nats = test_nats().await;
    let _service =
        start_container_run_raw_service(nats.node_a.clone(), node_id("node_a"), |_request| {
            NatsServiceResponse::ok("not json")
        })
        .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(&node_id("node_a"), run_request())
        .await
        .expect_err("invalid response payload fails");

    assert!(matches!(
        error,
        NodeContainerRuntimeError::Unavailable {
            node_id: actual_node_id,
            reason: NodeRuntimeUnavailableReason::DecodeResponse { .. },
            ..
        } if actual_node_id == node_id("node_a")
    ));
}

#[tokio::test]
async fn nats_node_runtime_preserves_domain_runtime_error() {
    let nats = test_nats().await;
    let conflict = NodeContainerRunDomainError::OperationStepConflict {
        container_id: container_id("ctr_existing"),
        expected: managed_labels(),
        actual: ManagedContainerLabels {
            revision_id: revision_id("rev_other"),
            ..managed_labels()
        },
    };
    let _service = start_container_run_service(nats.node_a.clone(), &node_id("node_a"), {
        let conflict = conflict.clone();
        move |_request| NodeRunContainerResult::DomainError {
            error: conflict.clone(),
        }
    })
    .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(&node_id("node_a"), run_request())
        .await
        .expect_err("domain runtime error fails");

    assert_eq!(
        error,
        NodeContainerRuntimeError::OperationStepConflict {
            node_id: node_id("node_a"),
            container_id: container_id("ctr_existing"),
            expected: managed_labels(),
            actual: ManagedContainerLabels {
                revision_id: revision_id("rev_other"),
                ..managed_labels()
            },
        }
    );
}

#[tokio::test]
async fn nats_node_runtime_rejects_response_for_different_node() {
    let nats = test_nats().await;
    let _service =
        start_container_run_raw_service(nats.node_a.clone(), node_id("node_a"), |_request| {
            NatsServiceResponse::ok(encode_run_response(NodeContainerRunRpcResponse::Ok(
                NodeContainerRunRpcOk {
                    node_id: node_id("node_b"),
                    outcome: NodeRunContainerOutcome::Created {
                        container_id: container_id("ctr_wrong"),
                    },
                },
            )))
        })
        .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(&node_id("node_a"), run_request())
        .await
        .expect_err("wrong-node domain error fails");

    assert_eq!(
        error,
        NodeContainerRuntimeError::Unavailable {
            node_id: node_id("node_a"),
            reason: NodeRuntimeUnavailableReason::WrongResponder {
                actual_node_id: node_id("node_b"),
            },
        }
    );
}

#[tokio::test]
async fn nats_node_runtime_calls_container_remove_service() {
    let nats = test_nats().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let _service = start_container_remove_service(nats.node_a.clone(), &node_id("node_a"), {
        let received = Arc::clone(&received);
        move |request| {
            received
                .lock()
                .expect("received remove request lock is not poisoned")
                .push(request);
            NodeRemoveContainerResult::Ok {
                container_id: container_id("ctr_old"),
            }
        }
    })
    .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    runtime
        .remove_container(&node_id("node_a"), remove_request("ctr_old"))
        .await
        .expect("node container remove succeeds");

    assert_eq!(
        received
            .lock()
            .expect("received remove request lock is not poisoned")
            .as_slice(),
        [NodeContainerRemoveRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_old"),
            expected_identity: managed_labels().identity(),
        }]
    );
}

#[tokio::test]
async fn nats_node_runtime_preserves_remove_domain_error() {
    let nats = test_nats().await;
    let failure = NodeContainerRemoveDomainError::RemoveFailed {
        container_id: container_id("ctr_old"),
        message: failure_message("container remove failed: busy"),
        inspect_hint: inspect_hint("ctr_old"),
    };
    let _service = start_container_remove_service(nats.node_a.clone(), &node_id("node_a"), {
        let failure = failure.clone();
        move |_request| NodeRemoveContainerResult::DomainError {
            error: failure.clone(),
        }
    })
    .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .remove_container(&node_id("node_a"), remove_request("ctr_old"))
        .await
        .expect_err("remove domain error is preserved");

    assert_eq!(
        error,
        NodeContainerRuntimeError::RemoveContainerFailed {
            node_id: node_id("node_a"),
            container_id: container_id("ctr_old"),
            message: failure_message("container remove failed: busy"),
            inspect_hint: inspect_hint("ctr_old"),
        }
    );
}

#[tokio::test]
async fn nats_node_preparer_calls_wireguard_ebpf_prepare_service() {
    let nats = test_nats().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let _service = start_wireguard_ebpf_prepare_service(nats.node_a.clone(), &node_id("node_a"), {
        let received = Arc::clone(&received);
        move |request| {
            received
                .lock()
                .expect("received request lock is not poisoned")
                .push(request);
            WireGuardEbpfPrepareResult::Ok
        }
    })
    .await;
    let mut preparer = NatsNodeWireGuardEbpfPreparer::new(nats.client);

    let report = preparer
        .prepare_wireguard_ebpf(wireguard_ebpf_request(&["node_a"]))
        .await
        .expect("wireguard ebpf prepare succeeds");
    assert_eq!(report.nodes, vec![ready_node("node_a")]);

    assert_eq!(
        received
            .lock()
            .expect("received request lock is not poisoned")
            .as_slice(),
        [NodeWireGuardEbpfPrepareRpcRequest {
            phase: NodeWireGuardEbpfPreparePhase::PrepareDataplane,
            operation_id: operation_id("op_123"),
            nodes: vec![node_id("node_a")],
            endpoint_routes: endpoint_routes(&["node_a"]),
            peer_endpoints: peer_endpoints(&["node_a"]),
            peers: Vec::new(),
        }]
    );
}

#[tokio::test]
async fn nats_node_preparer_bootstraps_public_keys_then_programs_peers() {
    let nats = test_nats().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let mut services = Vec::new();
    for (node, node_client) in [
        ("node_a", nats.node_a.clone()),
        ("node_b", nats.node_b.clone()),
    ] {
        let received = Arc::clone(&received);
        services.push(
            start_wireguard_ebpf_prepare_service(node_client, &node_id(node), move |request| {
                received
                    .lock()
                    .expect("received request lock is not poisoned")
                    .push(request);
                WireGuardEbpfPrepareResult::Ok
            })
            .await,
        );
    }
    let mut preparer = NatsNodeWireGuardEbpfPreparer::new(nats.client);

    let report = preparer
        .prepare_wireguard_ebpf(wireguard_ebpf_request(&["node_a", "node_b"]))
        .await
        .expect("wireguard ebpf prepare succeeds");

    assert_eq!(
        report.nodes,
        vec![ready_node("node_a"), ready_node("node_b")]
    );
    let received = received
        .lock()
        .expect("received request lock is not poisoned")
        .clone();
    let [first_read, second_read, first_prepare, second_prepare] = received.as_slice() else {
        panic!("expected two read requests and two prepare requests");
    };
    assert_eq!(
        first_read.phase,
        NodeWireGuardEbpfPreparePhase::ReadPublicKey
    );
    assert_eq!(
        second_read.phase,
        NodeWireGuardEbpfPreparePhase::ReadPublicKey
    );
    assert_eq!(
        first_prepare.phase,
        NodeWireGuardEbpfPreparePhase::PrepareDataplane
    );
    assert_eq!(
        second_prepare.phase,
        NodeWireGuardEbpfPreparePhase::PrepareDataplane
    );
    assert!(first_read.peers.is_empty());
    assert!(second_read.peers.is_empty());
    assert_eq!(
        first_prepare.peers,
        vec![
            ployz_core::dataplane::WireGuardPeer::from_endpoint(
                peer_endpoint("node_a", 1),
                wireguard_public_key("public-node_a"),
            ),
            ployz_core::dataplane::WireGuardPeer::from_endpoint(
                peer_endpoint("node_b", 2),
                wireguard_public_key("public-node_b"),
            ),
        ]
    );
    assert_eq!(second_prepare.peers, first_prepare.peers);
}

#[tokio::test]
async fn nats_node_preparer_maps_missing_responder_to_wireguard_unavailable() {
    let nats = test_nats().await;
    let mut preparer = NatsNodeWireGuardEbpfPreparer::new(nats.client);

    let error = preparer
        .prepare_wireguard_ebpf(wireguard_ebpf_request(&["node_missing"]))
        .await
        .expect_err("missing node responder fails");

    assert_eq!(
        error,
        WireGuardEbpfPrepareError::Unavailable {
            node_id: node_id("node_missing"),
            component: WireGuardEbpfComponent::WireGuard,
            message: failure_message("node runtime has no responders"),
        }
    );
}

async fn start_container_run_service(
    client: async_nats::Client,
    node_id: &NodeId,
    handler: impl Fn(NodeContainerRunRpcRequest) -> NodeRunContainerResult + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let node_id = node_id.clone();
    start_container_run_raw_service(client, node_id.clone(), move |request| {
        let response = decode_run_request(request)
            .map(&handler)
            .map(|result| result.into_nats_response(node_id.clone()));
        match response {
            Ok(response) => response,
            Err(message) => NatsServiceResponse::transport_error(
                ployz_nats::service_runtime::NatsServiceError::bad_request(message),
            ),
        }
    })
    .await
}

async fn start_container_run_raw_service(
    client: async_nats::Client,
    node_id: NodeId,
    handler: impl Fn(NatsServiceRequest) -> NatsServiceResponse + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let spec = node_runtime_service(&node_id);
    let endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject == node_service(&node_id, NodeServiceEndpoint::ContainerRun)
        })
        .expect("container.run endpoint exists")
        .clone();
    let mut service = start_nats_service(client.clone(), &spec)
        .await
        .expect("start node service");
    service
        .bind_endpoint(&endpoint, move |request| {
            let response = handler(request);
            async move { response }
        })
        .await
        .expect("bind container.run endpoint");
    // Round-trip so the server has registered the subscription before a
    // request arrives from another (authenticated) connection.
    client.flush().await.expect("flush service subscription");
    service
}

async fn start_container_remove_service(
    client: async_nats::Client,
    node_id: &NodeId,
    handler: impl Fn(NodeContainerRemoveRpcRequest) -> NodeRemoveContainerResult + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let node_id = node_id.clone();
    start_container_remove_raw_service(client, node_id.clone(), move |request| {
        let response = decode_remove_request(request)
            .map(&handler)
            .map(|result| result.into_nats_response(node_id.clone()));
        match response {
            Ok(response) => response,
            Err(message) => NatsServiceResponse::transport_error(
                ployz_nats::service_runtime::NatsServiceError::bad_request(message),
            ),
        }
    })
    .await
}

async fn start_container_remove_raw_service(
    client: async_nats::Client,
    node_id: NodeId,
    handler: impl Fn(NatsServiceRequest) -> NatsServiceResponse + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let spec = node_runtime_service(&node_id);
    let endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject == node_service(&node_id, NodeServiceEndpoint::ContainerRemove)
        })
        .expect("container.remove endpoint exists")
        .clone();
    let mut service = start_nats_service(client.clone(), &spec)
        .await
        .expect("start node service");
    service
        .bind_endpoint(&endpoint, move |request| {
            let response = handler(request);
            async move { response }
        })
        .await
        .expect("bind container.remove endpoint");
    client.flush().await.expect("flush service subscription");
    service
}

async fn start_wireguard_ebpf_prepare_service(
    client: async_nats::Client,
    node_id: &NodeId,
    handler: impl Fn(NodeWireGuardEbpfPrepareRpcRequest) -> WireGuardEbpfPrepareResult
    + Send
    + Sync
    + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let node_id = node_id.clone();
    start_wireguard_ebpf_prepare_raw_service(client, node_id.clone(), move |request| {
        let response = decode_wireguard_ebpf_request(request).map(|request| {
            let phase = request.phase;
            handler(request).into_nats_response(node_id.clone(), phase)
        });
        match response {
            Ok(response) => response,
            Err(message) => NatsServiceResponse::transport_error(
                ployz_nats::service_runtime::NatsServiceError::bad_request(message),
            ),
        }
    })
    .await
}

async fn start_wireguard_ebpf_prepare_raw_service(
    client: async_nats::Client,
    node_id: NodeId,
    handler: impl Fn(NatsServiceRequest) -> NatsServiceResponse + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let spec = node_runtime_service(&node_id);
    let endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject == node_service(&node_id, NodeServiceEndpoint::WireGuardEbpfPrepare)
        })
        .expect("wireguard_ebpf.prepare endpoint exists")
        .clone();
    let mut service = start_nats_service(client.clone(), &spec)
        .await
        .expect("start node service");
    service
        .bind_endpoint(&endpoint, move |request| {
            let response = handler(request);
            async move { response }
        })
        .await
        .expect("bind wireguard_ebpf.prepare endpoint");
    client.flush().await.expect("flush service subscription");
    service
}

fn decode_run_request(request: NatsServiceRequest) -> Result<NodeContainerRunRpcRequest, String> {
    serde_json::from_slice(&request.payload).map_err(|error| error.to_string())
}

fn encode_run_response(response: NodeContainerRunRpcResponse) -> Vec<u8> {
    serde_json::to_vec(&response).expect("node run response encodes")
}

fn decode_remove_request(
    request: NatsServiceRequest,
) -> Result<NodeContainerRemoveRpcRequest, String> {
    serde_json::from_slice(&request.payload).map_err(|error| error.to_string())
}

fn encode_remove_response(response: NodeContainerRemoveRpcResponse) -> Vec<u8> {
    serde_json::to_vec(&response).expect("node remove response encodes")
}

fn decode_wireguard_ebpf_request(
    request: NatsServiceRequest,
) -> Result<NodeWireGuardEbpfPrepareRpcRequest, String> {
    serde_json::from_slice(&request.payload).map_err(|error| error.to_string())
}

fn encode_wireguard_ebpf_response(response: NodeWireGuardEbpfPrepareRpcResponse) -> Vec<u8> {
    serde_json::to_vec(&response).expect("node wireguard ebpf response encodes")
}

enum NodeRunContainerResult {
    Ok { outcome: NodeRunContainerOutcome },
    DomainError { error: NodeContainerRunDomainError },
}

impl NodeRunContainerResult {
    fn into_nats_response(self, node_id: NodeId) -> NatsServiceResponse {
        match self {
            Self::Ok { outcome } => NatsServiceResponse::ok(encode_run_response(
                NodeContainerRunRpcResponse::Ok(NodeContainerRunRpcOk { node_id, outcome }),
            )),
            Self::DomainError { error } => NatsServiceResponse::domain_error(encode_run_response(
                NodeContainerRunRpcResponse::DomainError { node_id, error },
            )),
        }
    }
}

enum NodeRemoveContainerResult {
    Ok {
        container_id: ContainerId,
    },
    DomainError {
        error: NodeContainerRemoveDomainError,
    },
}

impl NodeRemoveContainerResult {
    fn into_nats_response(self, node_id: NodeId) -> NatsServiceResponse {
        match self {
            Self::Ok { container_id } => NatsServiceResponse::ok(encode_remove_response(
                NodeContainerRemoveRpcResponse::Ok(NodeContainerRpcOk {
                    node_id,
                    container_id,
                }),
            )),
            Self::DomainError { error } => {
                NatsServiceResponse::domain_error(encode_remove_response(
                    NodeContainerRemoveRpcResponse::DomainError { node_id, error },
                ))
            }
        }
    }
}

enum WireGuardEbpfPrepareResult {
    Ok,
}

impl WireGuardEbpfPrepareResult {
    fn into_nats_response(
        self,
        node_id: NodeId,
        phase: NodeWireGuardEbpfPreparePhase,
    ) -> NatsServiceResponse {
        match (self, phase) {
            (Self::Ok, NodeWireGuardEbpfPreparePhase::ReadPublicKey) => NatsServiceResponse::ok(
                encode_wireguard_ebpf_response(NodeWireGuardEbpfPrepareRpcResponse::PublicKey {
                    public_key: wireguard_public_key(format!("public-{}", node_id.as_str())),
                    node_id,
                }),
            ),
            (Self::Ok, NodeWireGuardEbpfPreparePhase::PrepareDataplane) => NatsServiceResponse::ok(
                encode_wireguard_ebpf_response(NodeWireGuardEbpfPrepareRpcResponse::Ok {
                    readiness: ready_node_for_id(node_id),
                }),
            ),
        }
    }
}

fn ready_node(node_id: &str) -> WireGuardEbpfNodeReady {
    ready_node_for_id(self::node_id(node_id))
}

fn ready_node_for_id(node_id: NodeId) -> WireGuardEbpfNodeReady {
    let public_key = wireguard_public_key(format!("public-{}", node_id.as_str()));
    WireGuardEbpfNodeReady {
        node_id,
        ready: WireGuardEbpfReady {
            wireguard: WireGuardReady {
                public_key,
                evidence: vec![WireGuardReadyEvidence::Command {
                    program: "wg".to_owned(),
                    args: vec!["--version".to_owned()],
                }],
            },
            ebpf_forwarding: EbpfForwardingReady {
                evidence: vec![EbpfForwardingReadyEvidence::PloyzTcBytecode {
                    path: "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".to_owned(),
                    symbols: vec!["ployz_egress".to_owned(), "ployz_ingress".to_owned()],
                }],
            },
        },
    }
}

fn remove_request(container_id: &str) -> NodeContainerRemoveRpcRequest {
    NodeContainerRemoveRpcRequest {
        operation_id: operation_id("op_123"),
        container_id: self::container_id(container_id),
        expected_identity: managed_labels().identity(),
    }
}

fn wireguard_ebpf_request(nodes: &[&str]) -> WireGuardEbpfPrepareRequest {
    WireGuardEbpfPrepareRequest {
        operation_id: operation_id("op_123"),
        nodes: nodes.iter().map(|node| node_id(node)).collect(),
        endpoint_routes: endpoint_routes(nodes),
        peer_endpoints: peer_endpoints(nodes),
        peers: Vec::new(),
    }
}

fn peer_endpoints(nodes: &[&str]) -> Vec<WireGuardPeerEndpoint> {
    nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            WireGuardPeerEndpoint::new(
                node_id(node),
                SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(203, 0, 113, (index + 1) as u8)),
                    DEFAULT_WIREGUARD_LISTEN_PORT,
                ),
            )
        })
        .collect()
}

fn peer_endpoint(node: &str, last_octet: u8) -> WireGuardPeerEndpoint {
    WireGuardPeerEndpoint::new(
        node_id(node),
        SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, last_octet)),
            DEFAULT_WIREGUARD_LISTEN_PORT,
        ),
    )
}

fn wireguard_public_key(value: impl Into<String>) -> WireGuardPublicKey {
    WireGuardPublicKey::try_new(value).expect("valid wireguard public key")
}

fn endpoint_routes(nodes: &[&str]) -> Vec<ployz_core::dataplane::WireGuardEbpfEndpointRoute> {
    nodes
        .iter()
        .map(|node| {
            ployz_core::dataplane::WireGuardEbpfEndpointRoute::default_for_node(&node_id(node))
        })
        .collect()
}

fn inspect_hint(container_id: &str) -> ployz_core::ops::OperatorHint {
    ployz_core::ops::OperatorHint::try_new(format!("ployz container inspect {container_id}"))
        .expect("valid inspect hint")
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    /// Controller principal: the requesting deploy-worker side.
    client: async_nats::Client,
    /// Node principals: the node-service stub side.
    node_a: async_nats::Client,
    node_b: async_nats::Client,
}

async fn test_nats() -> TestNats {
    let nats = ployz_test_support::nats::TestNats::start_with_nodes(&[
        node_id("node_a"),
        node_id("node_b"),
    ])
    .await;
    let client = nats.controller.clone();
    let node_a = nats.node_client(&node_id("node_a")).await;
    let node_b = nats.node_client(&node_id("node_b")).await;

    TestNats {
        _nats: nats,
        client,
        node_a,
        node_b,
    }
}

fn run_request() -> NodeContainerRunRpcRequest {
    NodeContainerRunRpcRequest {
        image: image("registry.example/api:rev_2"),
        endpoint: None,
        container: managed_container_spec(),
    }
}

fn managed_container_spec() -> NodeContainerRunSpec {
    NodeContainerRunSpec {
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_2"),
        operation_id: operation_id("op_123"),
        step_id: step_id("run_1"),
        kind: ManagedContainerKind::Service,
    }
}

fn managed_labels() -> ManagedContainerLabels {
    ManagedContainerLabels {
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_2"),
        operation_id: operation_id("op_123"),
        step_id: step_id("run_1"),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
    }
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

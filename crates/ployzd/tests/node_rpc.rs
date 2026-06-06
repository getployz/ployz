use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::ManagedContainerKind;
use ployz_core::subjects::NodeServiceEndpoint;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, start_nats_service};
use ployzd::deploy_worker::{
    NodeContainerRuntime, NodeContainerRuntimeError, NodeRunContainerOutcome,
    NodeRunContainerRequest, NodeRuntimeUnavailableReason,
};
use ployzd::docker::labels::ManagedContainerLabels;
use ployzd::node_rpc::{
    NatsNodeContainerRuntime, NodeContainerRunDomainError, NodeContainerRunRpcRequest,
    NodeContainerRunRpcResponse,
};
use ployzd::services::{node_endpoint_subject, node_runtime_service};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn nats_node_runtime_calls_container_run_service() {
    let nats = test_nats().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let _service = start_container_run_service(nats.client.clone(), &node_id("node_a"), {
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
        .run_container(run_request("node_a"))
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
            labels: managed_labels()
        }]
    );
}

#[tokio::test]
async fn nats_node_runtime_maps_missing_responder_to_request_failure() {
    let nats = test_nats().await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(run_request("node_missing"))
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
        start_container_run_raw_service(nats.client.clone(), node_id("node_a"), |_request| {
            NatsServiceResponse::transport_error(
                ployz_nats::service_runtime::NatsServiceError::bad_request("bad container request"),
            )
        })
        .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(run_request("node_a"))
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
        start_container_run_raw_service(nats.client.clone(), node_id("node_a"), |_request| {
            NatsServiceResponse::ok("not json")
        })
        .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(run_request("node_a"))
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
    let _service = start_container_run_service(nats.client.clone(), &node_id("node_a"), {
        let conflict = conflict.clone();
        move |_request| NodeRunContainerResult::DomainError {
            error: conflict.clone(),
        }
    })
    .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(run_request("node_a"))
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
        start_container_run_raw_service(nats.client.clone(), node_id("node_a"), |_request| {
            NatsServiceResponse::ok(encode_run_response(NodeContainerRunRpcResponse::Ok {
                node_id: node_id("node_b"),
                outcome: NodeRunContainerOutcome::Created {
                    container_id: container_id("ctr_wrong"),
                },
            }))
        })
        .await;
    let mut runtime = NatsNodeContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(run_request("node_a"))
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
            endpoint.subject == node_endpoint_subject(&node_id, NodeServiceEndpoint::ContainerRun)
        })
        .expect("container.run endpoint exists")
        .clone();
    let mut service = start_nats_service(client, &spec)
        .await
        .expect("start node service");
    service
        .bind_endpoint(&endpoint, move |request| {
            let response = handler(request);
            async move { response }
        })
        .await
        .expect("bind container.run endpoint");
    service
}

fn decode_run_request(request: NatsServiceRequest) -> Result<NodeContainerRunRpcRequest, String> {
    serde_json::from_slice(&request.payload).map_err(|error| error.to_string())
}

fn encode_run_response(response: NodeContainerRunRpcResponse) -> Vec<u8> {
    serde_json::to_vec(&response).expect("node run response encodes")
}

enum NodeRunContainerResult {
    Ok { outcome: NodeRunContainerOutcome },
    DomainError { error: NodeContainerRunDomainError },
}

impl NodeRunContainerResult {
    fn into_nats_response(self, node_id: NodeId) -> NatsServiceResponse {
        match self {
            Self::Ok { outcome } => {
                NatsServiceResponse::ok(encode_run_response(NodeContainerRunRpcResponse::Ok {
                    node_id,
                    outcome,
                }))
            }
            Self::DomainError { error } => NatsServiceResponse::domain_error(encode_run_response(
                NodeContainerRunRpcResponse::DomainError { node_id, error },
            )),
        }
    }
}

struct TestNats {
    _server: nats_server::Server,
    client: async_nats::Client,
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

    TestNats {
        _server: server,
        client,
    }
}

fn run_request(node_id: &str) -> NodeRunContainerRequest {
    NodeRunContainerRequest {
        node_id: self::node_id(node_id),
        image: image("registry.example/api:rev_2"),
        labels: managed_labels(),
    }
}

fn managed_labels() -> ManagedContainerLabels {
    ManagedContainerLabels {
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_2"),
        operation_id: operation_id("op_123"),
        step_id: step_id("run_1"),
        kind: ManagedContainerKind::Service,
    }
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

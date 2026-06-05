use ployz_core::ids::{NodeId, OperationId};
use ployz_core::ops::EventSequence;
use ployz_core::subjects::{
    API_DEPLOY_PLAN, API_DEPLOY_SUBMIT, API_MACHINE_ADD, API_OPS_STATUS, API_OPS_WATCH,
    NodeServiceEndpoint,
};
use ployz_nats::services::{EndpointExecution, NatsRequestFailure, ServiceDiscoveryQuery};
use ployz_sdk_types::{OperationDispatch, OpsStatusError};
use ployzd::operation_api::{ops_status_missing, queued_operation};
use ployzd::services::{DaemonServiceCatalog, NodeServiceCallError, node_endpoint_subject};

#[test]
fn service_catalog_supports_srv_ping_discovery() {
    let node_id = node_id("node_7");
    let catalog = DaemonServiceCatalog::for_node(&node_id);

    assert_eq!(ServiceDiscoveryQuery::All.subject(), "$SRV.PING");
    assert_eq!(
        ServiceDiscoveryQuery::Service { name: "plz-api" }.subject(),
        "$SRV.PING.plz-api"
    );

    let pings = catalog.discover(ServiceDiscoveryQuery::All);

    assert_eq!(pings.len(), 2);
    assert!(
        pings
            .iter()
            .any(|ping| ping.name == "plz-api" && ping.id == "plz-api.core")
    );
    assert!(pings.iter().any(|ping| {
        ping.name == "plz-node"
            && ping.id == "plz-node.node_7"
            && ping.metadata.get("node_id") == Some("node_7")
    }));
    assert!(catalog.has_endpoint_subject(API_OPS_STATUS));
    assert!(catalog.has_endpoint_subject(API_OPS_WATCH));
    assert!(!catalog.has_endpoint_subject(API_DEPLOY_PLAN));
    assert!(!catalog.has_endpoint_subject(API_MACHINE_ADD));
    assert!(catalog.has_endpoint_subject("plz.v1.svc.node.node_7.inspect"));
}

#[test]
fn api_service_marks_mutations_as_operation_acceptors() {
    let catalog = DaemonServiceCatalog::for_node(&node_id("node_7"));
    let api = catalog
        .services()
        .iter()
        .find(|service| service.name == "plz-api")
        .expect("api service is registered");
    let deploy_submit = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.subject == API_DEPLOY_SUBMIT)
        .expect("deploy.submit endpoint is registered");

    assert_eq!(deploy_submit.execution, EndpointExecution::AcceptsOperation);
}

#[test]
fn ops_watch_is_a_query_endpoint() {
    let catalog = DaemonServiceCatalog::for_node(&node_id("node_7"));
    let api = catalog
        .services()
        .iter()
        .find(|service| service.name == "plz-api")
        .expect("api service is registered");
    let ops_watch = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.subject == API_OPS_WATCH)
        .expect("ops.watch endpoint is registered");

    assert_eq!(ops_watch.execution, EndpointExecution::Query);
}

#[test]
fn ops_status_is_a_query_endpoint() {
    let catalog = DaemonServiceCatalog::for_node(&node_id("node_7"));
    let api = catalog
        .services()
        .iter()
        .find(|service| service.name == "plz-api")
        .expect("api service is registered");
    let ops_status = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.subject == API_OPS_STATUS)
        .expect("ops.status endpoint is registered");

    assert_eq!(ops_status.execution, EndpointExecution::Query);
}

#[test]
fn ops_status_returns_typed_missing_operation_error() {
    let operation_id = operation_id("op_missing");

    assert_eq!(
        ops_status_missing(&operation_id),
        OpsStatusError::NoSuchOperation { operation_id }
    );
}

#[test]
fn node_service_failures_map_actual_request_failures() {
    let node_id = node_id("node_7");
    let subject = node_endpoint_subject(&node_id, NodeServiceEndpoint::Inspect);

    assert_eq!(subject, "plz.v1.svc.node.node_7.inspect");
    assert_eq!(
        NodeServiceCallError::from_request_failure(
            &node_id,
            NatsRequestFailure::NoResponders {
                subject: subject.clone()
            }
        ),
        NodeServiceCallError::NodeUnavailable { node_id, subject }
    );
}

#[test]
fn mutating_service_acceptance_is_queued_not_inline_work() {
    let accepted = queued_operation(operation_id("op_123"), event_sequence(11));

    assert_eq!(
        accepted.dispatch,
        OperationDispatch::Queued {
            watch_subject: "plz.v1.op.op_123.>".to_owned(),
            start_sequence: event_sequence(11),
        }
    );
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

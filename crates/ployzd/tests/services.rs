use ployz_core::ids::{NodeId, OperationId, OperationOwnerId};
use ployz_core::ops::{EventSequence, OperationLeaseExpiresAt, OperationOwnerLease};
use ployz_core::subjects::{
    API_BACKUP_CREATE, API_DEPLOY_PLAN, API_DEPLOY_SUBMIT, API_MACHINE_ADD,
    API_MACHINE_JOIN_REPORT, API_OPS_STATUS, API_OPS_WATCH, NodeServiceEndpoint,
};
use ployz_nats::services::{EndpointExecution, NatsRequestFailure, ServiceDiscoveryQuery};
use ployz_sdk_types::OpsStatusError;
use ployzd::operation_api::{ops_status_missing, owned_operation};
use ployzd::services::{DaemonServiceCatalog, NodeServiceCallError, node_endpoint_subject};

#[test]
fn control_catalog_supports_srv_ping_discovery() {
    let node_id = node_id("node_7");
    let catalog = DaemonServiceCatalog::for_control();

    assert_eq!(ServiceDiscoveryQuery::All.subject(), "$SRV.PING");
    assert_eq!(
        ServiceDiscoveryQuery::Service { name: "plz-api" }.subject(),
        "$SRV.PING.plz-api"
    );

    let pings = catalog.discover(ServiceDiscoveryQuery::All);

    assert_eq!(pings.len(), 1);
    assert!(
        pings
            .iter()
            .any(|ping| ping.name == "plz-api" && ping.id == "plz-api.core")
    );
    assert!(catalog.has_endpoint_subject(API_OPS_STATUS));
    assert!(catalog.has_endpoint_subject(API_OPS_WATCH));
    assert!(catalog.has_endpoint_subject(API_BACKUP_CREATE));
    assert!(!catalog.has_endpoint_subject(API_DEPLOY_PLAN));
    assert!(catalog.has_endpoint_subject(API_MACHINE_ADD));
    assert!(catalog.has_endpoint_subject(API_MACHINE_JOIN_REPORT));
    assert!(!catalog.has_endpoint_subject("plz.v1.svc.node.node_7.inspect"));

    let node_catalog = DaemonServiceCatalog::for_node(&node_id);
    assert!(node_catalog.has_endpoint_subject("plz.v1.svc.node.node_7.inspect"));
    assert!(!node_catalog.has_endpoint_subject(API_OPS_STATUS));
}

#[test]
fn service_catalogs_keep_control_and_node_surfaces_separate() {
    let node_id = node_id("node_7");

    let control = DaemonServiceCatalog::for_control();
    let node = DaemonServiceCatalog::for_node(&node_id);

    assert_eq!(service_names(&control), vec!["plz-api"]);
    assert_eq!(service_names(&node), vec!["plz-node"]);

    let pings = node.discover(ServiceDiscoveryQuery::All);
    assert!(pings.iter().any(|ping| {
        ping.name == "plz-node"
            && ping.id == "plz-node.node_7"
            && ping.metadata.get("node_id") == Some(node_id.as_str())
    }));
    assert!(!node.has_endpoint_subject(API_OPS_STATUS));
    assert!(node.has_endpoint_subject("plz.v1.svc.node.node_7.inspect"));
}

#[test]
fn api_service_marks_mutations_as_operation_acceptors() {
    let catalog = DaemonServiceCatalog::for_control();
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
    let machine_add = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.subject == API_MACHINE_ADD)
        .expect("machine.add endpoint is registered");
    let machine_join_report = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.subject == API_MACHINE_JOIN_REPORT)
        .expect("machine.join.report endpoint is registered");
    let backup_create = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.subject == API_BACKUP_CREATE)
        .expect("backup.create endpoint is registered");

    assert_eq!(deploy_submit.execution, EndpointExecution::AcceptsOperation);
    assert_eq!(machine_add.execution, EndpointExecution::AcceptsOperation);
    assert_eq!(backup_create.execution, EndpointExecution::AcceptsOperation);
    assert_eq!(
        machine_join_report.execution,
        EndpointExecution::MutatesOperation
    );
}

#[test]
fn ops_watch_is_a_query_endpoint() {
    let catalog = DaemonServiceCatalog::for_control();
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
    let catalog = DaemonServiceCatalog::for_control();
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
fn mutating_service_acceptance_is_owned_not_inline_work() {
    let lease = operation_lease("op_123", "control", 120);
    let accepted = owned_operation(operation_id("op_123"), event_sequence(11), lease.clone());

    assert_eq!(accepted.owner_lease, lease);
    assert_eq!(accepted.watch_subject, "plz.v1.op.op_123.>".to_owned());
    assert_eq!(accepted.start_sequence, event_sequence(11));
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

fn operation_lease(operation_id: &str, owner_id: &str, expires_at: u64) -> OperationOwnerLease {
    OperationOwnerLease::new(
        OperationId::try_new(operation_id).expect("valid operation id"),
        OperationOwnerId::try_new(owner_id).expect("valid owner id"),
        OperationLeaseExpiresAt::try_new(expires_at).expect("valid lease expiry"),
    )
}

fn service_names(catalog: &DaemonServiceCatalog) -> Vec<&str> {
    catalog
        .services()
        .iter()
        .map(|service| service.name)
        .collect()
}

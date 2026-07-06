use ployz_core::subjects::{
    API_DEPLOY_PLAN, API_DEPLOY_SUBMIT, API_MACHINE_ADD, API_MACHINE_INSPECT,
    API_MACHINE_JOIN_REPORT, API_MACHINE_LIST, API_OPS_STATUS, API_OPS_WATCH, API_SERVICE_INSPECT,
    API_SERVICE_LIST, INTENT_GET,
};
use ployz_nats::services::{EndpointExecution, ServiceDiscoveryQuery};
use ployz_sdk_types::OpsStatusError;
use ployz_test_support::ids::{event_sequence, machine_id, operation_id};
use ployzd::operation_api::{ops_status_missing, owned_operation};
use ployzd::service_catalog::DaemonServiceCatalog;

#[test]
fn control_catalog_supports_srv_ping_discovery() {
    let machine_id = machine_id("machine_7");
    let catalog = DaemonServiceCatalog::for_control();

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
    assert!(catalog.has_endpoint_subject(API_OPS_STATUS));
    assert!(catalog.has_endpoint_subject(API_OPS_WATCH));
    assert!(!catalog.has_endpoint_subject(API_DEPLOY_PLAN));
    assert!(catalog.has_endpoint_subject(API_MACHINE_ADD));
    assert!(catalog.has_endpoint_subject(API_MACHINE_LIST));
    assert!(catalog.has_endpoint_subject(API_MACHINE_INSPECT));
    assert!(catalog.has_endpoint_subject(API_MACHINE_JOIN_REPORT));
    assert!(catalog.has_endpoint_subject(API_SERVICE_LIST));
    assert!(catalog.has_endpoint_subject(API_SERVICE_INSPECT));
    assert!(catalog.has_endpoint_subject(INTENT_GET));
    assert!(!catalog.has_endpoint_subject("plz.v1.svc.machine.machine_7.inspect"));

    let machine_catalog = DaemonServiceCatalog::for_machine(&machine_id);
    assert!(machine_catalog.has_endpoint_subject("plz.v1.svc.machine.machine_7.inspect"));
    assert!(!machine_catalog.has_endpoint_subject(API_OPS_STATUS));
}

#[test]
fn service_catalogs_keep_control_and_machine_surfaces_separate() {
    let machine_id = machine_id("machine_7");

    let control = DaemonServiceCatalog::for_control();
    let machine = DaemonServiceCatalog::for_machine(&machine_id);

    assert_eq!(service_names(&control), vec!["plz-api", "plz-intent"]);
    assert_eq!(service_names(&machine), vec!["plz-machine"]);

    let pings = machine.discover(ServiceDiscoveryQuery::All);
    assert!(pings.iter().any(|ping| {
        ping.name == "plz-machine"
            && ping.id == "plz-machine.machine_7"
            && ping.metadata.get("machine_id") == Some(machine_id.as_str())
    }));
    assert!(!machine.has_endpoint_subject(API_OPS_STATUS));
    assert!(machine.has_endpoint_subject("plz.v1.svc.machine.machine_7.inspect"));
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
    assert_eq!(deploy_submit.execution, EndpointExecution::AcceptsOperation);
    assert_eq!(machine_add.execution, EndpointExecution::AcceptsOperation);
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
fn service_read_endpoints_are_query_endpoints() {
    let catalog = DaemonServiceCatalog::for_control();
    let api = catalog
        .services()
        .iter()
        .find(|service| service.name == "plz-api")
        .expect("api service is registered");

    for subject in [API_SERVICE_LIST, API_SERVICE_INSPECT] {
        let endpoint = api
            .endpoints
            .iter()
            .find(|endpoint| endpoint.subject == subject)
            .expect("service endpoint is registered");
        assert_eq!(endpoint.execution, EndpointExecution::Query);
    }
}

#[test]
fn machine_read_endpoints_are_query_endpoints() {
    let catalog = DaemonServiceCatalog::for_control();
    let api = catalog
        .services()
        .iter()
        .find(|service| service.name == "plz-api")
        .expect("api service is registered");
    for subject in [API_MACHINE_LIST, API_MACHINE_INSPECT] {
        let endpoint = api
            .endpoints
            .iter()
            .find(|endpoint| endpoint.subject == subject)
            .expect("machine read endpoint is registered");
        assert_eq!(endpoint.execution, EndpointExecution::Query);
    }
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
fn mutating_service_acceptance_returns_operation_pointer() {
    let accepted = owned_operation(operation_id("op_123"), event_sequence(11));

    assert_eq!(accepted.operation_id, operation_id("op_123"));
    assert_eq!(accepted.watch_subject, "plz.v1.op.op_123.>".to_owned());
    assert_eq!(accepted.start_sequence, event_sequence(11));
}

fn service_names(catalog: &DaemonServiceCatalog) -> Vec<&str> {
    catalog
        .services()
        .iter()
        .map(|service| service.name)
        .collect()
}

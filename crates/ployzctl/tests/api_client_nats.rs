use ployz_core::deploy::{DeployRequest, ImageReference, ReplicaCount};
use ployz_core::ids::{OperationId, OperationOwnerId, RevisionId};
use ployz_core::ops::{
    EventSequence, OperationEventReplayLimit, OperationEventReplayRequest, OperationIdempotencyKey,
    OperationLeaseExpiresAt, OperationOwnerLease,
};
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::service_runtime::{
    NatsServiceError, NatsServiceErrorCode, NatsServiceResponse, start_nats_service,
};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    AcceptedOperation, DeploySubmitError, DeploySubmitRequest, DeploySubmitResponse,
    OperationApiResponse, OpsStatusError, OpsStatusResponse,
    operation_api::{DeploySubmitApi, OperationApiContract, OpsStatusApi, OpsWatchApi},
};
use ployzctl::api_client::{
    OperationApiClient, OperationApiClientError, OperationApiRequestFailure,
};

#[tokio::test]
async fn operation_api_client_decodes_successful_envelope() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: DeploySubmitResponse = OperationApiResponse::Ok {
                value: AcceptedOperation {
                    operation_id: operation_id("op_123"),
                    watch_subject: "plz.v1.op.op_123.>".to_owned(),
                    start_sequence: event_sequence(1),
                    owner_lease: operation_lease("op_123", "control", 120),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let accepted = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect("deploy submit responds");

    assert_eq!(
        accepted,
        AcceptedOperation {
            operation_id: operation_id("op_123"),
            watch_subject: "plz.v1.op.op_123.>".to_owned(),
            start_sequence: event_sequence(1),
            owner_lease: operation_lease("op_123", "control", 120),
        }
    );
}

#[tokio::test]
async fn operation_api_client_returns_service_error_headers_as_transport_failure() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            NatsServiceResponse::transport_error(NatsServiceError::bad_request("bad json"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let error = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect_err("service error returns client error");

    assert_eq!(
        error,
        OperationApiClientError::Service {
            endpoint: OperationApiEndpoint::DeploySubmit,
            failure: NatsServiceError {
                code: NatsServiceErrorCode::BadRequest,
                message: "bad json".to_owned(),
            },
        }
    );
}

#[tokio::test]
async fn operation_api_client_returns_domain_error_envelope_as_domain_failure() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: DeploySubmitResponse = OperationApiResponse::DomainError {
                error: DeploySubmitError::DuplicateSequenceMismatch {
                    operation_id: operation_id("op_123"),
                    sequence: event_sequence(42),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let error = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect_err("domain error envelope returns client domain error");

    assert_eq!(
        error,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::DeploySubmit,
            error: DeploySubmitError::DuplicateSequenceMismatch {
                operation_id: operation_id("op_123"),
                sequence: event_sequence(42),
            },
        }
    );
}

#[tokio::test]
async fn operation_api_client_reports_decode_failure_for_invalid_payload() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            NatsServiceResponse::ok("not json")
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let error = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect_err("invalid payload returns decode error");

    assert!(matches!(
        error,
        OperationApiClientError::DecodeResponse {
            endpoint: OperationApiEndpoint::DeploySubmit,
            ..
        }
    ));
}

#[tokio::test]
async fn operation_api_client_routes_ops_status_domain_errors() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(OpsStatusApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: OpsStatusResponse = OperationApiResponse::DomainError {
                error: OpsStatusError::NoSuchOperation {
                    operation_id: operation_id("op_missing"),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let error = api
        .ops_status(&ployz_sdk_types::OpsStatusRequest {
            operation_id: operation_id("op_missing"),
        })
        .await
        .expect_err("domain error envelope returns client domain error");

    assert_eq!(
        error,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::OpsStatus,
            error: OpsStatusError::NoSuchOperation {
                operation_id: operation_id("op_missing"),
            },
        }
    );
}

#[tokio::test]
async fn operation_api_client_routes_ops_watch_decode_failures() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(OpsWatchApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            NatsServiceResponse::ok(br#"{"status":"ok","value":"wrong shape"}"#.to_vec())
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let error = api
        .ops_watch(&ops_watch_request())
        .await
        .expect_err("invalid watch payload returns decode error");

    assert!(matches!(
        error,
        OperationApiClientError::DecodeResponse {
            endpoint: OperationApiEndpoint::OpsWatch,
            ..
        }
    ));
}

#[tokio::test]
async fn operation_api_client_reports_no_responders_as_typed_request_failure() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let api = OperationApiClient::new(client);

    let error = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect_err("missing service returns request failure");

    assert_eq!(
        error,
        OperationApiClientError::Request {
            endpoint: OperationApiEndpoint::DeploySubmit,
            failure: OperationApiRequestFailure::NoResponders,
        }
    );
}

fn test_api_service(endpoint: OperationApiEndpoint) -> NatsServiceSpec {
    NatsServiceSpec::new(
        "plz-api.test",
        "plz-api",
        ServiceVersion::new(0, 1, 0),
        "test API service",
        ServiceMetadata::empty(),
        vec![NatsServiceEndpointSpec::new(
            endpoint.name(),
            endpoint.subject(),
            endpoint_execution(endpoint.execution()),
        )],
    )
}

const fn endpoint_execution(execution: OperationApiEndpointExecution) -> EndpointExecution {
    match execution {
        OperationApiEndpointExecution::AcceptsOperation => EndpointExecution::AcceptsOperation,
        OperationApiEndpointExecution::Query => EndpointExecution::Query,
    }
}

fn deploy_submit_request() -> DeploySubmitRequest {
    DeploySubmitRequest {
        operation_id: operation_id("op_123"),
        idempotency_key: OperationIdempotencyKey::try_new("idem_1").expect("valid idempotency key"),
        target: deploy_target("svc_api"),
    }
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        service_id: ployz_core::ids::ServiceId::try_new(service_id).expect("valid service id"),
        target_revision: RevisionId::try_new("rev_2").expect("valid revision id"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn operation_lease(operation_id: &str, owner_id: &str, expires_at: u64) -> OperationOwnerLease {
    OperationOwnerLease::new(
        OperationId::try_new(operation_id).expect("valid operation id"),
        OperationOwnerId::try_new(owner_id).expect("valid owner id"),
        OperationLeaseExpiresAt::try_new(expires_at).expect("valid lease expiry"),
    )
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn ops_watch_request() -> OperationEventReplayRequest {
    OperationEventReplayRequest {
        operation_id: operation_id("op_123"),
        start_sequence: event_sequence(1),
        limit: OperationEventReplayLimit::try_new(10).expect("valid replay limit"),
    }
}

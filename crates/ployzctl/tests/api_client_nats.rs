use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{
    EventSequence, OperationEventReplayLimit, OperationEventReplayRequest, OperationIdempotencyKey,
};
use ployz_core::subjects::{
    API_DEPLOY_SUBMIT, API_OPS_STATUS, API_OPS_WATCH, OperationApiEndpoint,
};
use ployz_nats::service_runtime::{
    NatsServiceError, NatsServiceErrorCode, NatsServiceResponse, start_nats_service,
};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    AcceptedOperation, DeploySubmitError, DeploySubmitRequest, DeploySubmitResponse,
    OperationApiResponse, OperationDispatch, OpsStatusError, OpsStatusResponse,
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
    let spec = test_api_service(
        "deploy.submit",
        API_DEPLOY_SUBMIT,
        EndpointExecution::AcceptsOperation,
    );
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: DeploySubmitResponse = OperationApiResponse::Ok {
                value: AcceptedOperation {
                    operation_id: operation_id("op_123"),
                    dispatch: OperationDispatch::Queued {
                        watch_subject: "plz.v1.op.op_123.>".to_owned(),
                        start_sequence: event_sequence(1),
                    },
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
            dispatch: OperationDispatch::Queued {
                watch_subject: "plz.v1.op.op_123.>".to_owned(),
                start_sequence: event_sequence(1),
            },
        }
    );
}

#[tokio::test]
async fn operation_api_client_returns_service_error_headers_as_transport_failure() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(
        "deploy.submit",
        API_DEPLOY_SUBMIT,
        EndpointExecution::AcceptsOperation,
    );
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
    let spec = test_api_service(
        "deploy.submit",
        API_DEPLOY_SUBMIT,
        EndpointExecution::AcceptsOperation,
    );
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
    let spec = test_api_service(
        "deploy.submit",
        API_DEPLOY_SUBMIT,
        EndpointExecution::AcceptsOperation,
    );
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
    let spec = test_api_service("ops.status", API_OPS_STATUS, EndpointExecution::Query);
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
    let spec = test_api_service("ops.watch", API_OPS_WATCH, EndpointExecution::Query);
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

fn test_api_service(
    name: &'static str,
    subject: &str,
    execution: EndpointExecution,
) -> NatsServiceSpec {
    NatsServiceSpec::new(
        "plz-api.test",
        "plz-api",
        ServiceVersion::new(0, 1, 0),
        "test API service",
        ServiceMetadata::empty(),
        vec![NatsServiceEndpointSpec::new(name, subject, execution)],
    )
}

fn deploy_submit_request() -> DeploySubmitRequest {
    DeploySubmitRequest {
        operation_id: operation_id("op_123"),
        idempotency_key: OperationIdempotencyKey::try_new("idem_1").expect("valid idempotency key"),
        service_id: ServiceId::try_new("svc_api").expect("valid service id"),
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
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

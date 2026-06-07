use std::process::{Command, Output};

use ployz_core::deploy::{ImageReference, ReplicaCount};
use ployz_core::ids::{OperationId, OperationOwnerId, RevisionId, ServiceId};
use ployz_core::ops::{
    EventSequence, OperationIdempotencyKey, OperationLeaseExpiresAt, OperationOwnerLease,
};
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    AcceptedOperation, DeploySubmitRequest, DeploySubmitResponse, OperationApiResponse,
    operation_api::{DeploySubmitApi, OperationApiContract},
};

#[tokio::test(flavor = "multi_thread")]
async fn binary_deploy_detach_calls_nats_service() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let service_client = client.clone();
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client, &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |request| async move {
            let request: DeploySubmitRequest =
                serde_json::from_slice(&request.payload).expect("deploy request decodes");
            assert_eq!(request.operation_id, operation_id("op_deploy"));
            assert_eq!(
                request.idempotency_key,
                OperationIdempotencyKey::try_new("idem_deploy").expect("valid idempotency key")
            );
            assert_eq!(
                request.target.service_id,
                ServiceId::try_new("svc_api").expect("valid service id")
            );
            assert_eq!(
                request.target.target_revision,
                RevisionId::try_new("rev_2").expect("valid revision id")
            );
            assert_eq!(
                request.target.image,
                ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image")
            );
            assert_eq!(
                request.target.replicas,
                ReplicaCount::try_new(1).expect("valid replicas")
            );

            let response: DeploySubmitResponse = OperationApiResponse::Ok {
                value: accepted_operation("op_deploy"),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    service_client.flush().await.expect("service flushes");

    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .arg("--nats")
        .arg(server.client_url())
        .args(detached_deploy_args())
        .output()
        .expect("ployzctl binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        stdout(&output),
        "operation op_deploy\nwatch ployzctl ops watch op_deploy\n"
    );
    assert_eq!(stderr(&output), "");
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
        OperationApiEndpointExecution::MutatesOperation => EndpointExecution::MutatesOperation,
        OperationApiEndpointExecution::Query => EndpointExecution::Query,
    }
}

fn accepted_operation(operation_id: &str) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: self::operation_id(operation_id),
        watch_subject: format!("plz.v1.op.{operation_id}.>"),
        start_sequence: event_sequence(1),
        owner_lease: OperationOwnerLease::new(
            self::operation_id(operation_id),
            OperationOwnerId::try_new("control").expect("valid owner id"),
            OperationLeaseExpiresAt::try_new(120).expect("valid lease expiry"),
        ),
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn detached_deploy_args() -> [&'static str; 14] {
    [
        "deploy",
        "--detach",
        "--service",
        "svc_api",
        "--revision",
        "rev_2",
        "--image",
        "ghcr.io/acme/api:rev-2",
        "--replicas",
        "1",
        "--operation",
        "op_deploy",
        "--idempotency-key",
        "idem_deploy",
    ]
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

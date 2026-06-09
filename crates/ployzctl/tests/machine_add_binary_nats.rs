use std::process::{Command, Output};

use ployz_core::ids::{NodeId, OperationId, OperationOwnerId};
use ployz_core::ops::{
    EventSequence, OperationIdempotencyKey, OperationLeaseExpiresAt, OperationOwnerLease,
};
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    AcceptedOperation, MachineAddAccepted, MachineAddGateway, MachineAddRequest,
    MachineAddResponse, MachineBootstrapUrl, MachineJoinRuntimeNatsUrl, MachineJoinToken,
    MachineName, OperationApiResponse,
    operation_api::{MachineAddApi, OperationApiContract},
};

#[tokio::test(flavor = "multi_thread")]
async fn binary_machine_add_calls_nats_service() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let service_client = client.clone();
    let spec = test_api_service(MachineAddApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client, &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |request| async move {
            let request: MachineAddRequest =
                serde_json::from_slice(&request.payload).expect("machine add request decodes");
            assert_eq!(request.operation_id, operation_id("op_machine"));
            assert_eq!(
                request.idempotency_key,
                OperationIdempotencyKey::try_new("idem_machine").expect("valid idempotency key")
            );
            assert_eq!(request.node_id, node_id("node_2"));
            assert_eq!(
                request.name,
                MachineName::try_new("edge_2").expect("valid machine name")
            );
            assert_eq!(request.gateway, MachineAddGateway::Skip);

            let response: MachineAddResponse = OperationApiResponse::Ok {
                value: MachineAddAccepted {
                    accepted: accepted_operation("op_machine"),
                    node_id: node_id("node_2"),
                    bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
                        .expect("valid bootstrap url"),
                    bootstrap_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                        .expect("valid bootstrap NATS URL"),
                    join_token: MachineJoinToken::try_new("join_once_123")
                        .expect("valid join token"),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    service_client.flush().await.expect("service flushes");

    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .arg("--nats")
        .arg(server.client_url())
        .args([
            "machine",
            "add",
            "--node",
            "node_2",
            "--name",
            "edge_2",
            "--operation",
            "op_machine",
            "--idempotency-key",
            "idem_machine",
        ])
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
        "operation op_machine\nnode node_2\njoin-token join_once_123\ninstall curl -fsSL -- 'https://get.ployz.sh' | PLOYZ_NATS_URL='nats://127.0.0.1:7422' sh -s -- --join-token 'join_once_123'\n"
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

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

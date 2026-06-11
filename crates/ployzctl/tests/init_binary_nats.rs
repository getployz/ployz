use std::process::{Command, Output};
use std::time::Duration;

use ployz_core::ids::{NodeId, OperationId};
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::connect::connect_authenticated;
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    InitFirstNodeActivateRequest, InitFirstNodeActivateResponse, InitFirstNodeActivated,
    MachineAddGateway, OperationApiResponse,
    operation_api::{InitFirstNodeActivateApi, OperationApiContract},
};
use ployz_test_support::nats::SecuredTestNats;
use ployzctl::runtime::{PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_NKEY_SEED_FILE_ENV};

#[tokio::test(flavor = "multi_thread")]
async fn binary_init_can_activate_first_machine_without_running_keeper() {
    let server = SecuredTestNats::start().await.expect("secured test nats");
    let client = connect_authenticated(&server.controller_config(), Duration::from_secs(5))
        .await
        .expect("connect to test nats");
    let env = CliNatsEnv::new(&server);
    let service_client = client.clone();
    let spec = test_api_service(InitFirstNodeActivateApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client, &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |request| async move {
            let request: InitFirstNodeActivateRequest =
                serde_json::from_slice(&request.payload).expect("activate request decodes");
            assert_eq!(request.node_id, node_id("core_1"));
            assert_eq!(request.gateway, MachineAddGateway::Install);

            let response: InitFirstNodeActivateResponse = OperationApiResponse::Ok {
                value: InitFirstNodeActivated {
                    operation_id: operation_id("op_init_core_1"),
                    node_id: node_id("core_1"),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    service_client.flush().await.expect("service flushes");

    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .arg("--nats")
        .arg(server.client_url().as_str())
        .env(PLOYZ_NATS_CA_FILE_ENV, server.ca_path())
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, env.user_seed_path())
        .args([
            "init",
            "activate-first-node",
            "--node",
            "core_1",
            "--gateway",
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
        "operation op_init_core_1\nfirst-node core_1 active\n"
    );
    assert_eq!(stderr(&output), "");

    runtime.shutdown().await.expect("service shuts down");
}

struct CliNatsEnv {
    _dir: tempfile::TempDir,
    user_seed_file: std::path::PathBuf,
}

impl CliNatsEnv {
    fn new(server: &SecuredTestNats) -> Self {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let user_seed_file = dir.path().join("user.seed");
        std::fs::write(&user_seed_file, server.user_seed().secret()).expect("write user seed");
        Self {
            _dir: dir,
            user_seed_file,
        }
    }

    fn user_seed_path(&self) -> &std::path::Path {
        &self.user_seed_file
    }
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

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

use std::process::{Command, Output};

use ployz_core::deploy::{ImageReference, ReplicaCount};
use ployz_core::ids::{RevisionId, ServiceId};
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    AcceptedOperation, DeploySubmitRequest, DeploySubmitResponse, OperationApiResponse,
    operation_api::{DeploySubmitApi, OperationApiContract},
};
use ployz_test_support::ids::{event_sequence, operation_id};
use ployz_test_support::nats::{SecuredTestNats, TestNats};
use ployzctl::runtime::{PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_NKEY_SEED_FILE_ENV};

#[tokio::test(flavor = "multi_thread")]
async fn binary_deploy_calls_nats_service() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
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
            assert!(
                request
                    .operation_id
                    .as_str()
                    .starts_with("op_deploy_svc_api_")
            );
            assert_eq!(
                request.target.services[0].service_id,
                ServiceId::try_new("svc_api").expect("valid service id")
            );
            assert_eq!(
                request.target.target_revision,
                RevisionId::try_new("rev_2").expect("valid revision id")
            );
            assert_eq!(
                request.target.services[0].image,
                ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image")
            );
            assert_eq!(
                request.target.services[0].replicas,
                ReplicaCount::try_new(1).expect("valid replicas")
            );

            let response: DeploySubmitResponse = OperationApiResponse::Ok {
                value: accepted_operation(request.operation_id.as_str()),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    service_client.flush().await.expect("service flushes");

    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .arg("--nats")
        .arg(server.server.client_url().as_str())
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env(PLOYZ_NATS_CA_FILE_ENV, server.server.ca_path())
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, env.user_seed_path())
        .args(deploy_args())
        .output()
        .expect("ployzctl binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).starts_with("operation op_deploy_svc_api_"));
    assert!(stdout(&output).contains("watch ployzctl ops watch op_deploy_svc_api_"));
    assert_eq!(stderr(&output), "");
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

fn accepted_operation(operation_id: &str) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: self::operation_id(operation_id),
        watch_subject: format!("plz.v1.op.{operation_id}.>"),
        start_sequence: event_sequence(1),
    }
}

fn deploy_args() -> [&'static str; 10] {
    [
        "deploy",
        "--service",
        "svc_api",
        "--revision",
        "rev_2",
        "--image",
        "ghcr.io/acme/api:rev-2",
        "--replicas",
        "1",
        "--detach",
    ]
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

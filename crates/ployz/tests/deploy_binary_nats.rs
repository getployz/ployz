use std::process::{Command, Output};

use ployz::runtime::{PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_NKEY_SEED_FILE_ENV};
use ployz_core::deploy::{ImageReference, ReplicaCount};
use ployz_core::ids::ServiceId;
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    AcceptedOperation, DeployReservationExpiresAt, DeployReservationId, DeployReserveResponse,
    DeployReserved, DeploySubmitRequest, DeploySubmitResponse, OperationApiResponse,
    operation_api::{DeployReserveApi, DeploySubmitApi, OperationApiContract},
};
use ployz_test_support::ids::{event_sequence, operation_id};
use ployz_test_support::nats::{SecuredTestNats, TestNats};

#[tokio::test(flavor = "multi_thread")]
async fn binary_deploy_calls_nats_service() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
    let service_client = client.clone();
    let spec = test_api_service(&[DeployReserveApi::ENDPOINT, DeploySubmitApi::ENDPOINT]);
    let reserve_endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| endpoint.subject == DeployReserveApi::ENDPOINT.subject())
        .expect("deploy reserve endpoint is present")
        .clone();
    let submit_endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| endpoint.subject == DeploySubmitApi::ENDPOINT.subject())
        .expect("deploy submit endpoint is present")
        .clone();
    let mut runtime = start_nats_service(client, &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(&reserve_endpoint, |_request| async move {
            let response: DeployReserveResponse = OperationApiResponse::Ok {
                value: DeployReserved {
                    reservation_id: DeployReservationId::first(),
                    expires_at: DeployReservationExpiresAt::try_new(4_102_444_800)
                        .expect("valid expiration"),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("reserve endpoint binds");
    runtime
        .bind_endpoint(&submit_endpoint, |request| async move {
            let request: DeploySubmitRequest =
                serde_json::from_slice(&request.payload).expect("deploy request decodes");
            assert_eq!(request.reservation_id, DeployReservationId::first());
            assert!(
                request
                    .idempotency_key
                    .as_str()
                    .starts_with("idem_deploy_svc_api_")
            );
            let [service] = request.target.services.as_slice() else {
                panic!("deploy request has one service");
            };
            assert_eq!(
                service.service_id,
                ServiceId::try_new("svc_api").expect("valid service id")
            );
            assert_eq!(
                service.image,
                ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image")
            );
            assert_eq!(
                service.replicas,
                ReplicaCount::try_new(1).expect("valid replicas")
            );

            let response: DeploySubmitResponse = OperationApiResponse::Ok {
                value: accepted_operation("op_deploy_minted"),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    service_client.flush().await.expect("service flushes");

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--nats")
        .arg(server.server.client_url().as_str())
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env(PLOYZ_NATS_CA_FILE_ENV, server.server.ca_path())
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, env.user_seed_path())
        .args(deploy_args())
        .output()
        .expect("ployz binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).starts_with("operation op_deploy_minted"));
    assert!(stdout(&output).contains("watch ployz ops watch op_deploy_minted"));
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

fn test_api_service(endpoints: &[OperationApiEndpoint]) -> NatsServiceSpec {
    NatsServiceSpec::new(
        "plz-api.test",
        "plz-api",
        ServiceVersion::new(0, 1, 0),
        "test API service",
        ServiceMetadata::empty(),
        endpoints
            .iter()
            .copied()
            .map(|endpoint| {
                NatsServiceEndpointSpec::new(
                    endpoint.name(),
                    endpoint.subject(),
                    endpoint_execution(endpoint.execution()),
                )
            })
            .collect(),
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
        watch_subject: format!("plz.v1.progress.namespace.default.operation.{operation_id}.>"),
        start_sequence: event_sequence(1),
    }
}

fn deploy_args() -> [&'static str; 8] {
    [
        "deploy",
        "--service",
        "svc_api",
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

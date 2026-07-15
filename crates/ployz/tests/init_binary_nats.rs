use std::process::{Command, Output};

use ployz::dispatcher::{
    PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_NKEY_SEED_FILE_ENV, PLOYZ_NATS_URL_ENV,
};
use ployz::machine::operator_context::{
    CLUSTER_CONTEXT_FILE_NAME, ClusterContext, save_cluster_context,
};
use ployz_core::roles::GatewayRole;
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::service_runtime::{NatsServiceResponse, RunningNatsService, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    InitFirstMachineActivateRequest, InitFirstMachineActivateResponse, InitFirstMachineActivated,
    OperationApiResponse,
    operation_api::{InitFirstMachineActivateApi, OperationApiContract},
};
use ployz_test_support::ids::{machine_id, operation_id};
use ployz_test_support::nats::{SecuredTestNats, TestNats};

#[tokio::test(flavor = "multi_thread")]
async fn binary_init_can_activate_first_machine_without_running_host_runner() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
    let service_client = client.clone();
    let spec = test_api_service(InitFirstMachineActivateApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client, &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |request| async move {
            let request: InitFirstMachineActivateRequest =
                serde_json::from_slice(&request.payload).expect("activate request decodes");
            assert_eq!(request.machine_id, machine_id("core_1"));
            assert_eq!(request.roles.gateway, GatewayRole::Install);

            let response: InitFirstMachineActivateResponse = OperationApiResponse::Ok {
                value: InitFirstMachineActivated {
                    operation_id: operation_id("op_init_core_1"),
                    machine_id: machine_id("core_1"),
                },
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
        .args([
            "internal",
            "init",
            "activate-first-machine",
            "--machine",
            "core_1",
        ])
        .output()
        .expect("ployz binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        stdout(&output),
        "operation op_init_core_1\nfirst-machine core_1 active\n"
    );
    assert_eq!(stderr(&output), "");

    runtime.shutdown().await.expect("service shuts down");
}

/// U2: with no `--nats` flag and no NATS environment variables, the binary
/// connects through the cluster context recorded under `XDG_CONFIG_HOME`.
#[tokio::test(flavor = "multi_thread")]
async fn binary_loads_cluster_context_when_flag_and_env_are_absent() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
    let runtime = bind_activate_first_machine_service(client).await;

    let config_home = tempfile::TempDir::new().expect("config home dir");
    write_cluster_context(
        config_home.path(),
        server.server.client_url().clone(),
        &server,
        &env,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env_remove("HOME")
        .env_remove(PLOYZ_NATS_URL_ENV)
        .env_remove(PLOYZ_NATS_CA_FILE_ENV)
        .env_remove(PLOYZ_NATS_NKEY_SEED_FILE_ENV)
        .env("XDG_CONFIG_HOME", config_home.path())
        .args([
            "internal",
            "init",
            "activate-first-machine",
            "--machine",
            "core_1",
        ])
        .output()
        .expect("ployz binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        stdout(&output),
        "operation op_init_core_1\nfirst-machine core_1 active\n"
    );

    runtime.shutdown().await.expect("service shuts down");
}

/// U2: environment variables beat the cluster context. The context points at
/// a dead NATS port; the env URL points at the live server, so success
/// proves the env value won.
#[tokio::test(flavor = "multi_thread")]
async fn binary_env_nats_url_overrides_cluster_context() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
    let runtime = bind_activate_first_machine_service(client).await;

    let config_home = tempfile::TempDir::new().expect("config home dir");
    write_cluster_context(
        config_home.path(),
        NatsClientUrl::try_new("tls://127.0.0.1:9").expect("dead-port URL is well-formed"),
        &server,
        &env,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env_remove("HOME")
        .env("XDG_CONFIG_HOME", config_home.path())
        .env(PLOYZ_NATS_URL_ENV, server.server.client_url().as_str())
        .env(PLOYZ_NATS_CA_FILE_ENV, server.server.ca_path())
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, env.user_seed_path())
        .args([
            "internal",
            "init",
            "activate-first-machine",
            "--machine",
            "core_1",
        ])
        .output()
        .expect("ployz binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );

    runtime.shutdown().await.expect("service shuts down");
}

/// Binds a stub activate-first-machine API that accepts machine `core_1`.
async fn bind_activate_first_machine_service(client: async_nats::Client) -> RunningNatsService {
    let service_client = client.clone();
    let spec = test_api_service(InitFirstMachineActivateApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client, &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |request| async move {
            let request: InitFirstMachineActivateRequest =
                serde_json::from_slice(&request.payload).expect("activate request decodes");
            assert_eq!(request.machine_id, machine_id("core_1"));

            let response: InitFirstMachineActivateResponse = OperationApiResponse::Ok {
                value: InitFirstMachineActivated {
                    operation_id: operation_id("op_init_core_1"),
                    machine_id: machine_id("core_1"),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    service_client.flush().await.expect("service flushes");
    runtime
}

fn write_cluster_context(
    config_home: &std::path::Path,
    nats_url: NatsClientUrl,
    server: &TestNats,
    env: &CliNatsEnv,
) {
    let context_path = config_home.join("ployz").join(CLUSTER_CONTEXT_FILE_NAME);
    save_cluster_context(
        &context_path,
        &ClusterContext {
            nats_url,
            nats_ca_file: server.server.ca_path().to_owned(),
            operator_seed_file: env.user_seed_path().to_owned(),
            join_seed_file: None,
            machines: Vec::new(),
        },
    )
    .expect("cluster context saves");
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

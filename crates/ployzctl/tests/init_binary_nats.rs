use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::ids::{NodeId, OperationId};
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    InitFirstNodeActivateRequest, InitFirstNodeActivateResponse, InitFirstNodeActivated,
    MachineAddGateway, OperationApiResponse,
    operation_api::{InitFirstNodeActivateApi, OperationApiContract},
};

#[tokio::test(flavor = "multi_thread")]
async fn binary_init_run_keeper_can_explicitly_activate_first_machine() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
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

    let temp = temp_dir("ployzctl-init-nats");
    let keeper = temp.join("ployz-keeper");
    write_executable(&keeper, "#!/bin/sh\nprintf 'keeper installed\\n'\n");

    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .arg("--nats")
        .arg(server.client_url())
        .args(init_with_keeper_run_args(
            keeper.to_str().expect("keeper path is utf-8"),
        ))
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
        "keeper installed\noperation op_init_core_1\nfirst-node core_1 active\n"
    );
    assert_eq!(stderr(&output), "");

    runtime.shutdown().await.expect("service shuts down");
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

fn init_with_keeper_run_args(keeper: &str) -> Vec<&str> {
    vec![
        "init",
        "--node",
        "core_1",
        "--gateway",
        "--run-keeper-install",
        "--activate-first-node",
        "--keeper-binary",
        keeper,
        "--ployzd-version",
        "0.1.0",
        "--ployzd-source",
        "/tmp/ployzd",
        "--ployzd-sha256",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--ployzd-install-path",
        "/usr/local/bin/ployzd",
        "--ebpf-bytecode-version",
        "0.1.0",
        "--ebpf-bytecode-source",
        "/tmp/ployz-ebpf-tc",
        "--ebpf-bytecode-sha256",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--ebpf-bytecode-install-path",
        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
        "--ebpf-ctl-version",
        "0.1.0",
        "--ebpf-ctl-source",
        "/tmp/ployz-ebpf-ctl",
        "--ebpf-ctl-sha256",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--ebpf-ctl-install-path",
        "/usr/local/bin/ployz-ebpf-ctl",
        "--nats-version",
        "2.12.0",
        "--nats-source",
        "/tmp/nats-server",
        "--nats-sha256",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--nats-binary",
        "/usr/local/bin/nats-server",
        "--nats-config",
        "/etc/nats/nats-server.conf",
    ]
}

fn temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path).expect("temp dir can be created");
    path
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("executable can be written");
    let mut permissions = fs::metadata(path)
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("executable permissions can be set");
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

use std::process::{Command, Output};

use base64::Engine as _;
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
    InstallSha256Digest, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
    MachineJoinRuntimeNatsUrl, MachineJoinTrustedNats,
};
use ployz_core::nats_config::NatsCaCertificatePem;
use ployz_core::ops::OperationIdempotencyKey;
use ployz_core::roles::GatewayRole;
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    AcceptedOperation, MachineAddAccepted, MachineAddRequest, MachineAddResponse,
    MachineBootstrapUrl, MachineJoinToken, MachineName, OperationApiResponse,
    operation_api::{MachineAddApi, OperationApiContract},
};
use ployz_test_support::ids::{event_sequence, machine_id, operation_id};
use ployz_test_support::nats::{SecuredTestNats, TestNats};
use ployz_test_support::shell::shell_quote;
use ployzctl::runtime::{
    PLOYZ_JOIN_NKEY_SEED_FILE_ENV, PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_NKEY_SEED_FILE_ENV,
};

#[tokio::test(flavor = "multi_thread")]
async fn binary_machine_add_calls_nats_service() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
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
            assert_eq!(request.machine_id, machine_id("machine_2"));
            assert_eq!(
                request.name,
                MachineName::try_new("edge_2").expect("valid machine name")
            );
            assert_eq!(request.roles.gateway, GatewayRole::Skip);

            let response: MachineAddResponse = OperationApiResponse::Ok {
                value: MachineAddAccepted {
                    accepted: accepted_operation("op_machine"),
                    machine_id: machine_id("machine_2"),
                    bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
                        .expect("valid bootstrap url"),
                    join_bundle: machine_join_bundle(),
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
        .arg(server.server.client_url().as_str())
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env(PLOYZ_NATS_CA_FILE_ENV, server.server.ca_path())
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, env.user_seed_path())
        .env(PLOYZ_JOIN_NKEY_SEED_FILE_ENV, env.join_seed_path())
        .args([
            "machine",
            "add",
            "--machine",
            "machine_2",
            "--name",
            "edge_2",
            "--operation",
            "op_machine",
            "--idempotency-key",
            "idem_machine",
            "--no-gateway",
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
        format!(
            "operation op_machine\nmachine machine_2\njoin-token join_once_123\ninstall curl -fsSL -- 'https://get.ployz.sh' | PLOYZ_VERSION='0.1.0' PLOYZ_NATS_URL='nats://127.0.0.1:7422' PLOYZ_NATS_CA_B64={} PLOYZ_JOIN_NKEY_SEED={} sh -s -- --join-token 'join_once_123'\n",
            shell_quote(&test_ca_b64()),
            shell_quote(server.server.join_seed().secret())
        )
    );
    assert_eq!(stderr(&output), "");
}

struct CliNatsEnv {
    _dir: tempfile::TempDir,
    user_seed_file: std::path::PathBuf,
    join_seed_file: std::path::PathBuf,
}

impl CliNatsEnv {
    fn new(server: &SecuredTestNats) -> Self {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let user_seed_file = dir.path().join("user.seed");
        let join_seed_file = dir.path().join("join.seed");
        std::fs::write(&user_seed_file, server.user_seed().secret()).expect("write user seed");
        std::fs::write(&join_seed_file, server.join_seed().secret()).expect("write join seed");
        Self {
            _dir: dir,
            user_seed_file,
            join_seed_file,
        }
    }

    fn user_seed_path(&self) -> &std::path::Path {
        &self.user_seed_file
    }

    fn join_seed_path(&self) -> &std::path::Path {
        &self.join_seed_file
    }
}

fn test_ca_b64() -> String {
    base64::engine::general_purpose::STANDARD
        .encode("-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n")
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

fn machine_join_bundle() -> MachineJoinBundle {
    MachineJoinBundle {
        material: MachineJoinMaterial {
            cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                .expect("valid runtime nats url"),
            trusted_nats: MachineJoinTrustedNats {
                ca_pem: NatsCaCertificatePem::try_new(
                    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                )
                .expect("valid ca pem"),
            },
            ployzd: InstallArtifactSpec {
                version: version("0.1.0"),
                source: source("/tmp/ployzd"),
                sha256: digest("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                install_path: absolute_path("/usr/local/bin/ployzd"),
            },
            ebpf_bytecode: InstallArtifactSpec {
                version: version("0.1.0"),
                source: source("/tmp/ployz-ebpf-tc"),
                sha256: digest("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                install_path: absolute_path("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"),
            },
            ebpf_ctl: InstallArtifactSpec {
                version: version("0.1.0"),
                source: source("/tmp/ployz-ebpf-ctl"),
                sha256: digest("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                install_path: absolute_path("/usr/local/bin/ployz-ebpf-ctl"),
            },
        },
    }
}

fn version(value: &str) -> InstallArtifactVersion {
    InstallArtifactVersion::try_new(value).expect("valid artifact version")
}

fn source(value: &str) -> InstallArtifactSource {
    InstallArtifactSource::try_new(value).expect("valid artifact source")
}

fn digest(value: &str) -> InstallSha256Digest {
    InstallSha256Digest::try_new(value).expect("valid digest")
}

fn absolute_path(value: &str) -> AbsoluteInstallPath {
    AbsoluteInstallPath::try_new(value).expect("valid absolute install path")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

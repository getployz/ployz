use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
    MachineAddResponse, MachineBootstrapUrl, MachineJoinRedeemRequest, MachineJoinRedeemResponse,
    MachineJoinRedeemResult, MachineJoinRedeemed, MachineJoinReportOutcome,
    MachineJoinReportRequest, MachineJoinReportResponse, MachineJoinReported, MachineJoinToken,
    MachineName, OperationApiResponse,
    operation_api::{
        MachineAddApi, MachineJoinRedeemApi, MachineJoinReportApi, OperationApiContract,
    },
};

#[tokio::test(flavor = "multi_thread")]
async fn binary_init_run_keeper_records_first_machine_operation() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let service_client = client.clone();
    let spec = test_api_service(vec![
        MachineAddApi::ENDPOINT,
        MachineJoinRedeemApi::ENDPOINT,
        MachineJoinReportApi::ENDPOINT,
    ]);
    let add_endpoint = endpoint(&spec, MachineAddApi::ENDPOINT);
    let redeem_endpoint = endpoint(&spec, MachineJoinRedeemApi::ENDPOINT);
    let report_endpoint = endpoint(&spec, MachineJoinReportApi::ENDPOINT);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = start_nats_service(client, &spec)
        .await
        .expect("service starts");

    let add_calls = calls.clone();
    runtime
        .bind_endpoint(&add_endpoint, move |request| {
            let calls = add_calls.clone();
            async move {
                let request: MachineAddRequest =
                    serde_json::from_slice(&request.payload).expect("machine add request decodes");
                calls
                    .lock()
                    .expect("calls lock")
                    .push("machine.add".to_owned());
                assert_eq!(request.operation_id, operation_id("op_init_core_1"));
                assert_eq!(
                    request.idempotency_key,
                    OperationIdempotencyKey::try_new("idem_init_core_1")
                        .expect("valid idempotency key")
                );
                assert_eq!(request.node_id, node_id("core_1"));
                assert_eq!(
                    request.name,
                    MachineName::try_new("core_1").expect("valid machine name")
                );
                assert_eq!(request.gateway, MachineAddGateway::Install);

                let response: MachineAddResponse = OperationApiResponse::Ok {
                    value: MachineAddAccepted {
                        accepted: accepted_operation("op_init_core_1"),
                        node_id: node_id("core_1"),
                        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
                            .expect("valid bootstrap url"),
                        join_token: join_token(),
                    },
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            }
        })
        .await
        .expect("machine add endpoint binds");

    let redeem_calls = calls.clone();
    runtime
        .bind_endpoint(&redeem_endpoint, move |request| {
            let calls = redeem_calls.clone();
            async move {
                let request: MachineJoinRedeemRequest =
                    serde_json::from_slice(&request.payload).expect("redeem request decodes");
                calls
                    .lock()
                    .expect("calls lock")
                    .push("machine.join.redeem".to_owned());
                assert_eq!(request.join_token, join_token());

                let response: MachineJoinRedeemResponse = OperationApiResponse::Ok {
                    value: MachineJoinRedeemed {
                        operation_id: operation_id("op_init_core_1"),
                        node_id: node_id("core_1"),
                        name: MachineName::try_new("core_1").expect("valid machine name"),
                        gateway: ployz_core::roles::FirstNodeGateway::Install,
                        join_bundle: machine_join_bundle(),
                        secret_delivery: machine_join_secret_delivery(),
                        joined_at: ployz_core::machine::JoinTokenRedeemedAt::try_new(120)
                            .expect("valid joined time"),
                        last_event_sequence: event_sequence(2),
                        result: MachineJoinRedeemResult::Joined,
                    },
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            }
        })
        .await
        .expect("redeem endpoint binds");

    let report_calls = calls.clone();
    runtime
        .bind_endpoint(&report_endpoint, move |request| {
            let calls = report_calls.clone();
            async move {
                let request: MachineJoinReportRequest =
                    serde_json::from_slice(&request.payload).expect("report request decodes");
                calls
                    .lock()
                    .expect("calls lock")
                    .push("machine.join.report".to_owned());
                assert_eq!(request.join_token, join_token());
                assert_eq!(request.outcome, MachineJoinReportOutcome::Completed);

                let response: MachineJoinReportResponse = OperationApiResponse::Ok {
                    value: MachineJoinReported {
                        operation_id: operation_id("op_init_core_1"),
                        node_id: node_id("core_1"),
                        last_event_sequence: event_sequence(3),
                        outcome: MachineJoinReportOutcome::Completed,
                    },
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            }
        })
        .await
        .expect("report endpoint binds");
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
    assert_eq!(
        calls.lock().expect("calls lock").as_slice(),
        ["machine.add", "machine.join.redeem", "machine.join.report"]
    );

    runtime.shutdown().await.expect("service shuts down");
}

fn test_api_service(endpoints: Vec<OperationApiEndpoint>) -> NatsServiceSpec {
    NatsServiceSpec::new(
        "plz-api.test",
        "plz-api",
        ServiceVersion::new(0, 1, 0),
        "test API service",
        ServiceMetadata::empty(),
        endpoints
            .into_iter()
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

fn endpoint(spec: &NatsServiceSpec, endpoint: OperationApiEndpoint) -> NatsServiceEndpointSpec {
    spec.endpoints
        .iter()
        .find(|candidate| candidate.subject == endpoint.subject())
        .expect("endpoint exists")
        .clone()
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

fn machine_join_bundle() -> ployz_core::install::MachineJoinBundle {
    ployz_core::install::MachineJoinBundle {
        material: ployz_core::install::MachineJoinMaterial {
            cluster_name: ployz_core::install::MachineJoinClusterName::try_new("prod")
                .expect("valid cluster name"),
            runtime_nats_url: ployz_core::install::MachineJoinRuntimeNatsUrl::try_new(
                "nats://127.0.0.1:7422",
            )
            .expect("valid runtime nats url"),
            trusted_nats: ployz_core::install::MachineJoinTrustedNats {
                server_id: ployz_core::install::MachineJoinTrustedNatsServerId::try_new("nats_1")
                    .expect("valid nats server id"),
                config_sha256: digest(),
            },
            core_iroh: ployz_core::install::MachineJoinCoreIrohEndpoint {
                node_id: node_id("core_1"),
                public_key: ployz_core::install::MachineJoinIrohPublicKey::try_new("iroh_pub")
                    .expect("valid public key"),
                direct_addresses: Vec::new(),
                relay_url: None,
            },
            ployzd: ployz_core::install::MachineJoinPloyzdArtifact {
                version: artifact_version(),
                source: artifact_source("/tmp/ployzd"),
                sha256: digest(),
                install_path: absolute_path("/usr/local/bin/ployzd"),
            },
            ebpf_bytecode: machine_join_artifact(
                "/tmp/ployz-ebpf-tc",
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            ),
            ebpf_ctl: machine_join_artifact("/tmp/ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
        },
    }
}

fn machine_join_artifact(
    source: &str,
    install_path: &str,
) -> ployz_core::install::MachineJoinArtifact {
    ployz_core::install::MachineJoinArtifact {
        version: artifact_version(),
        source: artifact_source(source),
        sha256: digest(),
        install_path: absolute_path(install_path),
    }
}

fn machine_join_secret_delivery() -> ployz_core::install::MachineJoinSecretDelivery {
    ployz_core::install::MachineJoinSecretDelivery {
        nats_credentials: ployz_core::install::MachineJoinNatsCredentials::try_new("creds")
            .expect("valid nats credentials"),
        core_iroh_ticket: ployz_core::install::MachineJoinIrohTicket::try_new("ticket")
            .expect("valid ticket"),
    }
}

fn artifact_version() -> ployz_core::install::InstallArtifactVersion {
    ployz_core::install::InstallArtifactVersion::try_new("0.1.0").expect("valid artifact version")
}

fn artifact_source(value: &str) -> ployz_core::install::InstallArtifactSource {
    ployz_core::install::InstallArtifactSource::try_new(value).expect("valid artifact source")
}

fn absolute_path(value: &str) -> ployz_core::install::AbsoluteInstallPath {
    ployz_core::install::AbsoluteInstallPath::try_new(value).expect("valid absolute path")
}

fn digest() -> ployz_core::install::InstallSha256Digest {
    ployz_core::install::InstallSha256Digest::try_new(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .expect("valid digest")
}

fn join_token() -> MachineJoinToken {
    MachineJoinToken::try_new("join_once_123").expect("valid join token")
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

fn init_with_keeper_run_args(keeper: &str) -> Vec<&str> {
    vec![
        "init",
        "--node",
        "core_1",
        "--gateway",
        "--run-keeper-install",
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

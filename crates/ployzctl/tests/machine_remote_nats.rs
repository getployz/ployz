//! Runtime tests for the remote bootstrap commands (`machine init` and
//! `machine add USER@HOST`) against a stand-in `ssh` program and a real
//! secured NATS test server hosting mocked operation API endpoints.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
    InstallSha256Digest, MachineBootstrapUrl, MachineJoinBundle, MachineJoinClusterName,
    MachineJoinMaterial, MachineJoinRuntimeNatsUrl, MachineJoinTrustedNats,
};
use ployz_core::machine::MachineAddFailure;
use ployz_core::nats_config::NatsCaCertificatePem;
use ployz_core::ops::MachineAddOperationState;
use ployz_core::ops::{
    CoreReplaceOperationState, FailureMessage, OperationEventReplayPage, OperationStatus,
    OperationStatusSnapshot,
};
use ployz_core::roles::{GatewayRole, InstallRolePolicy};
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    AcceptedOperation, CoreReplaceReportRequest, CoreReplaceReportResponse, CoreReplaceReported,
    CoreReplaceRequest, CoreReplaceResponse, InitFirstMachineActivateError,
    InitFirstMachineActivateRequest, InitFirstMachineActivateResponse, InitFirstMachineActivated,
    MachineAddAccepted, MachineAddRequest, MachineAddResponse, MachineJoinToken, MachineName,
    OperationApiResponse, OpsStatusRequest, OpsStatusResponse, OpsWatchResponse,
    operation_api::{
        CoreReplaceApi, CoreReplaceReportApi, InitFirstMachineActivateApi, MachineAddApi,
        OperationApiContract, OpsStatusApi, OpsWatchApi,
    },
};
use ployz_test_support::fs::make_executable;
use ployz_test_support::ids::{event_sequence, machine_id, operation_id};
use ployz_test_support::nats::TestNats;
use ployzctl::bootstrap_command::{
    BootstrapRelease, DEFAULT_BOOTSTRAP_URL, DEFAULT_RELEASE_CHANNEL,
};
use ployzctl::commands::PloyzctlCommand;
use ployzctl::commands::core::{CorePromoteCommand, CoreReplaceCommand};
use ployzctl::commands::machine::{MachineAddRemoteCommand, MachineInitCommand};
use ployzctl::config::{
    ClusterContext, ClusterContextMachine, load_cluster_context, save_cluster_context,
};
use ployzctl::remote_machine_runtime::RemoteMachineExecutionError;
use ployzctl::runtime::{PloyzctlExecutionError, PloyzctlRuntimeConfig, execute_command};
use ployzctl::ssh::SshTarget;
use tokio::sync::Mutex;

const TEST_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TEST_SEED: &str = "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM";
const TEST_CA: &str = "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----";
const FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN: &str = "ployz-first-machine-bootstrap-result begin";
const FIRST_MACHINE_BOOTSTRAP_RESULT_END: &str = "ployz-first-machine-bootstrap-result end";
const CORE_PROMOTE_RESULT_BEGIN: &str = "ployz-core-promote-result begin";
const CORE_PROMOTE_RESULT_END: &str = "ployz-core-promote-result end";
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

/// One stand-in `ssh` that answers the machine-init phase commands and logs
/// every remote command it received.
struct FakeSshMachine {
    _dir: tempfile::TempDir,
    program: PathBuf,
    log: PathBuf,
    stdin_log: PathBuf,
}

struct EnvVarGuard {
    name: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var(name).ok();
        unsafe {
            std::env::set_var(name, value);
        }
        Self { name, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

impl FakeSshMachine {
    /// `hostname` is the reported remote hostname; `installer_exit` shapes
    /// the keeper bootstrap phase outcome.
    fn new(hostname: &str, installer_body: &str) -> Self {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let log = dir.path().join("commands.log");
        let stdin_log = dir.path().join("stdin.log");
        let program = dir.path().join("fake-ssh");
        let promote_result_json = serde_json::to_string(&serde_json::json!({
            "nats_urls": ["tls://203.0.113.20:4222", "tls://[2001:db8::20]:4222"],
            "ca_pem": TEST_CA,
        }))
        .expect("promote result serializes");
        let script = format!(
            r#"#!/bin/sh
cmd=""
for arg in "$@"; do
  cmd="$arg"
done
printf '%s\n----\n' "$cmd" >> {log}
case "$cmd" in
  hostname)
    echo '{hostname}'
    ;;
  'uname -m')
    echo 'x86_64'
    ;;
  'mkdir -p '*)
    :
    ;;
  *'ployz-keeper bootstrap core'* | *'ployz-keeper bootstrap join'*)
    {installer_body}
    ;;
  *'internal-core-demote --successor-nats-url'*)
    echo 'core replaced'
    ;;
  *'ployz-keeper core-promote'*)
    cat > {stdin_log}
    printf '%s\n' \
      '{promote_begin}' \
      '{promote_result_json}' \
      '{promote_end}'
    ;;
  'sudo cat /var/lib/ployz/join-material')
    printf '%s\n' 'machine_id=machine_2' 'cluster_name=test'
    ;;
  'curl -fsSL -- '*)
    printf '%s\n' \
      'PLOYZ_VERSION=0.0.1' \
      'PLOYZD_URL=https://example.invalid/ployzd' \
      'PLOYZD_SHA256={sha}' \
      'PLOYZ_EBPF_TC_URL=https://example.invalid/ployz-ebpf-tc' \
      'PLOYZ_EBPF_TC_SHA256={sha}' \
      'PLOYZ_EBPF_CTL_URL=https://example.invalid/ployz-ebpf-ctl' \
      'PLOYZ_EBPF_CTL_SHA256={sha}'
    ;;
  'set -eu'*)
    echo '{sha}  /usr/local/bin/nats-server'
    ;;
  'cat '*'ca.pem'*)
    printf '%s\n' '-----BEGIN CERTIFICATE-----' 'TUlJQg==' '-----END CERTIFICATE-----'
    ;;
  'cat '*'.seed'*)
    echo '{seed}'
    ;;
  *)
    echo "unexpected remote command: $cmd" >&2
    exit 9
    ;;
esac
"#,
            log = log.display(),
            stdin_log = stdin_log.display(),
            hostname = hostname,
            installer_body = installer_body,
            sha = TEST_SHA,
            seed = TEST_SEED,
            promote_begin = CORE_PROMOTE_RESULT_BEGIN,
            promote_result_json = promote_result_json,
            promote_end = CORE_PROMOTE_RESULT_END,
        );
        fs::write(&program, script).expect("fake ssh writes");
        make_executable(&program);
        Self {
            _dir: dir,
            program,
            log,
            stdin_log,
        }
    }

    fn commands(&self) -> Vec<String> {
        let raw = fs::read_to_string(&self.log).unwrap_or_default();
        raw.split("\n----\n")
            .map(str::to_owned)
            .filter(|entry| !entry.trim().is_empty())
            .collect()
    }

    fn stdin_text(&self) -> String {
        fs::read_to_string(&self.stdin_log).unwrap_or_default()
    }
}

fn first_machine_installer_success_body(machine_id: &str) -> String {
    let result = serde_json::json!({
        "machine_id": machine_id,
        "nats_url": "tls://203.0.113.10:4222",
        "ca_pem": TEST_CA,
        "operator_seed": TEST_SEED,
        "join_seed": TEST_SEED,
    });
    format!(
        "printf '%s\\n' 'keeper install ok' '{}' '{}' '{}'",
        FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN,
        serde_json::to_string(&result).expect("test bootstrap result serializes"),
        FIRST_MACHINE_BOOTSTRAP_RESULT_END,
    )
}

fn machine_init_command(target: &str) -> MachineInitCommand {
    MachineInitCommand {
        target: SshTarget::parse(target).expect("target parses"),
        identity_override: None,
        roles: InstallRolePolicy::install_all(),
        release: BootstrapRelease::Channel(DEFAULT_RELEASE_CHANNEL.to_owned()),
        release_manifest_url: None,
        detach: false,
        bootstrap_url: MachineBootstrapUrl::try_new(DEFAULT_BOOTSTRAP_URL)
            .expect("default bootstrap url is valid"),
        cluster_name: MachineJoinClusterName::try_new("testcluster").expect("valid cluster name"),
        installer_script: None,
        public_ip: None,
    }
}

struct ContextDir {
    _dir: tempfile::TempDir,
    context_path: PathBuf,
}

impl ContextDir {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let context_path = dir.path().join("ployz").join("context.json");
        Self {
            _dir: dir,
            context_path,
        }
    }
}

fn test_config(
    server: &TestNats,
    ssh: &FakeSshMachine,
    context: &ContextDir,
    seed_dir: &Path,
) -> PloyzctlRuntimeConfig {
    let user_seed_file = seed_dir.join("user.seed");
    fs::write(&user_seed_file, server.server.user_seed().secret()).expect("write user seed");
    let join_seed_file = seed_dir.join("join.seed");
    fs::write(&join_seed_file, server.server.join_seed().secret()).expect("write join seed");
    PloyzctlRuntimeConfig {
        nats_url: Some(server.server.client_url().as_str().to_owned()),
        nats_ca_file: Some(server.server.ca_path().to_owned()),
        nats_seed_file: Some(user_seed_file),
        join_seed_file: Some(join_seed_file),
        nats_connect_timeout: None,
        keeper_install_timeout: None,
        ops_watch_timeout: None,
        ops_watch_poll_interval: None,
        ssh_program: Some(ssh.program.clone()),
        ssh_install_timeout: None,
        cluster_context_path: Some(context.context_path.clone()),
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

fn endpoint(spec: &NatsServiceSpec, endpoint: OperationApiEndpoint) -> NatsServiceEndpointSpec {
    spec.endpoints
        .iter()
        .find(|candidate| candidate.name == endpoint.name())
        .expect("test endpoint is present")
        .clone()
}

const fn endpoint_execution(execution: OperationApiEndpointExecution) -> EndpointExecution {
    match execution {
        OperationApiEndpointExecution::AcceptsOperation => EndpointExecution::AcceptsOperation,
        OperationApiEndpointExecution::MutatesOperation => EndpointExecution::MutatesOperation,
        OperationApiEndpointExecution::Query => EndpointExecution::Query,
    }
}

fn accepted_operation(
    operation_id: &ployz_core::ids::OperationId,
    machine_id: &ployz_core::ids::MachineId,
) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: operation_id.clone(),
        watch_subject: format!(
            "plz.v1.progress.machine.{}.operation.{}.>",
            machine_id.as_str(),
            operation_id.as_str()
        ),
        start_sequence: event_sequence(1),
    }
}

fn machine_join_bundle() -> MachineJoinBundle {
    let artifact = |source: &str, install_path: &str| InstallArtifactSpec {
        version: InstallArtifactVersion::try_new("0.0.1").expect("valid version"),
        source: InstallArtifactSource::try_new(source).expect("valid source"),
        sha256: InstallSha256Digest::try_new(TEST_SHA).expect("valid digest"),
        install_path: AbsoluteInstallPath::try_new(install_path).expect("valid path"),
    };
    MachineJoinBundle {
        material: MachineJoinMaterial {
            cluster_name: MachineJoinClusterName::try_new("testcluster")
                .expect("valid cluster name"),
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222")
                .expect("valid runtime nats url"),
            trusted_nats: MachineJoinTrustedNats {
                ca_pem: NatsCaCertificatePem::try_new(format!("{TEST_CA}\n"))
                    .expect("valid ca pem"),
            },
            recovery_key_wrapped: ployz_core::install::WrappedCaKey::new(vec![1, 2, 3]),
            core_seeds_wrapped: ployz_core::install::WrappedCoreSeeds::new(vec![4, 5, 6]),
            ployzd: artifact("https://example.invalid/ployzd", "/usr/local/bin/ployzd"),
            ebpf_bytecode: artifact(
                "https://example.invalid/ployz-ebpf-tc",
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            ),
            ebpf_ctl: artifact(
                "https://example.invalid/ployz-ebpf-ctl",
                "/usr/local/bin/ployz-ebpf-ctl",
            ),
        },
    }
}

fn machine_join_secret_delivery() -> ployz_core::install::MachineJoinSecretDelivery {
    ployz_core::install::MachineJoinSecretDelivery {
        nats_credentials: ployz_core::nats_config::NatsUserSeed::try_new(TEST_SEED)
            .expect("valid nats credentials"),
    }
}

// ---------------------------------------------------------------------------
// core promote USER@HOST / core demote USER@HOST
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn core_promote_remote_runs_keeper_command_and_updates_context() {
    let _env_lock = ENV_LOCK.lock().await;
    let _secret = EnvVarGuard::set("PLOYZ_RECOVERY_SECRET", "recover-secret");

    let server = TestNats::start().await;
    let ssh = FakeSshMachine::new("edge-2", "echo never");
    let context = ContextDir::new();
    let seed_dir = tempfile::TempDir::new().expect("seed dir");
    let config = test_config(&server, &ssh, &context, seed_dir.path());
    save_cluster_context(
        &context.context_path,
        &ClusterContext {
            nats_url: server.server.client_url().clone(),
            nats_ca_file: server.server.ca_path().to_owned(),
            operator_seed_file: config
                .nats_seed_file
                .clone()
                .expect("test config has operator seed"),
            join_seed_file: config.join_seed_file.clone(),
            machines: Vec::new(),
        },
    )
    .expect("cluster context saves");

    let output = execute_command(
        PloyzctlCommand::CorePromote(CorePromoteCommand {
            target: SshTarget::parse("root@203.0.113.20").expect("target parses"),
        }),
        &config,
    )
    .await
    .expect("remote core promote succeeds");

    assert_eq!(
        output.stdout,
        "core promoted on root@203.0.113.20\nnats tls://203.0.113.20:4222,tls://[2001:db8::20]:4222\nca-pem printed by remote keeper\n"
    );
    assert!(output.stderr.contains("context "));
    assert!(
        output
            .stderr
            .contains("now points at tls://203.0.113.20:4222")
    );
    assert_eq!(
        ssh.commands(),
        vec![
            "sudo sh -c 'IFS= read -r PLOYZ_RECOVERY_SECRET; export PLOYZ_RECOVERY_SECRET; exec ployz-keeper core-promote'"
        ]
    );
    assert_eq!(ssh.stdin_text(), "recover-secret\n");
    let saved = load_cluster_context(&context.context_path)
        .expect("context reads")
        .expect("context exists");
    assert_eq!(saved.nats_url.as_str(), "tls://203.0.113.20:4222");
}

#[tokio::test(flavor = "multi_thread")]
async fn core_replace_remote_runs_keeper_command() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[
        CoreReplaceApi::ENDPOINT,
        CoreReplaceReportApi::ENDPOINT,
        OpsWatchApi::ENDPOINT,
        OpsStatusApi::ENDPOINT,
    ]);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");
    let expected_successor_url = server.server.client_url().clone();
    runtime
        .bind_endpoint(&endpoint(&spec, CoreReplaceApi::ENDPOINT), move |request| {
            let expected_successor_url = expected_successor_url.clone();
            async move {
                let request: CoreReplaceRequest =
                    serde_json::from_slice(&request.payload).expect("core replace decodes");
                assert_eq!(request.machine_id, machine_id("machine_2"));
                assert!(
                    request
                        .operation_id
                        .as_str()
                        .starts_with("op_core_replace_machine_2_")
                );
                assert_eq!(
                    request.successor_nats_url.as_str(),
                    expected_successor_url.as_str()
                );
                let response: CoreReplaceResponse = OperationApiResponse::Ok {
                    value: accepted_operation(&request.operation_id, &request.machine_id),
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            }
        })
        .await
        .expect("core replace endpoint binds");
    runtime
        .bind_endpoint(
            &endpoint(&spec, CoreReplaceReportApi::ENDPOINT),
            |request| async move {
                let request: CoreReplaceReportRequest =
                    serde_json::from_slice(&request.payload).expect("core replace report decodes");
                assert_eq!(request.machine_id, machine_id("machine_2"));
                let response: CoreReplaceReportResponse = OperationApiResponse::Ok {
                    value: CoreReplaceReported {
                        operation_id: request.operation_id,
                        machine_id: request.machine_id,
                        last_event_sequence: event_sequence(2),
                        outcome: request.outcome,
                    },
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("core replace report endpoint binds");
    runtime
        .bind_endpoint(
            &endpoint(&spec, OpsWatchApi::ENDPOINT),
            |_request| async move {
                let response: OpsWatchResponse = OperationApiResponse::Ok {
                    value: OperationEventReplayPage::terminal(Vec::new()),
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("ops watch endpoint binds");
    runtime
        .bind_endpoint(
            &endpoint(&spec, OpsStatusApi::ENDPOINT),
            |request| async move {
                let request: OpsStatusRequest =
                    serde_json::from_slice(&request.payload).expect("status request decodes");
                let response: OpsStatusResponse = OperationApiResponse::Ok {
                    value: OperationStatusSnapshot::new(OperationStatus::CoreReplace {
                        id: request.operation_id,
                        machine_id: machine_id("machine_2"),
                        successor_nats_url: MachineJoinRuntimeNatsUrl::try_new(
                            "tls://203.0.113.20:4222",
                        )
                        .expect("valid URL"),
                        state: CoreReplaceOperationState::Completed,
                        last_event_sequence: event_sequence(2),
                    }),
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("ops status endpoint binds");
    client.flush().await.expect("service flushes");

    let ssh = FakeSshMachine::new("stale-core", "echo never");
    let context = ContextDir::new();
    let seed_dir = tempfile::TempDir::new().expect("seed dir");
    let config = test_config(&server, &ssh, &context, seed_dir.path());
    save_cluster_context(
        &context.context_path,
        &ClusterContext {
            nats_url: server.server.client_url().clone(),
            nats_ca_file: server.server.ca_path().to_owned(),
            operator_seed_file: config
                .nats_seed_file
                .clone()
                .expect("test config has operator seed"),
            join_seed_file: config.join_seed_file.clone(),
            machines: vec![
                ClusterContextMachine {
                    machine_id: machine_id("machine_2"),
                    ssh: Some(SshTarget::parse("root@203.0.113.10").expect("target parses")),
                },
                ClusterContextMachine {
                    machine_id: machine_id("machine_1"),
                    ssh: Some(SshTarget::parse("root@203.0.113.11").expect("target parses")),
                },
                ClusterContextMachine {
                    machine_id: machine_id("machine_3"),
                    ssh: Some(SshTarget::parse("root@203.0.113.12").expect("target parses")),
                },
            ],
        },
    )
    .expect("cluster context saves");

    let output = execute_command(
        PloyzctlCommand::CoreReplace(CoreReplaceCommand {
            target: SshTarget::parse("root@203.0.113.10").expect("target parses"),
        }),
        &config,
    )
    .await
    .expect("remote core replace succeeds");

    assert_eq!(output.stdout, "core demoted on root@203.0.113.10\n");
    assert_eq!(
        ssh.commands(),
        vec![
            "sudo cat /var/lib/ployz/join-material".to_owned(),
            format!(
                "sudo ployz-keeper internal-core-demote --successor-nats-url '{}'",
                server.server.client_url().as_str()
            ),
        ]
    );
}

// ---------------------------------------------------------------------------
// machine init (U4)
// ---------------------------------------------------------------------------

/// Successful init: spec built from the remote hostname with gateway and DNS
/// installed, installer driven over SSH, operator material collected, local
/// context written, and the first machine activated through the existing API.
#[tokio::test(flavor = "multi_thread")]
async fn machine_init_installs_activates_and_writes_local_context() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[InitFirstMachineActivateApi::ENDPOINT]);
    let activate_endpoint = endpoint(&spec, InitFirstMachineActivateApi::ENDPOINT);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");
    runtime
        .bind_endpoint(&activate_endpoint, |request| async move {
            let request: InitFirstMachineActivateRequest =
                serde_json::from_slice(&request.payload).expect("activate request decodes");
            assert_eq!(request.machine_id, machine_id("sg-core-1"));
            assert_eq!(request.roles.gateway, GatewayRole::Install);
            let response: InitFirstMachineActivateResponse = OperationApiResponse::Ok {
                value: InitFirstMachineActivated {
                    operation_id: operation_id("op_first_machine"),
                    machine_id: machine_id("sg-core-1"),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    client.flush().await.expect("service flushes");

    let first_machine_success = first_machine_installer_success_body("sg-core-1");
    let ssh = FakeSshMachine::new("sg-core-1", &first_machine_success);
    let context = ContextDir::new();
    let seed_dir = tempfile::TempDir::new().expect("seed dir");
    let config = test_config(&server, &ssh, &context, seed_dir.path());
    let output = execute_command(
        PloyzctlCommand::MachineInit(machine_init_command("root@203.0.113.10")),
        &config,
    )
    .await
    .expect("machine init succeeds");

    assert!(output.stdout.contains("operation op_first_machine"));
    assert!(output.stdout.contains("first-machine sg-core-1 active"));
    assert!(output.stdout.contains("context "));

    // The local cluster context records the remote cluster and the
    // collected material files (KTD5).
    let loaded = load_cluster_context(&context.context_path)
        .expect("context loads")
        .expect("context exists");
    assert_eq!(loaded.nats_url.as_str(), "tls://203.0.113.10:4222");
    let ca = fs::read_to_string(&loaded.nats_ca_file).expect("collected ca reads");
    assert!(ca.contains("BEGIN CERTIFICATE"));
    let operator_seed =
        fs::read_to_string(&loaded.operator_seed_file).expect("collected operator seed reads");
    assert_eq!(operator_seed.trim(), TEST_SEED);
    let join_seed_file = loaded.join_seed_file.expect("join seed file recorded");
    assert_eq!(
        fs::read_to_string(join_seed_file)
            .expect("collected join seed reads")
            .trim(),
        TEST_SEED
    );
    let [machine] = loaded.machines.as_slice() else {
        panic!("expected one recorded machine");
    };
    assert_eq!(machine.machine_id, machine_id("sg-core-1"));
    assert_eq!(
        machine.ssh.as_ref().map(|target| target.destination()),
        Some("root@203.0.113.10".to_owned())
    );

    // Founder bootstrap is one rendered command delivered over SSH. The
    // script/keeper side owns release resolution, local artifacts, and the
    // first-machine spec.
    let commands = ssh.commands();
    let install = commands
        .iter()
        .find(|command| command.contains("ployz-keeper bootstrap core"))
        .expect("founder installer ran remotely");
    for expected in [
        "curl -fsSL -- 'https://ployz.sh'",
        "PLOYZ_MACHINE_ID='sg-core-1'",
        "PLOYZ_CHANNEL='alpha'",
        "PLOYZ_GATEWAY='install'",
        "PLOYZ_DNS='install'",
        "PLOYZ_MACHINE_BOOTSTRAP_URL='https://ployz.sh'",
        "PLOYZ_MACHINE_JOIN_CLUSTER_NAME='testcluster'",
        "PLOYZ_MACHINE_JOIN_NATS_URL='tls://203.0.113.10:4222'",
        "ployz-keeper bootstrap core",
    ] {
        assert!(
            install.contains(expected),
            "install missing {expected}: {install}"
        );
    }
    assert!(!install.contains("PLOYZ_MACHINE_PUBLIC_IP="));
    assert!(
        commands
            .iter()
            .all(|command| !command.contains("first-machine-install.json"))
    );
    assert!(
        commands
            .iter()
            .all(|command| !command.contains("machine-join-template.json"))
    );
    assert!(commands.iter().all(|command| !command.starts_with("cat ")));
    assert!(
        commands
            .iter()
            .all(|command| command != "systemctl restart ployzd-control")
    );
}

/// A failed remote installer exits with the SSH phase and the remote output
/// as evidence, before any local context is written (R14).
#[tokio::test(flavor = "multi_thread")]
async fn machine_init_installer_failure_names_phase_and_output() {
    let server = TestNats::start().await;
    let ssh = FakeSshMachine::new("sg-core-1", "echo 'keeper exploded' >&2\nexit 1");
    let context = ContextDir::new();
    let seed_dir = tempfile::TempDir::new().expect("seed dir");
    let config = test_config(&server, &ssh, &context, seed_dir.path());

    let error = execute_command(
        PloyzctlCommand::MachineInit(machine_init_command("root@203.0.113.10")),
        &config,
    )
    .await
    .expect_err("failed installer fails machine init");

    let rendered = error.to_string();
    assert!(rendered.contains("run-installer"), "{rendered}");
    assert!(rendered.contains("keeper exploded"), "{rendered}");
    assert!(
        !context.context_path.exists(),
        "no cluster context may be recorded for a failed init"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn machine_init_activation_failure_does_not_record_machine_ssh() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[InitFirstMachineActivateApi::ENDPOINT]);
    let activate_endpoint = endpoint(&spec, InitFirstMachineActivateApi::ENDPOINT);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");
    runtime
        .bind_endpoint(&activate_endpoint, |_request| async move {
            let response: InitFirstMachineActivateResponse = OperationApiResponse::DomainError {
                error: InitFirstMachineActivateError::InvalidPlan,
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    client.flush().await.expect("service flushes");

    let first_machine_success = first_machine_installer_success_body("sg-core-1");
    let ssh = FakeSshMachine::new("sg-core-1", &first_machine_success);
    let context = ContextDir::new();
    let seed_dir = tempfile::TempDir::new().expect("seed dir");
    let config = test_config(&server, &ssh, &context, seed_dir.path());

    execute_command(
        PloyzctlCommand::MachineInit(machine_init_command("root@203.0.113.10")),
        &config,
    )
    .await
    .expect_err("activation failure fails machine init");

    let loaded = load_cluster_context(&context.context_path)
        .expect("context loads")
        .expect("context exists");
    assert!(
        loaded.machines.is_empty(),
        "machine ssh mapping is saved only after activation succeeds"
    );
}

/// An invalid remote hostname fails before any install work and points at
/// `--name` (R6, AE3).
#[tokio::test(flavor = "multi_thread")]
async fn machine_init_invalid_hostname_fails_before_install() {
    let server = TestNats::start().await;
    let ssh = FakeSshMachine::new("sg.core.1", "echo never");
    let context = ContextDir::new();
    let seed_dir = tempfile::TempDir::new().expect("seed dir");
    let config = test_config(&server, &ssh, &context, seed_dir.path());

    let error = execute_command(
        PloyzctlCommand::MachineInit(machine_init_command("root@203.0.113.10")),
        &config,
    )
    .await
    .expect_err("invalid hostname fails machine init");

    assert!(matches!(
        error,
        PloyzctlExecutionError::RemoteMachine {
            ref source
        } if matches!(&**source, RemoteMachineExecutionError::MachineIdentity { .. })
    ));
    let rendered = error.to_string();
    assert!(rendered.contains("sg.core.1"));
    assert!(rendered.contains("--name"));
    assert_eq!(
        ssh.commands(),
        vec!["hostname".to_owned()],
        "no remote work may run after the identity fails"
    );
}

// ---------------------------------------------------------------------------
// machine add USER@HOST (U6)
// ---------------------------------------------------------------------------

/// Remote add: identity from the remote hostname, the normal machine-add
/// operation submitted with client-generated ids, the join-mode installer
/// driven over SSH with the join bundle, and the operation watched to
/// completion.
#[tokio::test(flavor = "multi_thread")]
async fn machine_add_remote_submits_installs_and_watches_to_completion() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[
        MachineAddApi::ENDPOINT,
        OpsWatchApi::ENDPOINT,
        OpsStatusApi::ENDPOINT,
    ]);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(
            &endpoint(&spec, MachineAddApi::ENDPOINT),
            |request| async move {
                let request: MachineAddRequest =
                    serde_json::from_slice(&request.payload).expect("machine add request decodes");
                // Identity derives from the remote hostname (R9); the gateway
                // defaults to install (R8); ids are client-generated (KTD9).
                assert_eq!(request.machine_id, machine_id("sg-edge-1"));
                assert_eq!(
                    request.name,
                    MachineName::try_new("sg-edge-1").expect("valid machine name")
                );
                assert_eq!(request.roles.gateway, GatewayRole::Install);
                assert!(
                    request
                        .operation_id
                        .as_str()
                        .starts_with("op_add_sg-edge-1_")
                );
                assert!(
                    request
                        .idempotency_key
                        .as_str()
                        .starts_with("idem_add_sg-edge-1_")
                );

                let response: MachineAddResponse = OperationApiResponse::Ok {
                    value: MachineAddAccepted {
                        accepted: accepted_operation(&request.operation_id, &request.machine_id),
                        machine_id: request.machine_id,
                        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
                            .expect("valid bootstrap url"),
                        join_bundle: machine_join_bundle(),
                        join_token: MachineJoinToken::try_new("join_once_123")
                            .expect("valid join token"),
                        join_secret_delivery: machine_join_secret_delivery(),
                    },
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("machine add endpoint binds");
    runtime
        .bind_endpoint(
            &endpoint(&spec, OpsWatchApi::ENDPOINT),
            |_request| async move {
                let response: OpsWatchResponse = OperationApiResponse::Ok {
                    value: OperationEventReplayPage::terminal(Vec::new()),
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("ops watch endpoint binds");
    runtime
        .bind_endpoint(
            &endpoint(&spec, OpsStatusApi::ENDPOINT),
            |request| async move {
                let request: OpsStatusRequest =
                    serde_json::from_slice(&request.payload).expect("status request decodes");
                let response: OpsStatusResponse = OperationApiResponse::Ok {
                    value: OperationStatusSnapshot::new(OperationStatus::MachineAdd {
                        id: request.operation_id,
                        machine_id: machine_id("sg-edge-1"),
                        name: MachineName::try_new("sg-edge-1").expect("valid machine name"),
                        roles: InstallRolePolicy::install_all(),
                        state: MachineAddOperationState::Completed,
                        last_event_sequence: event_sequence(5),
                    }),
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("ops status endpoint binds");
    client.flush().await.expect("service flushes");

    let ssh = FakeSshMachine::new("sg-edge-1", "echo 'join ok'");
    let context = ContextDir::new();
    let seed_dir = tempfile::TempDir::new().expect("seed dir");
    let config = test_config(&server, &ssh, &context, seed_dir.path());
    save_cluster_context(
        &context.context_path,
        &ClusterContext {
            nats_url: server.server.client_url().clone(),
            nats_ca_file: server.server.ca_path().to_owned(),
            operator_seed_file: config
                .nats_seed_file
                .clone()
                .expect("test config has operator seed"),
            join_seed_file: config.join_seed_file.clone(),
            machines: Vec::new(),
        },
    )
    .expect("cluster context saves");

    let output = execute_command(
        PloyzctlCommand::MachineAddRemote(MachineAddRemoteCommand {
            target: SshTarget::parse("root@203.0.113.11").expect("target parses"),
            identity_override: None,
            roles: InstallRolePolicy::install_all(),
            detach: false,
            installer_script: None,
        }),
        &config,
    )
    .await
    .expect("remote machine add succeeds");

    assert!(output.stdout.contains("operation op_add_sg-edge-1_"));
    assert!(output.stdout.contains("machine sg-edge-1"));
    assert!(output.stdout.contains("machine-add completed"));
    let loaded = load_cluster_context(&context.context_path)
        .expect("context loads")
        .expect("context exists");
    let [machine] = loaded.machines.as_slice() else {
        panic!("expected one recorded machine");
    };
    assert_eq!(machine.machine_id, machine_id("sg-edge-1"));
    assert_eq!(
        machine.ssh.as_ref().map(|target| target.destination()),
        Some("root@203.0.113.11".to_owned())
    );

    // The remote machine ran the join-mode installer with the join bundle
    // material and its own public IP.
    let commands = ssh.commands();
    let install = commands
        .iter()
        .find(|command| command.contains("ployz-keeper bootstrap join"))
        .expect("join installer ran remotely");
    for expected in [
        "curl -fsSL -- 'https://get.ployz.sh'",
        "PLOYZ_NATS_URL='tls://203.0.113.10:4222'",
        "PLOYZ_JOIN_NKEY_SEED=",
        "ployz-keeper bootstrap join 'join_once_123'",
    ] {
        assert!(
            install.contains(expected),
            "install missing {expected}: {install}"
        );
    }
    assert!(!install.contains("PLOYZ_MACHINE_PUBLIC_IP="));
}

/// Installer failure after token redemption keeps the operation id visible
/// next to the SSH phase and remote output (R14); the operation itself stays
/// on the server as evidence.
#[tokio::test(flavor = "multi_thread")]
async fn machine_add_remote_installer_failure_carries_operation_and_phase() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[MachineAddApi::ENDPOINT]);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");
    runtime
        .bind_endpoint(
            &endpoint(&spec, MachineAddApi::ENDPOINT),
            |request| async move {
                let request: MachineAddRequest =
                    serde_json::from_slice(&request.payload).expect("machine add request decodes");
                let response: MachineAddResponse = OperationApiResponse::Ok {
                    value: MachineAddAccepted {
                        accepted: accepted_operation(&request.operation_id, &request.machine_id),
                        machine_id: request.machine_id,
                        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
                            .expect("valid bootstrap url"),
                        join_bundle: machine_join_bundle(),
                        join_token: MachineJoinToken::try_new("join_once_123")
                            .expect("valid join token"),
                        join_secret_delivery: machine_join_secret_delivery(),
                    },
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("machine add endpoint binds");
    client.flush().await.expect("service flushes");

    let ssh = FakeSshMachine::new("sg-edge-1", "echo 'keeper join exploded' >&2\nexit 1");
    let context = ContextDir::new();
    let seed_dir = tempfile::TempDir::new().expect("seed dir");
    let config = test_config(&server, &ssh, &context, seed_dir.path());
    save_cluster_context(
        &context.context_path,
        &ClusterContext {
            nats_url: server.server.client_url().clone(),
            nats_ca_file: server.server.ca_path().to_owned(),
            operator_seed_file: config
                .nats_seed_file
                .clone()
                .expect("test config has operator seed"),
            join_seed_file: config.join_seed_file.clone(),
            machines: Vec::new(),
        },
    )
    .expect("cluster context saves");

    let error = execute_command(
        PloyzctlCommand::MachineAddRemote(MachineAddRemoteCommand {
            target: SshTarget::parse("root@203.0.113.11").expect("target parses"),
            identity_override: None,
            roles: InstallRolePolicy::install_all(),
            detach: false,
            installer_script: None,
        }),
        &config,
    )
    .await
    .expect_err("failed join installer fails remote machine add");

    let PloyzctlExecutionError::RemoteMachine { source } = &error else {
        panic!("expected remote machine error, got {error:?}");
    };
    let RemoteMachineExecutionError::RemoteJoinInstall { operation_id, .. } = &**source else {
        panic!("expected remote join install error, got {error:?}");
    };
    assert!(operation_id.as_str().starts_with("op_add_sg-edge-1_"));
    let rendered = error.to_string();
    assert!(
        rendered.contains("machine add operation op_add_sg-edge-1_"),
        "{rendered}"
    );
    assert!(rendered.contains("run-installer"), "{rendered}");
    assert!(rendered.contains("keeper join exploded"), "{rendered}");
    let loaded = load_cluster_context(&context.context_path)
        .expect("context loads")
        .expect("context exists");
    assert!(
        loaded.machines.is_empty(),
        "failed remote join must not persist machine ssh context"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn machine_add_remote_terminal_failure_does_not_record_machine_ssh() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[
        MachineAddApi::ENDPOINT,
        OpsWatchApi::ENDPOINT,
        OpsStatusApi::ENDPOINT,
    ]);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(
            &endpoint(&spec, MachineAddApi::ENDPOINT),
            |request| async move {
                let request: MachineAddRequest =
                    serde_json::from_slice(&request.payload).expect("machine add request decodes");
                let response: MachineAddResponse = OperationApiResponse::Ok {
                    value: MachineAddAccepted {
                        accepted: accepted_operation(&request.operation_id, &request.machine_id),
                        machine_id: request.machine_id,
                        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
                            .expect("valid bootstrap url"),
                        join_bundle: machine_join_bundle(),
                        join_token: MachineJoinToken::try_new("join_once_123")
                            .expect("valid join token"),
                        join_secret_delivery: machine_join_secret_delivery(),
                    },
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("machine add endpoint binds");
    runtime
        .bind_endpoint(
            &endpoint(&spec, OpsWatchApi::ENDPOINT),
            |_request| async move {
                let response: OpsWatchResponse = OperationApiResponse::Ok {
                    value: OperationEventReplayPage::terminal(Vec::new()),
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("ops watch endpoint binds");
    runtime
        .bind_endpoint(
            &endpoint(&spec, OpsStatusApi::ENDPOINT),
            |request| async move {
                let request: OpsStatusRequest =
                    serde_json::from_slice(&request.payload).expect("status request decodes");
                let response: OpsStatusResponse = OperationApiResponse::Ok {
                    value: OperationStatusSnapshot::new(OperationStatus::MachineAdd {
                        id: request.operation_id,
                        machine_id: machine_id("sg-edge-1"),
                        name: MachineName::try_new("sg-edge-1").expect("valid machine name"),
                        roles: InstallRolePolicy::install_all(),
                        state: MachineAddOperationState::Failed {
                            failure: MachineAddFailure::BootstrapFailed {
                                message: FailureMessage::try_new("join failed")
                                    .expect("valid failure message"),
                            },
                        },
                        last_event_sequence: event_sequence(5),
                    }),
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("ops status endpoint binds");
    client.flush().await.expect("service flushes");

    let ssh = FakeSshMachine::new("sg-edge-1", "echo 'join ok'");
    let context = ContextDir::new();
    let seed_dir = tempfile::TempDir::new().expect("seed dir");
    let config = test_config(&server, &ssh, &context, seed_dir.path());
    save_cluster_context(
        &context.context_path,
        &ClusterContext {
            nats_url: server.server.client_url().clone(),
            nats_ca_file: server.server.ca_path().to_owned(),
            operator_seed_file: config
                .nats_seed_file
                .clone()
                .expect("test config has operator seed"),
            join_seed_file: config.join_seed_file.clone(),
            machines: Vec::new(),
        },
    )
    .expect("cluster context saves");

    let error = execute_command(
        PloyzctlCommand::MachineAddRemote(MachineAddRemoteCommand {
            target: SshTarget::parse("root@203.0.113.11").expect("target parses"),
            identity_override: None,
            roles: InstallRolePolicy::install_all(),
            detach: false,
            installer_script: None,
        }),
        &config,
    )
    .await
    .expect_err("failed terminal status fails remote machine add");

    let rendered = error.to_string();
    assert!(rendered.contains("machine add operation op_add_sg-edge-1_"));
    assert!(rendered.contains("Failed"), "{rendered}");
    let loaded = load_cluster_context(&context.context_path)
        .expect("context loads")
        .expect("context exists");
    assert!(
        loaded.machines.is_empty(),
        "failed terminal operation must not persist machine ssh context"
    );
}

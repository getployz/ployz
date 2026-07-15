//! Runtime tests for the remote bootstrap commands (`machine init` and
//! `machine add USER@HOST`) against a stand-in `ssh` program and a real
//! secured NATS test server hosting mocked operation API endpoints.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use ployz::commands::PloyzctlCommand;
use ployz::dispatcher::{PloyzctlExecutionError, PloyzctlRuntimeConfig, execute_command};
use ployz::machine::bootstrap::{BootstrapRelease, DEFAULT_BOOTSTRAP_URL, DEFAULT_RELEASE_CHANNEL};
use ployz::machine::command::{MachineAddRemoteCommand, MachineInitCommand};
use ployz::machine::local_release::LocalReleaseBundle;
use ployz::machine::operator_context::{
    ClusterContext, load_cluster_context, save_cluster_context,
};
use ployz::machine::runtime::{MachineExecutionError, remote::RemoteMachineExecutionError};
use ployz::ssh::SshTarget;
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
    InstallSha256Digest, MachineBootstrapUrl, MachineJoinBundle, MachineJoinClusterName,
    MachineJoinMaterial, MachineJoinRuntimeNatsUrl, MachineJoinTrustedNats,
};
use ployz_core::machine::MachineAddFailure;
use ployz_core::nats_config::NatsCaCertificatePem;
use ployz_core::operation::MachineAddOperationState;
use ployz_core::operation::{
    FailureMessage, OperationEventReplayPage, OperationStatus, OperationStatusSnapshot,
};
use ployz_core::roles::{GatewayRole, InstallRolePolicy};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_nats::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_sdk_types::{
    AcceptedOperation, InitFirstMachineActivateError, InitFirstMachineActivateRequest,
    InitFirstMachineActivateResponse, InitFirstMachineActivated, MachineAddAccepted,
    MachineAddRequest, MachineAddResponse, MachineJoinToken, MachineName, OperationApiResponse,
    OpsStatusRequest, OpsStatusResponse, OpsWatchResponse,
    operation_api::{
        InitFirstMachineActivateApi, MachineAddApi, OperationApiContract, OpsStatusApi, OpsWatchApi,
    },
};
use ployz_test_support::fs::make_executable;
use ployz_test_support::ids::{event_sequence, machine_id, operation_id};
use ployz_test_support::nats::TestNats;
use sha2::{Digest as _, Sha256};

const TEST_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TEST_SEED: &str = "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM";
const TEST_CA: &str = "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----";
const FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN: &str = "ployz-first-machine-bootstrap-result begin";
const FIRST_MACHINE_BOOTSTRAP_RESULT_END: &str = "ployz-first-machine-bootstrap-result end";

/// One stand-in `ssh` that answers the machine-init phase commands and logs
/// every remote command it received.
struct FakeSshMachine {
    _dir: tempfile::TempDir,
    program: PathBuf,
    log: PathBuf,
    stdin_log: PathBuf,
}

impl FakeSshMachine {
    /// `hostname` is the reported remote hostname; `installer_exit` shapes
    /// the Host Runner bootstrap phase outcome.
    fn new(hostname: &str, installer_body: &str) -> Self {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let log = dir.path().join("commands.log");
        let stdin_log = dir.path().join("stdin.log");
        let program = dir.path().join("fake-ssh");
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
  *'run tar -xf - -C'*)
    cat > {stdin_log}
    ;;
  *'ployz host bootstrap core'* | *'ployz host bootstrap join'*)
    {installer_body}
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
        "printf '%s\\n' 'Host Runner install ok' '{}' '{}' '{}'",
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
        local_release: None,
        public_ip: None,
        automatic_hostname_configuration:
            ployz_core::ingress::AutomaticHostnameConfiguration::Ployz,
        ployz_dns_target: ployz_core::ingress::PloyzDnsTargetIntent::Enabled,
        host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
    }
}

fn local_release_bundle(root: &Path) -> (LocalReleaseBundle, String) {
    let staging = root.join("staging");
    fs::create_dir(&staging).expect("staging dir");
    let artifacts = [
        ("ployz", "PLOYZ"),
        ("ployzd", "PLOYZD"),
        ("ployz-ebpf-ctl", "PLOYZ_EBPF_CTL"),
        ("ployz-ebpf-tc", "PLOYZ_EBPF_TC"),
    ]
    .map(|(name, key)| {
        let contents = format!("local {name}");
        fs::write(staging.join(name), &contents).expect("artifact writes");
        (name, key, format!("{:x}", Sha256::digest(contents)))
    });
    let ployz_script = "#!/bin/sh\necho local\n";
    fs::write(staging.join("ployz.sh"), ployz_script).expect("ployz.sh writes");
    let platform = "linux-amd64";
    let nats_version = "2.14.2";
    let nats_url = "https://example.invalid/nats.tar.gz";
    let nats_sha256 = "a".repeat(64);
    let mut identity = Sha256::new();
    for (name, _, sha256) in &artifacts {
        identity.update(format!("{name} {sha256}\n"));
    }
    identity.update(format!(
        "ployz.sh {:x}\nplatform {platform}\nnats-version {nats_version}\nnats-url {nats_url}\nnats-sha256 {nats_sha256}\n",
        Sha256::digest(ployz_script)
    ));
    let version = format!("dev-{}", &format!("{:x}", identity.finalize())[..16]);
    let directory = root.join(&version);
    fs::rename(staging, &directory).expect("bundle is named by version");
    let remote = format!("/var/lib/ployz/dev-releases/{version}");
    fs::write(
        directory.join("install.sh"),
        format!(
            "#!/bin/sh\nset -eu\nPLOYZ_RELEASE_MANIFEST_URL=file://{remote}/release.env exec {remote}/ployz.sh \"$@\"\n"
        ),
    )
    .expect("install wrapper writes");
    let mut manifest = format!(
        "PLOYZ_VERSION={version}\nPLOYZ_RELEASE_TAG={version}\nPLOYZ_RELEASE_PLATFORM={platform}\n"
    );
    for (name, key, sha256) in artifacts {
        manifest.push_str(&format!(
            "{key}_URL={remote}/{name}\n{key}_SHA256={sha256}\n"
        ));
    }
    manifest.push_str(&format!(
        "PLOYZ_NATS_SERVER_VERSION={nats_version}\nPLOYZ_NATS_SERVER_URL={nats_url}\nPLOYZ_NATS_SERVER_SHA256={nats_sha256}\n"
    ));
    fs::write(directory.join("release.env"), manifest).expect("manifest writes");

    (
        LocalReleaseBundle::open(directory).expect("valid local release"),
        version,
    )
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
        host_runner_install_timeout: None,
        ops_watch_timeout: None,
        ops_watch_poll_interval: None,
        ssh_program: Some(ssh.program.clone()),
        ssh_install_timeout: None,
        cluster_context_path: Some(context.context_path.clone()),
        deploy_history_root: None,
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
            dataplane_endpoint_supernet: ployz_core::network::MachineEndpointSupernet::default_v1(),
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
// machine init (U4)
// ---------------------------------------------------------------------------

/// Successful init: spec built from the remote hostname with gateway and DNS
/// installed, installer driven over SSH, operator material collected, local
/// context written, and the first machine activated through the existing API.
#[tokio::test(flavor = "multi_thread")]
async fn machine_init_installs_activates_and_writes_local_context() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[OperationApiEndpoint::from(
        InitFirstMachineActivateApi::ENDPOINT,
    )]);
    let activate_endpoint = endpoint(
        &spec,
        OperationApiEndpoint::from(InitFirstMachineActivateApi::ENDPOINT),
    );
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
    // script/Host Runner side owns release resolution, local artifacts, and the
    // first-machine spec.
    let commands = ssh.commands();
    let install = commands
        .iter()
        .find(|command| command.contains("ployz host bootstrap core"))
        .expect("founder installer ran remotely");
    for expected in [
        "curl -fsSL -- 'https://ployz.sh'",
        "PLOYZ_MACHINE_ID='sg-core-1'",
        "PLOYZ_CHANNEL='alpha'",
        "PLOYZ_GATEWAY='install'",
        "PLOYZ_MACHINE_BOOTSTRAP_URL='https://ployz.sh'",
        "PLOYZ_MACHINE_JOIN_CLUSTER_NAME='testcluster'",
        "PLOYZ_MACHINE_JOIN_NATS_URL='tls://203.0.113.10:4222'",
        "ployz host bootstrap core",
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

#[tokio::test(flavor = "multi_thread")]
async fn machine_init_stages_and_installs_a_local_release() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[OperationApiEndpoint::from(
        InitFirstMachineActivateApi::ENDPOINT,
    )]);
    let activate_endpoint = endpoint(
        &spec,
        OperationApiEndpoint::from(InitFirstMachineActivateApi::ENDPOINT),
    );
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");
    runtime
        .bind_endpoint(&activate_endpoint, |_request| async move {
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
    let release_dir = tempfile::TempDir::new().expect("release dir");
    let (bundle, version) = local_release_bundle(release_dir.path());
    let config = test_config(&server, &ssh, &context, seed_dir.path());
    let mut command = machine_init_command("root@203.0.113.10");
    command.local_release = Some(Box::new(bundle));

    execute_command(PloyzctlCommand::MachineInit(command), &config)
        .await
        .expect("machine init succeeds");

    let remote = format!("/var/lib/ployz/dev-releases/{version}");
    let commands = ssh.commands();
    assert!(
        commands
            .iter()
            .any(|command| command.contains("run tar -xf - -C")),
        "local release was not staged: {commands:?}"
    );
    let archive = fs::read(&ssh.stdin_log).expect("staged archive reached ssh stdin");
    for expected in [
        "ployz",
        "ployzd",
        "install.sh",
        "release.env",
        "local ployzd",
    ] {
        assert!(
            archive
                .windows(expected.len())
                .any(|bytes| bytes == expected.as_bytes()),
            "archive missing {expected}"
        );
    }
    let install = commands
        .iter()
        .find(|command| command.contains("ployz host bootstrap core"))
        .expect("founder installer ran remotely");
    assert!(
        install.contains(&format!("{remote}/install.sh")),
        "{install}"
    );
    assert!(
        install.contains(&format!(
            "PLOYZ_RELEASE_MANIFEST_URL='file://{remote}/release.env'"
        )),
        "{install}"
    );
    assert!(
        install.contains(&format!("PLOYZ_VERSION='{version}'")),
        "{install}"
    );
}

/// A failed remote installer exits with the SSH phase and the remote output
/// as evidence, before any local context is written (R14).
#[tokio::test(flavor = "multi_thread")]
async fn machine_init_installer_failure_names_phase_and_output() {
    let server = TestNats::start().await;
    let ssh = FakeSshMachine::new("sg-core-1", "echo 'Host Runner exploded' >&2\nexit 1");
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
    assert!(rendered.contains("Host Runner exploded"), "{rendered}");
    assert!(
        !context.context_path.exists(),
        "no cluster context may be recorded for a failed init"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn machine_init_activation_failure_does_not_record_machine_ssh() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[OperationApiEndpoint::from(
        InitFirstMachineActivateApi::ENDPOINT,
    )]);
    let activate_endpoint = endpoint(
        &spec,
        OperationApiEndpoint::from(InitFirstMachineActivateApi::ENDPOINT),
    );
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
        PloyzctlExecutionError::Machine(MachineExecutionError::RemoteMachine {
            ref source,
        }) if matches!(&**source, RemoteMachineExecutionError::MachineIdentity { .. })
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
        OperationApiEndpoint::from(MachineAddApi::ENDPOINT),
        OperationApiEndpoint::from(OpsWatchApi::ENDPOINT),
        OperationApiEndpoint::from(OpsStatusApi::ENDPOINT),
    ]);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(
            &endpoint(&spec, OperationApiEndpoint::from(MachineAddApi::ENDPOINT)),
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
            &endpoint(&spec, OperationApiEndpoint::from(OpsWatchApi::ENDPOINT)),
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
            &endpoint(&spec, OperationApiEndpoint::from(OpsStatusApi::ENDPOINT)),
            |request| async move {
                let request: OpsStatusRequest =
                    serde_json::from_slice(&request.payload).expect("status request decodes");
                let response: OpsStatusResponse = OperationApiResponse::Ok {
                    value: OperationStatusSnapshot::new(OperationStatus::MachineAdd {
                        id: request.operation_id,
                        machine_id: machine_id("sg-edge-1"),
                        name: MachineName::try_new("sg-edge-1").expect("valid machine name"),
                        roles: InstallRolePolicy::install_all(),
                        host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
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
            local_release: None,
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
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
        .find(|command| command.contains("ployz host bootstrap join"))
        .expect("join installer ran remotely");
    for expected in [
        "curl -fsSL -- 'https://get.ployz.sh'",
        "PLOYZ_NATS_URL='tls://203.0.113.10:4222'",
        "PLOYZ_JOIN_NKEY_SEED=",
        "ployz host bootstrap join 'join_once_123'",
    ] {
        assert!(
            install.contains(expected),
            "install missing {expected}: {install}"
        );
    }
    assert!(!install.contains("PLOYZ_MACHINE_PUBLIC_IP="));
}

#[tokio::test(flavor = "multi_thread")]
async fn machine_add_remote_local_release_mismatch_reports_accepted_operation() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[OperationApiEndpoint::from(MachineAddApi::ENDPOINT)]);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");
    runtime
        .bind_endpoint(
            &endpoint(&spec, OperationApiEndpoint::from(MachineAddApi::ENDPOINT)),
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
    let release_dir = tempfile::TempDir::new().expect("release dir");
    let (bundle, _) = local_release_bundle(release_dir.path());

    let error = execute_command(
        PloyzctlCommand::MachineAddRemote(MachineAddRemoteCommand {
            target: SshTarget::parse("root@203.0.113.11").expect("target parses"),
            identity_override: None,
            roles: InstallRolePolicy::install_all(),
            detach: false,
            installer_script: None,
            local_release: Some(Box::new(bundle)),
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
        }),
        &config,
    )
    .await
    .expect_err("mismatched local release fails remote machine add");

    let PloyzctlExecutionError::Machine(MachineExecutionError::RemoteMachine { source }) = &error
    else {
        panic!("expected remote machine error, got {error:?}");
    };
    let RemoteMachineExecutionError::LocalReleaseAfterMachineAdd {
        operation_id,
        message,
    } = &**source
    else {
        panic!("expected accepted local release error, got {error:?}");
    };
    assert!(operation_id.as_str().starts_with("op_add_sg-edge-1_"));
    assert!(message.contains("does not match the cluster machine-join release"));
    assert!(
        error
            .to_string()
            .contains("machine add operation op_add_sg-edge-1_")
    );
    assert_eq!(ssh.commands(), vec!["hostname".to_owned()]);
}

/// Installer failure after token redemption keeps the operation id visible
/// next to the SSH phase and remote output (R14); the operation itself stays
/// on the server as evidence.
#[tokio::test(flavor = "multi_thread")]
async fn machine_add_remote_installer_failure_carries_operation_and_phase() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[OperationApiEndpoint::from(MachineAddApi::ENDPOINT)]);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");
    runtime
        .bind_endpoint(
            &endpoint(&spec, OperationApiEndpoint::from(MachineAddApi::ENDPOINT)),
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

    let ssh = FakeSshMachine::new("sg-edge-1", "echo 'Host Runner join exploded' >&2\nexit 1");
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
            local_release: None,
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
        }),
        &config,
    )
    .await
    .expect_err("failed join installer fails remote machine add");

    let PloyzctlExecutionError::Machine(MachineExecutionError::RemoteMachine { source }) = &error
    else {
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
    assert!(rendered.contains("Host Runner join exploded"), "{rendered}");
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
        OperationApiEndpoint::from(MachineAddApi::ENDPOINT),
        OperationApiEndpoint::from(OpsWatchApi::ENDPOINT),
        OperationApiEndpoint::from(OpsStatusApi::ENDPOINT),
    ]);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(
            &endpoint(&spec, OperationApiEndpoint::from(MachineAddApi::ENDPOINT)),
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
            &endpoint(&spec, OperationApiEndpoint::from(OpsWatchApi::ENDPOINT)),
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
            &endpoint(&spec, OperationApiEndpoint::from(OpsStatusApi::ENDPOINT)),
            |request| async move {
                let request: OpsStatusRequest =
                    serde_json::from_slice(&request.payload).expect("status request decodes");
                let response: OpsStatusResponse = OperationApiResponse::Ok {
                    value: OperationStatusSnapshot::new(OperationStatus::MachineAdd {
                        id: request.operation_id,
                        machine_id: machine_id("sg-edge-1"),
                        name: MachineName::try_new("sg-edge-1").expect("valid machine name"),
                        roles: InstallRolePolicy::install_all(),
                        host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
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
            local_release: None,
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
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

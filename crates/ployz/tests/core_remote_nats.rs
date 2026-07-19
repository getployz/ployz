//! Runtime tests for Core recovery commands against a stand-in `ssh` program
//! and a secured NATS test server hosting mocked operation API endpoints.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use ployz::commands::PloyzctlCommand;
use ployz::core::command::{CorePromoteCommand, CoreReplaceCommand};
use ployz::dispatcher::{PloyzctlRuntimeConfig, execute_command};
use ployz::machine::operator_context::{
    ClusterContext, ClusterContextMachine, load_cluster_context, save_cluster_context,
};
use ployz::ssh::SshTarget;
use ployz_core::operation::OperationEventReplayPage;
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_nats::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_sdk_types::{
    AcceptedOperation, CoreReplaceReportRequest, CoreReplaceReportResponse, CoreReplaceReported,
    CoreReplaceRequest, CoreReplaceResponse, OperationApiResponse, OpsWatchResponse,
    operation_api::{CoreReplaceApi, CoreReplaceReportApi, OperationApiContract, OpsWatchApi},
};
use ployz_test_support::fs::make_executable;
use ployz_test_support::ids::{event_sequence, machine_id};
use ployz_test_support::nats::TestNats;
use tokio::sync::Mutex;

const TEST_CA: &str = "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----";
const CORE_PROMOTE_RESULT_BEGIN: &str = "ployz-core-promote-result begin";
const CORE_PROMOTE_RESULT_END: &str = "ployz-core-promote-result end";
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

/// Stand-in `ssh` for the Core recovery phase commands.
struct FakeCoreSsh {
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

impl FakeCoreSsh {
    fn new() -> Self {
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
  *'internal-core-demote --successor-nats-url'*)
    echo 'core replaced'
    ;;
  *'ployz host core-promote'*)
    cat > {stdin_log}
    printf '%s\n' \
      '{promote_begin}' \
      '{promote_result_json}' \
      '{promote_end}'
    ;;
  'sudo cat /var/lib/ployz/join-material')
    printf '%s\n' 'machine_id=machine_2' 'cluster_name=test'
    ;;
  *)
    echo "unexpected remote command: $cmd" >&2
    exit 9
    ;;
esac
"#,
            log = log.display(),
            stdin_log = stdin_log.display(),
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
    ssh: &FakeCoreSsh,
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
        executor_context_root: None,
        allow_insecure_executor_enrollment: false,
        executor_enrollment_timeout: None,
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

// ---------------------------------------------------------------------------
// core promote USER@HOST / core demote USER@HOST
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn core_promote_remote_runs_host_runner_command_and_updates_context() {
    let _env_lock = ENV_LOCK.lock().await;
    let _secret = EnvVarGuard::set("PLOYZ_RECOVERY_SECRET", "recover-secret");

    let server = TestNats::start().await;
    let ssh = FakeCoreSsh::new();
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
        "core promoted on root@203.0.113.20\nnats tls://203.0.113.20:4222,tls://[2001:db8::20]:4222\nca-pem printed by remote Host Runner\n"
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
            "sudo sh -c 'IFS= read -r PLOYZ_RECOVERY_SECRET; export PLOYZ_RECOVERY_SECRET; exec ployz host core-promote'"
        ]
    );
    assert_eq!(ssh.stdin_text(), "recover-secret\n");
    let saved = load_cluster_context(&context.context_path)
        .expect("context reads")
        .expect("context exists");
    assert_eq!(saved.nats_url.as_str(), "tls://203.0.113.20:4222");
}

#[tokio::test(flavor = "multi_thread")]
async fn core_replace_remote_runs_host_runner_command() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let spec = test_api_service(&[
        OperationApiEndpoint::from(CoreReplaceApi::ENDPOINT),
        OperationApiEndpoint::from(CoreReplaceReportApi::ENDPOINT),
        OperationApiEndpoint::from(OpsWatchApi::ENDPOINT),
    ]);
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");
    let expected_successor_url = server.server.client_url().clone();
    runtime
        .bind_endpoint(
            &endpoint(&spec, OperationApiEndpoint::from(CoreReplaceApi::ENDPOINT)),
            move |request| {
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
                    NatsServiceResponse::ok(
                        serde_json::to_vec(&response).expect("response serializes"),
                    )
                }
            },
        )
        .await
        .expect("core replace endpoint binds");
    runtime
        .bind_endpoint(
            &endpoint(
                &spec,
                OperationApiEndpoint::from(CoreReplaceReportApi::ENDPOINT),
            ),
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
            &endpoint(&spec, OperationApiEndpoint::from(OpsWatchApi::ENDPOINT)),
            |_request| async move {
                let response: OpsWatchResponse = OperationApiResponse::Ok {
                    value: OperationEventReplayPage::terminal(
                        Vec::new(),
                        ployz_core::operation::OperationOutcome::Succeeded,
                    ),
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            },
        )
        .await
        .expect("ops watch endpoint binds");
    client.flush().await.expect("service flushes");

    let ssh = FakeCoreSsh::new();
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
                "sudo ployz host internal-core-demote --successor-nats-url '{}'",
                server.server.client_url().as_str()
            ),
        ]
    );
}

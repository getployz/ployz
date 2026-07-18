use std::process::{Command, Output};

use ployz::dispatcher::{PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_NKEY_SEED_FILE_ENV};
use ployz_core::build::{BuildAdapter, BuildCacheScope, BuildPlatforms, GitSource};
use ployz_core::image::OciPlatform;
use ployz_core::operation::{
    BuildCleanupEvidence, BuildOperationState, CancellationReason, OperationEvent,
    OperationEventReplayPage, OperationOutcome, OperationStatus, OperationStatusSnapshot,
    ReplayedOperationEvent,
};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_nats::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_sdk_types::{
    AcceptedOperation, BuildCancelError, BuildCancelRequest, BuildCancelResponse,
    BuildSubmitRequest, BuildSubmitResponse, OperationApiResponse, OpsStatusRequest,
    OpsStatusResponse, OpsWatchResponse,
    operation_api::{
        BuildCancelApi, BuildSubmitApi, OperationApiContract, OpsStatusApi, OpsWatchApi,
    },
};
use ployz_test_support::ids::{event_sequence, operation_event_recorded_at, operation_id};
use ployz_test_support::nats::{SecuredTestNats, TestNats};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const SECRET: &str = "never-print-this-build-secret";

#[tokio::test(flavor = "multi_thread")]
async fn binary_build_submit_sends_exact_secret_bearing_request_without_echoing_secret() {
    let server = TestNats::start().await;
    let env = CliNatsEnv::new(&server.server);
    let spec = test_api_service(&[OperationApiEndpoint::from(BuildSubmitApi::ENDPOINT)]);
    let endpoint = endpoint(&spec, OperationApiEndpoint::from(BuildSubmitApi::ENDPOINT));
    let mut runtime = start_nats_service(server.controller.clone(), &spec)
        .await
        .expect("service starts");
    runtime
        .bind_endpoint(&endpoint, |request| async move {
            let request: BuildSubmitRequest =
                serde_json::from_slice(&request.payload).expect("request decodes");
            assert_eq!(request.operation_id.as_str(), "op_build_contract");
            assert_eq!(
                request.source.url().as_str(),
                "https://git.example/repo.git"
            );
            assert_eq!(request.source.commit().as_str(), SHA);
            assert_eq!(request.source.credential().username().as_str(), "builder");
            assert_eq!(request.source.credential().secret().secret(), SECRET);
            assert_eq!(request.platforms.iter().count(), 2);
            let BuildAdapter::Dockerfile { dockerfile, target } = request.adapter else {
                panic!("expected Dockerfile adapter");
            };
            assert_eq!(dockerfile.as_str(), "Dockerfile");
            assert!(target.is_none());
            let response: BuildSubmitResponse = OperationApiResponse::Ok {
                value: accepted_operation("op_build_contract"),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    server.controller.flush().await.expect("service flushes");

    let output = build_command(&server.server, &env)
        .env("BUILD_GIT_TOKEN", SECRET)
        .args([
            "build",
            "submit",
            "--git",
            "https://git.example/repo.git",
            "--commit",
            SHA,
            "--git-username",
            "builder",
            "--git-secret-env",
            "BUILD_GIT_TOKEN",
            "--platform",
            "linux/amd64",
            "--platform",
            "linux/arm64",
            "--dockerfile",
            "Dockerfile",
            "--operation-id",
            "op_build_contract",
            "--detach",
        ])
        .output()
        .expect("ployz binary runs");

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("operation op_build_contract"));
    assert!(!stdout(&output).contains(SECRET));
    assert!(!stderr(&output).contains(SECRET));
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_build_cancel_sends_default_reason_and_surfaces_domain_error() {
    let server = TestNats::start().await;
    let env = CliNatsEnv::new(&server.server);
    let spec = test_api_service(&[OperationApiEndpoint::from(BuildCancelApi::ENDPOINT)]);
    let endpoint = endpoint(&spec, OperationApiEndpoint::from(BuildCancelApi::ENDPOINT));
    let mut runtime = start_nats_service(server.controller.clone(), &spec)
        .await
        .expect("service starts");
    runtime
        .bind_endpoint(&endpoint, |request| async move {
            let request: BuildCancelRequest =
                serde_json::from_slice(&request.payload).expect("request decodes");
            assert_eq!(request.operation_id.as_str(), "op_build_missing");
            assert_eq!(
                request.reason.as_str(),
                "operator requested build cancellation"
            );
            let response: BuildCancelResponse = OperationApiResponse::DomainError {
                error: BuildCancelError::NoSuchOperation {
                    operation_id: request.operation_id,
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    server.controller.flush().await.expect("service flushes");

    let output = build_command(&server.server, &env)
        .args(["build", "cancel", "op_build_missing"])
        .output()
        .expect("ployz binary runs");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("no such build operation op_build_missing"));
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_build_cancel_follows_the_accepted_operation_to_terminal() {
    let server = TestNats::start().await;
    let env = CliNatsEnv::new(&server.server);
    let spec = test_api_service(&[
        OperationApiEndpoint::from(BuildCancelApi::ENDPOINT),
        OperationApiEndpoint::from(OpsWatchApi::ENDPOINT),
        OperationApiEndpoint::from(OpsStatusApi::ENDPOINT),
    ]);
    let cancel_endpoint = endpoint(&spec, OperationApiEndpoint::from(BuildCancelApi::ENDPOINT));
    let watch_endpoint = endpoint(&spec, OperationApiEndpoint::from(OpsWatchApi::ENDPOINT));
    let status_endpoint = endpoint(&spec, OperationApiEndpoint::from(OpsStatusApi::ENDPOINT));
    let mut runtime = start_nats_service(server.controller.clone(), &spec)
        .await
        .expect("service starts");
    runtime
        .bind_endpoint(&cancel_endpoint, |_request| async move {
            let response: BuildCancelResponse = OperationApiResponse::Ok {
                value: accepted_operation("op_build_cancelled"),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("cancel endpoint binds");
    runtime
        .bind_endpoint(&watch_endpoint, |_request| async move {
            let response: OpsWatchResponse = OperationApiResponse::Ok {
                value: OperationEventReplayPage::terminal(
                    vec![replayed(
                        1,
                        OperationEvent::BuildCancelled {
                            operation_id: operation_id("op_build_cancelled"),
                            reason: cancellation_reason(),
                            cleanup: completed_cleanup(),
                        },
                    )],
                    OperationOutcome::Cancelled,
                ),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("watch endpoint binds");
    runtime
        .bind_endpoint(&status_endpoint, |request| async move {
            let request: OpsStatusRequest =
                serde_json::from_slice(&request.payload).expect("status request decodes");
            let response: OpsStatusResponse = OperationApiResponse::Ok {
                value: OperationStatusSnapshot::new(build_cancelled_status(request.operation_id)),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("status endpoint binds");
    server.controller.flush().await.expect("service flushes");

    let output = build_command(&server.server, &env)
        .args(["build", "cancel", "op_build_cancelled"])
        .output()
        .expect("ployz binary runs");
    assert!(
        !output.status.success(),
        "cancelled operation exits non-zero"
    );
    assert!(stdout(&output).contains("build.cancelled"));
}

#[test]
fn binary_rejects_git_secret_value_in_argv() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "build",
            "submit",
            "--git",
            "https://git.example/repo.git",
            "--commit",
            SHA,
            "--git-username",
            "builder",
            "--git-secret",
            SECRET,
            "--platform",
            "linux/amd64",
            "--dockerfile",
            "Dockerfile",
        ])
        .output()
        .expect("ployz binary runs");
    assert!(!output.status.success());
    assert!(!stdout(&output).contains(SECRET));
    assert!(!stderr(&output).contains(SECRET));
}

#[test]
fn binary_build_submit_fails_cleanly_when_the_named_secret_is_missing() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env_remove("BUILD_GIT_TOKEN")
        .args([
            "build",
            "submit",
            "--git",
            "https://git.example/repo.git",
            "--commit",
            SHA,
            "--git-username",
            "builder",
            "--git-secret-env",
            "BUILD_GIT_TOKEN",
            "--platform",
            "linux/amd64",
            "--dockerfile",
            "Dockerfile",
        ])
        .output()
        .expect("ployz binary runs");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("environment variable BUILD_GIT_TOKEN"));
    assert!(!stderr(&output).contains(SECRET));
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
}

fn build_command(server: &SecuredTestNats, env: &CliNatsEnv) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .arg("--nats")
        .arg(server.client_url().as_str())
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env(PLOYZ_NATS_CA_FILE_ENV, server.ca_path())
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, &env.user_seed_file);
    command
}

fn test_api_service(endpoints: &[OperationApiEndpoint]) -> NatsServiceSpec {
    NatsServiceSpec::new(
        "plz-api.build-test",
        "plz-api",
        ServiceVersion::new(0, 1, 0),
        "build test API service",
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

fn accepted_operation(id: &str) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: operation_id(id),
        watch_subject: format!("plz.v1.progress.build.operation.{id}.>"),
        start_sequence: event_sequence(1),
    }
}

fn build_cancelled_status(id: ployz_core::ids::OperationId) -> OperationStatus {
    let source = GitSource::try_new(
        "https://git.example/repo.git",
        SHA,
        "builder",
        SECRET,
        None::<String>,
    )
    .expect("source");
    OperationStatus::Build {
        id,
        source: source.evidence(),
        adapter: BuildAdapter::Railpack {
            cache_scope: BuildCacheScope::try_new("test").expect("cache scope"),
        },
        platforms: BuildPlatforms::try_new([
            OciPlatform::try_new("linux", "amd64").expect("platform")
        ])
        .expect("platforms"),
        state: BuildOperationState::Cancelled {
            reason: cancellation_reason(),
            cleanup: completed_cleanup(),
        },
        last_event_sequence: event_sequence(1),
    }
}

fn cancellation_reason() -> CancellationReason {
    CancellationReason::try_new("operator requested build cancellation").expect("reason")
}

fn completed_cleanup() -> BuildCleanupEvidence {
    BuildCleanupEvidence::Completed {
        machine_ids: Vec::new(),
    }
}

fn replayed(sequence: u64, event: OperationEvent) -> ReplayedOperationEvent {
    ReplayedOperationEvent {
        sequence: event_sequence(sequence),
        recorded_at_unix_ms: operation_event_recorded_at(1_784_116_800_000 + sequence),
        event,
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

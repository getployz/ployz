use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ployz::dispatcher::{PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_NKEY_SEED_FILE_ENV};
use ployz_core::deploy::{DeployServiceSpec, ImageReference, ReplicaCount};
use ployz_core::ids::NamespaceId;
use ployz_core::ops::{
    DeployOperationState, ManagedDnsReconcileOperationState, ManagedDnsReconcileSubject,
    OperationEvent, OperationEventReplayPage, OperationEventReplayRequest, OperationStatus,
    OperationStatusSnapshot, ReplayedOperationEvent,
};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_nats::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_sdk_types::{
    OperationApiResponse, OpsListRequest, OpsListResponse, OpsListResult, OpsStatusRequest,
    OpsStatusResponse, OpsWatchResponse,
    operation_api::{OperationApiContract, OpsListApi, OpsStatusApi, OpsWatchApi},
};
use ployz_test_support::ids::{event_sequence, operation_id, service_id};
use ployz_test_support::nats::{SecuredTestNats, TestNats};

#[tokio::test(flavor = "multi_thread")]
async fn binary_ops_watch_replays_terminal_event_after_a_caught_up_page() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
    let service_client = client.clone();
    let spec = test_api_service(&[
        OperationApiEndpoint::from(OpsWatchApi::ENDPOINT),
        OperationApiEndpoint::from(OpsStatusApi::ENDPOINT),
    ]);
    let watch_endpoint = endpoint(&spec, OperationApiEndpoint::from(OpsWatchApi::ENDPOINT));
    let status_endpoint = endpoint(&spec, OperationApiEndpoint::from(OpsStatusApi::ENDPOINT));
    let watch_calls = Arc::new(AtomicUsize::new(0));
    let mut runtime = start_nats_service(client, &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(&watch_endpoint, {
            let watch_calls = watch_calls.clone();
            move |request| {
                let watch_calls = watch_calls.clone();
                async move {
                    let request: OperationEventReplayRequest =
                        serde_json::from_slice(&request.payload).expect("watch request decodes");
                    assert_eq!(request.operation_id, operation_id("op_deploy"));

                    let call = watch_calls.fetch_add(1, Ordering::SeqCst);
                    let page = match call {
                        0 => {
                            assert_eq!(request.start_sequence, event_sequence(1));
                            OperationEventReplayPage::caught_up(vec![replayed(
                                1,
                                OperationEvent::DeploySubmitted {
                                    operation_id: operation_id("op_deploy"),
                                    reservation_id: Some(
                                        ployz_core::deploy::DeployReservationId::first(),
                                    ),
                                    target: deploy_request(),
                                },
                            )])
                        }
                        1 => {
                            assert_eq!(request.start_sequence, event_sequence(2));
                            OperationEventReplayPage::terminal(vec![replayed(
                                2,
                                OperationEvent::DeployCompleted {
                                    operation_id: operation_id("op_deploy"),
                                    outcome: ployz_core::ops::DeployCompletionOutcome::Completed,
                                },
                            )])
                        }
                        unexpected => panic!("unexpected watch call {unexpected}"),
                    };
                    let response: OpsWatchResponse = OperationApiResponse::Ok { value: page };
                    NatsServiceResponse::ok(
                        serde_json::to_vec(&response).expect("response serializes"),
                    )
                }
            }
        })
        .await
        .expect("watch endpoint binds");

    runtime
        .bind_endpoint(&status_endpoint, |request| async move {
            let request: OpsStatusRequest =
                serde_json::from_slice(&request.payload).expect("status request decodes");
            assert_eq!(request.operation_id, operation_id("op_deploy"));

            let response: OpsStatusResponse = OperationApiResponse::Ok {
                value: OperationStatusSnapshot::new(OperationStatus::Deploy {
                    id: operation_id("op_deploy"),
                    namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
                    service_id: service_id("svc_api"),
                    origin: None,
                    state: DeployOperationState::Completed {
                        outcome: ployz_core::ops::DeployCompletionOutcome::Completed,
                    },
                    last_event_sequence: event_sequence(2),
                }),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("status endpoint binds");
    service_client.flush().await.expect("service flushes");

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--nats")
        .arg(server.server.client_url().as_str())
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env(PLOYZ_NATS_CA_FILE_ENV, server.server.ca_path())
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, env.user_seed_path())
        .args(["ops", "watch", "op_deploy"])
        .output()
        .expect("ployz binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "1 deploy.submitted\n2 deploy.completed\n");
    assert_eq!(stderr(&output), "");
    assert_eq!(watch_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_ops_list_prints_continuation_hint_to_stderr() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
    let spec = test_api_service(&[OperationApiEndpoint::from(OpsListApi::ENDPOINT)]);
    let list_endpoint = endpoint(&spec, OperationApiEndpoint::from(OpsListApi::ENDPOINT));
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(&list_endpoint, |request| async move {
            let request: OpsListRequest =
                serde_json::from_slice(&request.payload).expect("list request decodes");
            assert!(!request.active_only);
            assert_eq!(request.before, None);
            let response: OpsListResponse = OperationApiResponse::Ok {
                value: OpsListResult {
                    operations: vec![OperationStatusSnapshot::new(
                        OperationStatus::ManagedDnsReconcile {
                            id: operation_id("op_oldest_on_page"),
                            subject: ManagedDnsReconcileSubject::Acquire,
                            state: ManagedDnsReconcileOperationState::Accepted,
                            last_event_sequence: event_sequence(1),
                        },
                    )],
                    has_more: true,
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("list endpoint binds");
    client.flush().await.expect("service flushes");

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--nats")
        .arg(server.server.client_url().as_str())
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env(PLOYZ_NATS_CA_FILE_ENV, server.server.ca_path())
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, env.user_seed_path())
        .args(["ops", "list"])
        .output()
        .expect("ployz binary runs");

    assert!(output.status.success());
    assert_eq!(
        stdout(&output),
        "op_oldest_on_page managed-dns-reconcile Ployz DNS target acquisition accepted\n"
    );
    assert_eq!(
        stderr(&output),
        "More operations available:\n  ployz ops list --before op_oldest_on_page\n"
    );
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

fn replayed(sequence: u64, event: OperationEvent) -> ReplayedOperationEvent {
    ReplayedOperationEvent {
        sequence: event_sequence(sequence),
        event,
    }
}

fn deploy_request() -> ployz_core::deploy::DeployRequest {
    ployz_core::deploy::DeployRequest {
        namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
        origin: None,
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

use std::process::{Command, Output};

use ployz::deploy::history_store::{
    ClusterFingerprint, DeployHistory, DeployHistoryEntry, DeployHistoryTimestamp,
};
use ployz::dispatcher::{PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_NKEY_SEED_FILE_ENV};
use ployz_core::deploy::{
    ContainerRuntimeSpec, DeployOrigin, DeployRequest, DeployServiceSpec, ImageReference,
    ImageSource, ReplicaCount,
};
use ployz_core::ids::{NamespaceId, ServiceId};
use ployz_core::operation::{
    DeployCompletionOutcome, DeployOperationFailure, DeployOperationState, OperationEvent,
    OperationEventReplayPage, OperationOutcome, OperationStatus, OperationStatusSnapshot,
    ReplayedOperationEvent,
};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_nats::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_sdk_types::{
    AcceptedOperation, DeployReservationExpiresAt, DeployReservationId, DeployReserveResponse,
    DeployReserved, DeploySubmitRequest, DeploySubmitResponse, OperationApiResponse,
    OpsStatusRequest, OpsStatusResponse, OpsWatchResponse,
    operation_api::{
        DeployReserveApi, DeploySubmitApi, OperationApiContract, OpsStatusApi, OpsWatchApi,
    },
};
use ployz_test_support::ids::{
    event_sequence, operation_event_recorded_at, operation_id, service_id,
};
use ployz_test_support::nats::{SecuredTestNats, TestNats};

#[tokio::test(flavor = "multi_thread")]
async fn binary_deploy_calls_nats_service() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
    let service_client = client.clone();
    let spec = test_api_service(&[
        OperationApiEndpoint::from(DeployReserveApi::ENDPOINT),
        OperationApiEndpoint::from(DeploySubmitApi::ENDPOINT),
    ]);
    let reserve_endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject == OperationApiEndpoint::from(DeployReserveApi::ENDPOINT).subject()
        })
        .expect("deploy reserve endpoint is present")
        .clone();
    let submit_endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject == OperationApiEndpoint::from(DeploySubmitApi::ENDPOINT).subject()
        })
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
            assert_eq!(
                request.target.origin,
                Some(DeployOrigin::try_new("release candidate").expect("valid origin"))
            );
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

#[tokio::test(flavor = "multi_thread")]
async fn binary_rollback_replays_the_selected_pinned_payload_as_a_new_deploy() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
    let namespace_id = NamespaceId::try_new("default").expect("valid namespace");
    let selected_request = pinned_request(None);
    let history = DeployHistory::new(
        env.state_home().join("ployz/deploy-history"),
        ClusterFingerprint::from_connection(
            server.server.client_url().as_str(),
            server.server.ca_path(),
        )
        .expect("cluster fingerprint"),
        namespace_id.clone(),
    );
    history
        .append_success(DeployHistoryEntry {
            recorded_at: DeployHistoryTimestamp::from_unix_seconds(1_750_000_000),
            operation_id: operation_id("op_selected"),
            request: selected_request.clone(),
        })
        .expect("selected history entry persists");

    let service_client = client.clone();
    let spec = test_api_service(&[
        OperationApiEndpoint::from(DeployReserveApi::ENDPOINT),
        OperationApiEndpoint::from(DeploySubmitApi::ENDPOINT),
        OperationApiEndpoint::from(OpsWatchApi::ENDPOINT),
        OperationApiEndpoint::from(OpsStatusApi::ENDPOINT),
    ]);
    let reserve_endpoint = endpoint(
        &spec,
        OperationApiEndpoint::from(DeployReserveApi::ENDPOINT),
    );
    let submit_endpoint = endpoint(&spec, OperationApiEndpoint::from(DeploySubmitApi::ENDPOINT));
    let watch_endpoint = endpoint(&spec, OperationApiEndpoint::from(OpsWatchApi::ENDPOINT));
    let status_endpoint = endpoint(&spec, OperationApiEndpoint::from(OpsStatusApi::ENDPOINT));
    let mut runtime = start_nats_service(client, &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(&reserve_endpoint, move |request| {
            let namespace_id = namespace_id.clone();
            async move {
                let request: ployz_sdk_types::DeployReserveRequest =
                    serde_json::from_slice(&request.payload).expect("reserve request decodes");
                assert_eq!(request.namespace_id, namespace_id);
                let response: DeployReserveResponse = OperationApiResponse::Ok {
                    value: DeployReserved {
                        reservation_id: DeployReservationId::first(),
                        expires_at: DeployReservationExpiresAt::try_new(4_102_444_800)
                            .expect("valid expiration"),
                    },
                };
                NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
            }
        })
        .await
        .expect("reserve endpoint binds");
    runtime
        .bind_endpoint(&submit_endpoint, |request| async move {
            let request: DeploySubmitRequest =
                serde_json::from_slice(&request.payload).expect("deploy request decodes");
            assert_eq!(request.reservation_id, DeployReservationId::first());
            assert!(request.idempotency_key.as_str().starts_with("idem_deploy_"));
            assert_eq!(
                request.target,
                pinned_request(Some(
                    DeployOrigin::try_new("rollback").expect("valid rollback origin")
                ))
            );
            let response: DeploySubmitResponse = OperationApiResponse::Ok {
                value: accepted_operation("op_rollback"),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("submit endpoint binds");
    runtime
        .bind_endpoint(&watch_endpoint, |request| async move {
            let request: ployz_core::operation::OperationEventReplayRequest =
                serde_json::from_slice(&request.payload).expect("watch request decodes");
            assert_eq!(request.operation_id, operation_id("op_rollback"));
            assert_eq!(request.start_sequence, event_sequence(1));
            let response: OpsWatchResponse = OperationApiResponse::Ok {
                value: OperationEventReplayPage::terminal(
                    vec![
                        replayed(
                            1,
                            OperationEvent::DeploySubmitted {
                                operation_id: operation_id("op_rollback"),
                                reservation_id: Some(DeployReservationId::first()),
                                target: pinned_request(Some(
                                    DeployOrigin::try_new("rollback")
                                        .expect("valid rollback origin"),
                                )),
                            },
                        ),
                        replayed(
                            2,
                            OperationEvent::DeployCompleted {
                                operation_id: operation_id("op_rollback"),
                                outcome: DeployCompletionOutcome::Completed,
                            },
                        ),
                    ],
                    OperationOutcome::Succeeded,
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
            assert_eq!(request.operation_id, operation_id("op_rollback"));
            let response: OpsStatusResponse = OperationApiResponse::Ok {
                value: OperationStatusSnapshot::new(deploy_status(
                    "op_rollback",
                    DeployOperationState::Completed {
                        outcome: DeployCompletionOutcome::Completed,
                    },
                )),
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
        .env("XDG_STATE_HOME", env.state_home())
        .env(PLOYZ_NATS_CA_FILE_ENV, server.server.ca_path())
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, env.user_seed_path())
        .args(["deploy", "rollback", "--to", "op_selected"])
        .output()
        .expect("ployz binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).contains("deploy op_rollback: succeeded"));
    assert_eq!(stderr(&output), "");
    let entries = history.load().expect("history reloads");
    let [_selected, rollback] = entries.as_slice() else {
        panic!("selected deploy and rollback must be recorded: {entries:?}");
    };
    assert_eq!(rollback.operation_id, operation_id("op_rollback"));
    assert_eq!(
        rollback.request.origin,
        Some(DeployOrigin::try_new("rollback").expect("valid rollback origin"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_foreground_deploy_exits_non_zero_when_operation_fails() {
    let server = TestNats::start().await;
    let client = server.controller.clone();
    let env = CliNatsEnv::new(&server.server);
    let service_client = client.clone();
    let spec = test_api_service(&[
        OperationApiEndpoint::from(DeployReserveApi::ENDPOINT),
        OperationApiEndpoint::from(DeploySubmitApi::ENDPOINT),
        OperationApiEndpoint::from(OpsWatchApi::ENDPOINT),
        OperationApiEndpoint::from(OpsStatusApi::ENDPOINT),
    ]);
    let reserve_endpoint = endpoint(
        &spec,
        OperationApiEndpoint::from(DeployReserveApi::ENDPOINT),
    );
    let submit_endpoint = endpoint(&spec, OperationApiEndpoint::from(DeploySubmitApi::ENDPOINT));
    let watch_endpoint = endpoint(&spec, OperationApiEndpoint::from(OpsWatchApi::ENDPOINT));
    let status_endpoint = endpoint(&spec, OperationApiEndpoint::from(OpsStatusApi::ENDPOINT));
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
        .bind_endpoint(&submit_endpoint, |_request| async move {
            let response: DeploySubmitResponse = OperationApiResponse::Ok {
                value: accepted_operation("op_deploy_failed"),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("submit endpoint binds");
    runtime
        .bind_endpoint(&watch_endpoint, |request| async move {
            let request: ployz_core::operation::OperationEventReplayRequest =
                serde_json::from_slice(&request.payload).expect("watch request decodes");
            assert_eq!(request.operation_id, operation_id("op_deploy_failed"));
            let response: OpsWatchResponse = OperationApiResponse::Ok {
                value: OperationEventReplayPage::terminal(
                    vec![
                        replayed(
                            1,
                            OperationEvent::DeploySubmitted {
                                operation_id: operation_id("op_deploy_failed"),
                                reservation_id: Some(DeployReservationId::first()),
                                target: forward_request(),
                            },
                        ),
                        replayed(
                            2,
                            OperationEvent::DeployFailed {
                                operation_id: operation_id("op_deploy_failed"),
                                failure: DeployOperationFailure::NoUsableMachines {
                                    reasons: Vec::new(),
                                },
                            },
                        ),
                    ],
                    OperationOutcome::Failed,
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
            assert_eq!(request.operation_id, operation_id("op_deploy_failed"));
            let response: OpsStatusResponse = OperationApiResponse::Ok {
                value: OperationStatusSnapshot::new(deploy_status(
                    "op_deploy_failed",
                    DeployOperationState::Failed {
                        failure: DeployOperationFailure::NoUsableMachines {
                            reasons: Vec::new(),
                        },
                    },
                )),
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
        .env("XDG_STATE_HOME", env.state_home())
        .env(PLOYZ_NATS_CA_FILE_ENV, server.server.ca_path())
        .env(PLOYZ_NATS_NKEY_SEED_FILE_ENV, env.user_seed_path())
        .args([
            "deploy",
            "--service",
            "svc_api",
            "--image",
            "ghcr.io/acme/api:rev-2",
            "--replicas",
            "1",
        ])
        .output()
        .expect("ployz binary runs");

    assert!(
        !output.status.success(),
        "a followed deploy that failed must exit non-zero; stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("Deploy failed"),
        "the failure must still be printed; stdout:\n{}",
        stdout(&output)
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

    fn state_home(&self) -> std::path::PathBuf {
        self._dir.path().join("state")
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

fn deploy_status(operation: &str, state: DeployOperationState) -> OperationStatus {
    OperationStatus::Deploy {
        id: operation_id(operation),
        namespace_id: NamespaceId::try_new("default").expect("valid namespace"),
        service_id: service_id("svc_api"),
        origin: None,
        state,
        last_event_sequence: event_sequence(2),
    }
}

fn forward_request() -> DeployRequest {
    DeployRequest {
        namespace_id: NamespaceId::try_new("default").expect("valid namespace"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
            image_source: ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replicas"),
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn accepted_operation(operation_id: &str) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: self::operation_id(operation_id),
        watch_subject: format!("plz.v1.progress.namespace.default.operation.{operation_id}.>"),
        start_sequence: event_sequence(1),
    }
}

fn pinned_request(origin: Option<DeployOrigin>) -> DeployRequest {
    DeployRequest {
        namespace_id: NamespaceId::try_new("default").expect("valid namespace"),
        origin,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id("svc_api"),
            image: ImageReference::try_new(
                "ghcr.io/acme/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .expect("valid pinned image"),
            image_source: ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replicas"),
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn replayed(sequence: u64, event: OperationEvent) -> ReplayedOperationEvent {
    ReplayedOperationEvent {
        sequence: event_sequence(sequence),
        recorded_at_unix_ms: operation_event_recorded_at(1_784_116_800_000 + sequence),
        event,
    }
}

fn deploy_args() -> [&'static str; 10] {
    [
        "deploy",
        "--service",
        "svc_api",
        "--image",
        "ghcr.io/acme/api:rev-2",
        "--replicas",
        "1",
        "--origin",
        "release candidate",
        "--detach",
    ]
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

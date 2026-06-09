use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ployz_core::deploy::{ImageReference, ReplicaCount};
use ployz_core::ids::{OperationId, RevisionId, ServiceId};
use ployz_core::ops::{
    DeployOperationState, DeployRunningStage, EventSequence, OperationEvent,
    OperationEventReplayPage, OperationEventReplayRequest, OperationOwnershipStatus,
    OperationStatus, OperationStatusSnapshot, ReplayedOperationEvent,
};
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    OperationApiResponse, OpsStatusRequest, OpsStatusResponse, OpsWatchResponse,
    operation_api::{OperationApiContract, OpsStatusApi, OpsWatchApi},
};

#[tokio::test(flavor = "multi_thread")]
async fn binary_ops_watch_polls_until_operation_is_terminal() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let service_client = client.clone();
    let spec = test_api_service(&[OpsWatchApi::ENDPOINT, OpsStatusApi::ENDPOINT]);
    let watch_endpoint = endpoint(&spec, OpsWatchApi::ENDPOINT);
    let status_endpoint = endpoint(&spec, OpsStatusApi::ENDPOINT);
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
                value: OperationStatusSnapshot::new(
                    OperationStatus::Deploy {
                        id: operation_id("op_deploy"),
                        service_id: service_id("svc_api"),
                        state: DeployOperationState::Running {
                            stage: DeployRunningStage::WaitingForHealth,
                        },
                        last_event_sequence: event_sequence(1),
                    },
                    OperationOwnershipStatus::Unclaimed,
                ),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("status endpoint binds");
    service_client.flush().await.expect("service flushes");

    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .arg("--nats")
        .arg(server.client_url())
        .args(["ops", "watch", "op_deploy"])
        .output()
        .expect("ployzctl binary runs");

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
        service_id: service_id("svc_api"),
        target_revision: RevisionId::try_new("rev_2").expect("valid revision id"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
        route: None,
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::machine_runtime::ManagedContainerKind;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, start_nats_service};
use ployz_test_support::ids::{
    container_id, failure_message, machine_id, namespace_id, operation_id, revision_id, service_id,
    step_id,
};
use ployzd::deploy_worker::{
    MachineContainerRuntime, MachineContainerRuntimeError, MachineRuntimeUnavailableReason,
};
use ployzd::docker::labels::ManagedContainerLabels;
use ployzd::machine_runtime::client::{NatsMachineContainerRuntime, NatsMachineSubstrateUpdater};
use ployzd::machine_runtime::protocol::{
    MachineContainerRemoveDomainError, MachineContainerRemoveRpcRequest,
    MachineContainerRemoveRpcResponse, MachineContainerRpcOk, MachineContainerRunDomainError,
    MachineContainerRunRpcOk, MachineContainerRunRpcRequest, MachineContainerRunRpcResponse,
    MachineContainerRunSpec, MachineRunContainerOutcome, MachineSubstrateReportRpcOk,
    MachineSubstrateReportRpcResponse,
};
use ployzd::services::machine_runtime_service;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn nats_machine_runtime_calls_container_run_service() {
    let nats = test_nats().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let _service = start_container_run_service(nats.machine_a.clone(), &machine_id("machine_a"), {
        let received = Arc::clone(&received);
        move |request| {
            received
                .lock()
                .expect("received request lock is not poisoned")
                .push(request);
            MachineRunContainerResult::Ok {
                outcome: MachineRunContainerOutcome::Created {
                    container_id: container_id("ctr_123"),
                },
            }
        }
    })
    .await;
    let mut runtime = NatsMachineContainerRuntime::new(nats.client);

    let outcome = runtime
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect("machine container run succeeds");

    assert_eq!(
        outcome,
        MachineRunContainerOutcome::Created {
            container_id: container_id("ctr_123")
        }
    );
    assert_eq!(
        received
            .lock()
            .expect("received request lock is not poisoned")
            .as_slice(),
        [MachineContainerRunRpcRequest {
            image: image("registry.example/api:rev_2"),
            endpoint: None,
            container: managed_container_spec()
        }]
    );
}

#[tokio::test]
async fn nats_machine_runtime_maps_missing_responder_to_request_failure() {
    let nats = test_nats().await;
    let mut runtime = NatsMachineContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(&machine_id("machine_missing"), run_request())
        .await
        .expect_err("missing machine responder fails");

    assert_eq!(
        error,
        MachineContainerRuntimeError::Unavailable {
            machine_id: machine_id("machine_missing"),
            reason: MachineRuntimeUnavailableReason::NoResponders,
        }
    );
}

#[tokio::test]
async fn nats_machine_runtime_maps_service_error_headers() {
    let nats = test_nats().await;
    let _service = start_container_run_raw_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        |_request| {
            NatsServiceResponse::transport_error(
                ployz_nats::service_runtime::NatsServiceError::bad_request("bad container request"),
            )
        },
    )
    .await;
    let mut runtime = NatsMachineContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("service error header fails");

    assert_eq!(
        error,
        MachineContainerRuntimeError::Unavailable {
            machine_id: machine_id("machine_a"),
            reason: MachineRuntimeUnavailableReason::ServiceBadRequest {
                message: "bad container request".to_owned(),
            },
        }
    );
}

#[tokio::test]
async fn nats_machine_runtime_reports_invalid_response_payload() {
    let nats = test_nats().await;
    let _service = start_container_run_raw_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        |_request| NatsServiceResponse::ok("not json"),
    )
    .await;
    let mut runtime = NatsMachineContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("invalid response payload fails");

    assert!(matches!(
        error,
        MachineContainerRuntimeError::Unavailable {
            machine_id: actual_machine_id,
            reason: MachineRuntimeUnavailableReason::DecodeResponse { .. },
            ..
        } if actual_machine_id == machine_id("machine_a")
    ));
}

#[tokio::test]
async fn nats_machine_runtime_preserves_domain_runtime_error() {
    let nats = test_nats().await;
    let conflict = MachineContainerRunDomainError::OperationStepConflict {
        container_id: container_id("ctr_existing"),
        expected: managed_labels(),
        actual: ManagedContainerLabels {
            revision_id: revision_id("rev_other"),
            ..managed_labels()
        },
    };
    let _service = start_container_run_service(nats.machine_a.clone(), &machine_id("machine_a"), {
        let conflict = conflict.clone();
        move |_request| MachineRunContainerResult::DomainError {
            error: conflict.clone(),
        }
    })
    .await;
    let mut runtime = NatsMachineContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("domain runtime error fails");

    assert_eq!(
        error,
        MachineContainerRuntimeError::OperationStepConflict {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_existing"),
            expected: managed_labels(),
            actual: ManagedContainerLabels {
                revision_id: revision_id("rev_other"),
                ..managed_labels()
            },
        }
    );
}

#[tokio::test]
async fn nats_machine_runtime_rejects_response_for_different_machine() {
    let nats = test_nats().await;
    let _service = start_container_run_raw_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        |_request| {
            NatsServiceResponse::ok(encode_run_response(MachineContainerRunRpcResponse::Ok(
                MachineContainerRunRpcOk {
                    machine_id: machine_id("machine_b"),
                    outcome: MachineRunContainerOutcome::Created {
                        container_id: container_id("ctr_wrong"),
                    },
                },
            )))
        },
    )
    .await;
    let mut runtime = NatsMachineContainerRuntime::new(nats.client);

    let error = runtime
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("wrong-machine domain error fails");

    assert_eq!(
        error,
        MachineContainerRuntimeError::Unavailable {
            machine_id: machine_id("machine_a"),
            reason: MachineRuntimeUnavailableReason::WrongResponder {
                actual_machine_id: machine_id("machine_b"),
            },
        }
    );
}

#[tokio::test]
async fn nats_machine_runtime_reports_substrate_versions() {
    let nats = test_nats().await;
    let _service =
        start_substrate_report_service(nats.machine_a.clone(), &machine_id("machine_a")).await;
    let runtime = NatsMachineSubstrateUpdater::new(nats.client);

    let reported = runtime
        .report_substrate_versions(&machine_id("machine_a"), &operation_id("op_update_1"))
        .await
        .expect("substrate report succeeds");

    assert_eq!(
        reported.ployzd.as_ref().map(|version| version.as_str()),
        Some("0.2.0")
    );
    assert_eq!(reported.keeper, None);
}

#[tokio::test]
async fn nats_machine_runtime_calls_container_remove_service() {
    let nats = test_nats().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let _service =
        start_container_remove_service(nats.machine_a.clone(), &machine_id("machine_a"), {
            let received = Arc::clone(&received);
            move |request| {
                received
                    .lock()
                    .expect("received remove request lock is not poisoned")
                    .push(request);
                MachineRemoveContainerResult::Ok {
                    container_id: container_id("ctr_old"),
                }
            }
        })
        .await;
    let mut runtime = NatsMachineContainerRuntime::new(nats.client);

    runtime
        .remove_container(&machine_id("machine_a"), remove_request("ctr_old"))
        .await
        .expect("machine container remove succeeds");

    assert_eq!(
        received
            .lock()
            .expect("received remove request lock is not poisoned")
            .as_slice(),
        [MachineContainerRemoveRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_old"),
            expected_identity: managed_labels().identity(),
        }]
    );
}

#[tokio::test]
async fn nats_machine_runtime_preserves_remove_domain_error() {
    let nats = test_nats().await;
    let failure = MachineContainerRemoveDomainError::RemoveFailed {
        container_id: container_id("ctr_old"),
        message: failure_message("container remove failed: busy"),
        inspect_hint: inspect_hint("ctr_old"),
    };
    let _service =
        start_container_remove_service(nats.machine_a.clone(), &machine_id("machine_a"), {
            let failure = failure.clone();
            move |_request| MachineRemoveContainerResult::DomainError {
                error: failure.clone(),
            }
        })
        .await;
    let mut runtime = NatsMachineContainerRuntime::new(nats.client);

    let error = runtime
        .remove_container(&machine_id("machine_a"), remove_request("ctr_old"))
        .await
        .expect_err("remove domain error is preserved");

    assert_eq!(
        error,
        MachineContainerRuntimeError::RemoveContainerFailed {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_old"),
            message: failure_message("container remove failed: busy"),
            inspect_hint: inspect_hint("ctr_old"),
        }
    );
}

async fn start_container_run_service(
    client: async_nats::Client,
    machine_id: &MachineId,
    handler: impl Fn(MachineContainerRunRpcRequest) -> MachineRunContainerResult + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let machine_id = machine_id.clone();
    start_container_run_raw_service(client, machine_id.clone(), move |request| {
        let response = decode_run_request(request)
            .map(&handler)
            .map(|result| result.into_nats_response(machine_id.clone()));
        match response {
            Ok(response) => response,
            Err(message) => NatsServiceResponse::transport_error(
                ployz_nats::service_runtime::NatsServiceError::bad_request(message),
            ),
        }
    })
    .await
}

async fn start_container_run_raw_service(
    client: async_nats::Client,
    machine_id: MachineId,
    handler: impl Fn(NatsServiceRequest) -> NatsServiceResponse + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let spec = machine_runtime_service(&machine_id);
    let endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject == machine_service(&machine_id, MachineServiceEndpoint::ContainerRun)
        })
        .expect("container.run endpoint exists")
        .clone();
    let mut service = start_nats_service(client.clone(), &spec)
        .await
        .expect("start machine service");
    service
        .bind_endpoint(&endpoint, move |request| {
            let response = handler(request);
            async move { response }
        })
        .await
        .expect("bind container.run endpoint");
    // Round-trip so the server has registered the subscription before a
    // request arrives from another (authenticated) connection.
    client.flush().await.expect("flush service subscription");
    service
}

async fn start_container_remove_service(
    client: async_nats::Client,
    machine_id: &MachineId,
    handler: impl Fn(MachineContainerRemoveRpcRequest) -> MachineRemoveContainerResult
    + Send
    + Sync
    + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let machine_id = machine_id.clone();
    start_container_remove_raw_service(client, machine_id.clone(), move |request| {
        let response = decode_remove_request(request)
            .map(&handler)
            .map(|result| result.into_nats_response(machine_id.clone()));
        match response {
            Ok(response) => response,
            Err(message) => NatsServiceResponse::transport_error(
                ployz_nats::service_runtime::NatsServiceError::bad_request(message),
            ),
        }
    })
    .await
}

async fn start_container_remove_raw_service(
    client: async_nats::Client,
    machine_id: MachineId,
    handler: impl Fn(NatsServiceRequest) -> NatsServiceResponse + Send + Sync + 'static,
) -> ployz_nats::service_runtime::RunningNatsService {
    let spec = machine_runtime_service(&machine_id);
    let endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject
                == machine_service(&machine_id, MachineServiceEndpoint::ContainerRemove)
        })
        .expect("container.remove endpoint exists")
        .clone();
    let mut service = start_nats_service(client.clone(), &spec)
        .await
        .expect("start machine service");
    service
        .bind_endpoint(&endpoint, move |request| {
            let response = handler(request);
            async move { response }
        })
        .await
        .expect("bind container.remove endpoint");
    client.flush().await.expect("flush service subscription");
    service
}

async fn start_substrate_report_service(
    client: async_nats::Client,
    machine_id: &MachineId,
) -> ployz_nats::service_runtime::RunningNatsService {
    let machine_id = machine_id.clone();
    let spec = machine_runtime_service(&machine_id);
    let endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject
                == machine_service(&machine_id, MachineServiceEndpoint::SubstrateReport)
        })
        .expect("substrate.report endpoint exists")
        .clone();
    let mut service = start_nats_service(client.clone(), &spec)
        .await
        .expect("start machine service");
    service
        .bind_endpoint(&endpoint, move |_request| {
            let machine_id = machine_id.clone();
            async move {
                NatsServiceResponse::ok(
                    serde_json::to_vec(&MachineSubstrateReportRpcResponse::Ok(
                        MachineSubstrateReportRpcOk {
                            machine_id,
                            reported: ployz_core::ops::MachineSubstrateVersions {
                                ployzd: Some(
                                    ployz_core::install::InstallArtifactVersion::try_new("0.2.0")
                                        .expect("test version is valid"),
                                ),
                                keeper: None,
                            },
                        },
                    ))
                    .expect("substrate report response encodes"),
                )
            }
        })
        .await
        .expect("bind substrate.report endpoint");
    client.flush().await.expect("flush service subscription");
    service
}

fn decode_run_request(
    request: NatsServiceRequest,
) -> Result<MachineContainerRunRpcRequest, String> {
    serde_json::from_slice(&request.payload).map_err(|error| error.to_string())
}

fn encode_run_response(response: MachineContainerRunRpcResponse) -> Vec<u8> {
    serde_json::to_vec(&response).expect("machine run response encodes")
}

fn decode_remove_request(
    request: NatsServiceRequest,
) -> Result<MachineContainerRemoveRpcRequest, String> {
    serde_json::from_slice(&request.payload).map_err(|error| error.to_string())
}

fn encode_remove_response(response: MachineContainerRemoveRpcResponse) -> Vec<u8> {
    serde_json::to_vec(&response).expect("machine remove response encodes")
}

enum MachineRunContainerResult {
    Ok {
        outcome: MachineRunContainerOutcome,
    },
    DomainError {
        error: MachineContainerRunDomainError,
    },
}

impl MachineRunContainerResult {
    fn into_nats_response(self, machine_id: MachineId) -> NatsServiceResponse {
        match self {
            Self::Ok { outcome } => NatsServiceResponse::ok(encode_run_response(
                MachineContainerRunRpcResponse::Ok(MachineContainerRunRpcOk {
                    machine_id,
                    outcome,
                }),
            )),
            Self::DomainError { error } => NatsServiceResponse::domain_error(encode_run_response(
                MachineContainerRunRpcResponse::DomainError { machine_id, error },
            )),
        }
    }
}

enum MachineRemoveContainerResult {
    Ok {
        container_id: ContainerId,
    },
    DomainError {
        error: MachineContainerRemoveDomainError,
    },
}

impl MachineRemoveContainerResult {
    fn into_nats_response(self, machine_id: MachineId) -> NatsServiceResponse {
        match self {
            Self::Ok { container_id } => NatsServiceResponse::ok(encode_remove_response(
                MachineContainerRemoveRpcResponse::Ok(MachineContainerRpcOk {
                    machine_id,
                    container_id,
                }),
            )),
            Self::DomainError { error } => {
                NatsServiceResponse::domain_error(encode_remove_response(
                    MachineContainerRemoveRpcResponse::DomainError { machine_id, error },
                ))
            }
        }
    }
}

fn remove_request(container_id: &str) -> MachineContainerRemoveRpcRequest {
    MachineContainerRemoveRpcRequest {
        operation_id: operation_id("op_123"),
        container_id: self::container_id(container_id),
        expected_identity: managed_labels().identity(),
    }
}

fn inspect_hint(container_id: &str) -> ployz_core::ops::OperatorHint {
    ployz_core::ops::OperatorHint::try_new(format!("ployz container inspect {container_id}"))
        .expect("valid inspect hint")
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    /// Controller principal: the requesting deploy-worker side.
    client: async_nats::Client,
    /// Machine principals: the machine-service stub side.
    machine_a: async_nats::Client,
}

async fn test_nats() -> TestNats {
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        machine_id("machine_a"),
        machine_id("machine_b"),
    ])
    .await;
    nats.bootstrap_resources().await;
    let client = nats.controller.clone();
    let machine_a = nats.machine_client(&machine_id("machine_a")).await;

    TestNats {
        _nats: nats,
        client,
        machine_a,
    }
}

fn run_request() -> MachineContainerRunRpcRequest {
    MachineContainerRunRpcRequest {
        image: image("registry.example/api:rev_2"),
        endpoint: None,
        container: managed_container_spec(),
    }
}

fn managed_container_spec() -> MachineContainerRunSpec {
    MachineContainerRunSpec {
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_2"),
        operation_id: operation_id("op_123"),
        step_id: step_id("run_1"),
        kind: ManagedContainerKind::Service,
    }
}

fn managed_labels() -> ManagedContainerLabels {
    ManagedContainerLabels {
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_2"),
        operation_id: operation_id("op_123"),
        step_id: step_id("run_1"),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
    }
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

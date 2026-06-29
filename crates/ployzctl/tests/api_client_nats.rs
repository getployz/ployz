use ployz_core::deploy::{DeployRequest, ImageReference, ReplicaCount};
use ployz_core::ids::RevisionId;
use ployz_core::install::InstallArtifactSpec;
use ployz_core::machine::JoinTokenRedeemedAt;
use ployz_core::ops::{
    OperationEventReplayLimit, OperationEventReplayRequest, OperationIdempotencyKey,
};
use ployz_core::roles::InstallRolePolicy;
use ployz_core::state::{
    ActiveMachineState, ActiveServiceState, GatewayServingStatus, GatewayStatusObservation,
    MachinePublicIpObservation,
};
use ployz_core::subjects::{OperationApiEndpoint, OperationApiEndpointExecution};
use ployz_nats::service_runtime::{
    NatsServiceError, NatsServiceErrorCode, NatsServiceResponse, start_nats_service,
};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_sdk_types::{
    AcceptedOperation, DeploySubmitError, DeploySubmitRequest, DeploySubmitResponse,
    MachineAddAccepted, MachineAddRequest, MachineAddResponse, MachineBootstrapUrl,
    MachineInspectRequest, MachineInspectResponse, MachineJoinBundle, MachineJoinRedeemRequest,
    MachineJoinRedeemResponse, MachineJoinRedeemResult, MachineJoinRedeemed, MachineJoinToken,
    MachineListResponse, MachineListResult, MachineName, MachineSnapshot, OperationApiResponse,
    OpsStatusError, OpsStatusResponse, ServiceInspectRequest, ServiceInspectResponse,
    ServiceListResponse, ServiceListResult, ServiceSnapshot,
    operation_api::{
        DeploySubmitApi, MachineAddApi, MachineInspectApi, MachineJoinRedeemApi, MachineListApi,
        OperationApiContract, OpsStatusApi, OpsWatchApi, ServiceInspectApi, ServiceListApi,
    },
};
use ployz_test_support::ids::{event_sequence, machine_id, operation_id};
use ployzctl::api_client::{
    NatsServiceRequestFailure, OperationApiClient, OperationApiClientError,
};
use std::sync::{Arc, OnceLock};
use tokio::sync::{Mutex, OwnedMutexGuard};

static SECURED_API_FIXTURE_LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();

struct SecuredApiFixture {
    _lock: OwnedMutexGuard<()>,
    _nats: ployz_test_support::nats::TestNats,
    service_client: async_nats::Client,
    user_client: async_nats::Client,
}

async fn secured_api_fixture() -> SecuredApiFixture {
    let lock = SECURED_API_FIXTURE_LOCK
        .get_or_init(|| Arc::new(Mutex::new(())))
        .clone()
        .lock_owned()
        .await;
    let nats = ployz_test_support::nats::TestNats::start().await;
    let service_client = nats.controller.clone();
    let user_client = nats.user.clone();
    SecuredApiFixture {
        _lock: lock,
        _nats: nats,
        service_client,
        user_client,
    }
}

#[tokio::test]
async fn operation_api_client_decodes_successful_envelope() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: DeploySubmitResponse = OperationApiResponse::Ok {
                value: AcceptedOperation {
                    operation_id: operation_id("op_123"),
                    watch_subject: "plz.v1.op.op_123.>".to_owned(),
                    start_sequence: event_sequence(1),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let accepted = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect("deploy submit responds");

    assert_eq!(
        accepted,
        AcceptedOperation {
            operation_id: operation_id("op_123"),
            watch_subject: "plz.v1.op.op_123.>".to_owned(),
            start_sequence: event_sequence(1),
        }
    );
}

#[tokio::test]
async fn operation_api_client_routes_machine_add_success() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(MachineAddApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: MachineAddResponse = OperationApiResponse::Ok {
                value: MachineAddAccepted {
                    accepted: AcceptedOperation {
                        operation_id: operation_id("op_machine"),
                        watch_subject: "plz.v1.op.op_machine.>".to_owned(),
                        start_sequence: event_sequence(2),
                    },
                    machine_id: machine_id("machine_2"),
                    bootstrap_url: MachineBootstrapUrl::try_new(
                        ployz_core::install::DEFAULT_MACHINE_BOOTSTRAP_URL,
                    )
                    .expect("valid bootstrap url"),
                    join_bundle: machine_join_bundle(),
                    join_token: MachineJoinToken::try_new("join_token").expect("valid join token"),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let accepted = api
        .machine_add(&machine_add_request())
        .await
        .expect("machine add responds");

    assert_eq!(accepted.machine_id, machine_id("machine_2"));
    assert_eq!(accepted.accepted.operation_id, operation_id("op_machine"));
}

#[tokio::test]
async fn operation_api_client_routes_machine_join_redeem_success() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(MachineJoinRedeemApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: MachineJoinRedeemResponse = OperationApiResponse::Ok {
                value: MachineJoinRedeemed {
                    operation_id: operation_id("op_machine"),
                    machine_id: machine_id("machine_2"),
                    name: MachineName::try_new("edge_2").expect("valid machine name"),
                    roles: InstallRolePolicy::install_all().without_gateway(),
                    join_bundle: machine_join_bundle(),
                    secret_delivery: machine_join_secret_delivery(),
                    joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed timestamp"),
                    last_event_sequence: event_sequence(8),
                    result: MachineJoinRedeemResult::Joined,
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let redeemed = api
        .machine_join_redeem(&machine_join_redeem_request())
        .await
        .expect("machine join redeem responds");

    assert_eq!(redeemed.operation_id, operation_id("op_machine"));
    assert_eq!(redeemed.machine_id, machine_id("machine_2"));
    assert_eq!(redeemed.result, MachineJoinRedeemResult::Joined);
}

#[tokio::test]
async fn operation_api_client_routes_machine_list_success() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(MachineListApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: MachineListResponse = OperationApiResponse::Ok {
                value: MachineListResult {
                    machines: vec![machine_snapshot("machine_2")],
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let result = api
        .machine_list(&ployz_sdk_types::MachineListRequest {})
        .await
        .expect("machine list responds");

    assert_eq!(result.machines, vec![machine_snapshot("machine_2")]);
}

#[tokio::test]
async fn operation_api_client_routes_machine_inspect_success() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(MachineInspectApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |request| async move {
            let request: MachineInspectRequest =
                serde_json::from_slice(&request.payload).expect("machine inspect request decodes");
            assert_eq!(request.machine_id, machine_id("machine_2"));

            let response: MachineInspectResponse = OperationApiResponse::Ok {
                value: machine_snapshot("machine_2"),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let result = api
        .machine_inspect(&MachineInspectRequest {
            machine_id: machine_id("machine_2"),
        })
        .await
        .expect("machine inspect responds");

    assert_eq!(result, machine_snapshot("machine_2"));
}

#[tokio::test]
async fn operation_api_client_routes_service_list_success() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(ServiceListApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: ServiceListResponse = OperationApiResponse::Ok {
                value: ServiceListResult {
                    services: vec![service_snapshot("svc_api", "rev_2")],
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let result = api
        .service_list(&ployz_sdk_types::ServiceListRequest {})
        .await
        .expect("service list responds");

    assert_eq!(result.services, vec![service_snapshot("svc_api", "rev_2")]);
}

#[tokio::test]
async fn operation_api_client_routes_service_inspect_success() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(ServiceInspectApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |request| async move {
            let request: ServiceInspectRequest =
                serde_json::from_slice(&request.payload).expect("service inspect request decodes");
            assert_eq!(
                request.service_id,
                ployz_core::ids::ServiceId::try_new("svc_api").expect("valid service id")
            );

            let response: ServiceInspectResponse = OperationApiResponse::Ok {
                value: service_snapshot("svc_api", "rev_2"),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let result = api
        .service_inspect(&ServiceInspectRequest {
            service_id: ployz_core::ids::ServiceId::try_new("svc_api").expect("valid service id"),
        })
        .await
        .expect("service inspect responds");

    assert_eq!(result, service_snapshot("svc_api", "rev_2"));
}

#[tokio::test]
async fn operation_api_client_returns_service_error_headers_as_transport_failure() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            NatsServiceResponse::transport_error(NatsServiceError::bad_request("bad json"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let error = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect_err("service error returns client error");

    assert_eq!(
        error,
        OperationApiClientError::Service {
            endpoint: OperationApiEndpoint::DeploySubmit,
            failure: NatsServiceError {
                code: NatsServiceErrorCode::BadRequest,
                message: "bad json".to_owned(),
            },
        }
    );
}

#[tokio::test]
async fn operation_api_client_returns_domain_error_envelope_as_domain_failure() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: DeploySubmitResponse = OperationApiResponse::DomainError {
                error: DeploySubmitError::DuplicateSequenceMismatch {
                    operation_id: operation_id("op_123"),
                    sequence: event_sequence(42),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let error = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect_err("domain error envelope returns client domain error");

    assert_eq!(
        error,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::DeploySubmit,
            error: DeploySubmitError::DuplicateSequenceMismatch {
                operation_id: operation_id("op_123"),
                sequence: event_sequence(42),
            },
        }
    );
}

#[tokio::test]
async fn operation_api_client_reports_decode_failure_for_invalid_payload() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            NatsServiceResponse::ok("not json")
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let error = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect_err("invalid payload returns decode error");

    assert!(matches!(
        error,
        OperationApiClientError::DecodeResponse {
            endpoint: OperationApiEndpoint::DeploySubmit,
            ..
        }
    ));
}

#[tokio::test]
async fn operation_api_client_routes_ops_status_domain_errors() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(OpsStatusApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: OpsStatusResponse = OperationApiResponse::DomainError {
                error: OpsStatusError::NoSuchOperation {
                    operation_id: operation_id("op_missing"),
                },
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let error = api
        .ops_status(&ployz_sdk_types::OpsStatusRequest {
            operation_id: operation_id("op_missing"),
        })
        .await
        .expect_err("domain error envelope returns client domain error");

    assert_eq!(
        error,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::OpsStatus,
            error: OpsStatusError::NoSuchOperation {
                operation_id: operation_id("op_missing"),
            },
        }
    );
}

#[tokio::test]
async fn operation_api_client_routes_ops_watch_decode_failures() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let spec = test_api_service(OpsWatchApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(nats.service_client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            NatsServiceResponse::ok(br#"{"status":"ok","value":"wrong shape"}"#.to_vec())
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let error = api
        .ops_watch(&ops_watch_request())
        .await
        .expect_err("invalid watch payload returns decode error");

    assert!(matches!(
        error,
        OperationApiClientError::DecodeResponse {
            endpoint: OperationApiEndpoint::OpsWatch,
            ..
        }
    ));
}

#[tokio::test]
async fn operation_api_client_reports_no_responders_as_typed_request_failure() {
    let nats = secured_api_fixture().await;
    let client = nats.user_client.clone();
    let api = OperationApiClient::new(client);

    let error = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect_err("missing service returns request failure");

    assert_eq!(
        error,
        OperationApiClientError::Request {
            endpoint: OperationApiEndpoint::DeploySubmit,
            failure: NatsServiceRequestFailure::NoResponders,
        }
    );
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

fn deploy_submit_request() -> DeploySubmitRequest {
    DeploySubmitRequest {
        operation_id: operation_id("op_123"),
        target: deploy_target("svc_api"),
    }
}

fn machine_add_request() -> MachineAddRequest {
    MachineAddRequest {
        operation_id: operation_id("op_machine"),
        idempotency_key: OperationIdempotencyKey::try_new("idem_machine")
            .expect("valid idempotency key"),
        machine_id: machine_id("machine_2"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        roles: InstallRolePolicy::install_all().without_gateway(),
    }
}

fn machine_join_redeem_request() -> MachineJoinRedeemRequest {
    MachineJoinRedeemRequest {
        join_token: MachineJoinToken::try_new("join_token").expect("valid join token"),
    }
}

fn machine_join_bundle() -> MachineJoinBundle {
    MachineJoinBundle {
        material: ployz_core::install::MachineJoinMaterial {
            cluster_name: ployz_core::install::MachineJoinClusterName::try_new("prod")
                .expect("valid cluster name"),
            runtime_nats_url: ployz_core::install::MachineJoinRuntimeNatsUrl::try_new(
                "nats://127.0.0.1:7422",
            )
            .expect("valid runtime nats url"),
            trusted_nats: ployz_core::install::MachineJoinTrustedNats {
                ca_pem: ployz_core::nats_config::NatsCaCertificatePem::try_new(
                    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                )
                .expect("valid ca pem"),
            },
            ployzd: InstallArtifactSpec {
                version: ployz_core::install::InstallArtifactVersion::try_new("0.1.0")
                    .expect("valid version"),
                source: ployz_core::install::InstallArtifactSource::try_new("/tmp/ployzd")
                    .expect("valid source"),
                sha256: ployz_core::install::InstallSha256Digest::try_new(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("valid digest"),
                install_path: ployz_core::install::AbsoluteInstallPath::try_new(
                    "/usr/local/bin/ployzd",
                )
                .expect("valid install path"),
            },
            ebpf_bytecode: InstallArtifactSpec {
                version: ployz_core::install::InstallArtifactVersion::try_new("0.1.0")
                    .expect("valid version"),
                source: ployz_core::install::InstallArtifactSource::try_new("/tmp/ployz-ebpf-tc")
                    .expect("valid source"),
                sha256: ployz_core::install::InstallSha256Digest::try_new(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("valid digest"),
                install_path: ployz_core::install::AbsoluteInstallPath::try_new(
                    "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                )
                .expect("valid install path"),
            },
            ebpf_ctl: InstallArtifactSpec {
                version: ployz_core::install::InstallArtifactVersion::try_new("0.1.0")
                    .expect("valid version"),
                source: ployz_core::install::InstallArtifactSource::try_new("/tmp/ployz-ebpf-ctl")
                    .expect("valid source"),
                sha256: ployz_core::install::InstallSha256Digest::try_new(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("valid digest"),
                install_path: ployz_core::install::AbsoluteInstallPath::try_new(
                    "/usr/local/bin/ployz-ebpf-ctl",
                )
                .expect("valid install path"),
            },
        },
    }
}

fn machine_snapshot(machine_id: &str) -> MachineSnapshot {
    let machine_id = self::machine_id(machine_id);
    MachineSnapshot {
        active: ActiveMachineState {
            machine_id: machine_id.clone(),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            activated_by: operation_id("op_machine"),
        },
        public_ip: Some(MachinePublicIpObservation {
            machine_id: machine_id.clone(),
            public_ip: "203.0.113.10".parse().expect("valid public ip"),
        }),
        gateway: Some(GatewayStatusObservation {
            machine_id,
            listen_addr: "127.0.0.1:8080".parse().expect("valid gateway listen addr"),
            serving: GatewayServingStatus::Current,
            route_count: 2,
        }),
        observed_container_count: 3,
    }
}

fn machine_join_secret_delivery() -> ployz_core::install::MachineJoinSecretDelivery {
    ployz_core::install::MachineJoinSecretDelivery {
        nats_credentials: ployz_core::nats_config::NatsUserSeed::try_new(
            "SUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        )
        .expect("valid nats credentials"),
    }
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        service_id: ployz_core::ids::ServiceId::try_new(service_id).expect("valid service id"),
        target_revision: RevisionId::try_new("rev_2").expect("valid revision id"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
        route: None,
    }
}

fn service_snapshot(service_id: &str, revision_id: &str) -> ServiceSnapshot {
    ServiceSnapshot {
        active: ActiveServiceState {
            service_id: ployz_core::ids::ServiceId::try_new(service_id).expect("valid service id"),
            active_revision: RevisionId::try_new(revision_id).expect("valid revision id"),
        },
    }
}

fn ops_watch_request() -> OperationEventReplayRequest {
    OperationEventReplayRequest {
        operation_id: operation_id("op_123"),
        start_sequence: event_sequence(1),
        limit: OperationEventReplayLimit::try_new(10).expect("valid replay limit"),
    }
}

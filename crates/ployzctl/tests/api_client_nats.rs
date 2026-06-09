use ployz_core::deploy::{DeployRequest, ImageReference, ReplicaCount};
use ployz_core::ids::{NodeId, OperationId, OperationOwnerId, RevisionId};
use ployz_core::install::MachineJoinArtifact;
use ployz_core::machine::JoinTokenRedeemedAt;
use ployz_core::ops::{
    EventSequence, OperationEventReplayLimit, OperationEventReplayRequest, OperationIdempotencyKey,
    OperationLeaseExpiresAt, OperationOwnerLease,
};
use ployz_core::roles::FirstNodeGateway;
use ployz_core::state::{
    ActiveMachineState, ActiveServiceState, GatewayServingStatus, GatewayStatusObservation,
    NodePublicIpObservation,
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
    MachineAddAccepted, MachineAddGateway, MachineAddRequest, MachineAddResponse,
    MachineBootstrapUrl, MachineInspectRequest, MachineInspectResponse, MachineJoinBundle,
    MachineJoinPloyzdArtifact, MachineJoinRedeemRequest, MachineJoinRedeemResponse,
    MachineJoinRedeemResult, MachineJoinRedeemed, MachineJoinRuntimeNatsUrl, MachineJoinToken,
    MachineListResponse, MachineListResult, MachineName, MachineSnapshot, OperationApiResponse,
    OpsStatusError, OpsStatusResponse, ServiceInspectRequest, ServiceInspectResponse,
    ServiceListResponse, ServiceListResult, ServiceSnapshot,
    operation_api::{
        DeploySubmitApi, MachineAddApi, MachineInspectApi, MachineJoinRedeemApi, MachineListApi,
        OperationApiContract, OpsStatusApi, OpsWatchApi, ServiceInspectApi, ServiceListApi,
    },
};
use ployzctl::api_client::{
    OperationApiClient, OperationApiClientError, OperationApiRequestFailure,
};

#[tokio::test]
async fn operation_api_client_decodes_successful_envelope() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: DeploySubmitResponse = OperationApiResponse::Ok {
                value: AcceptedOperation {
                    operation_id: operation_id("op_123"),
                    watch_subject: "plz.v1.op.op_123.>".to_owned(),
                    start_sequence: event_sequence(1),
                    owner_lease: operation_lease("op_123", "control", 120),
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
            owner_lease: operation_lease("op_123", "control", 120),
        }
    );
}

#[tokio::test]
async fn operation_api_client_routes_machine_add_success() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(MachineAddApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
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
                        owner_lease: operation_lease("op_machine", "control", 120),
                    },
                    node_id: node_id("node_2"),
                    bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.dev/ployz.sh")
                        .expect("valid bootstrap url"),
                    join_bundle: machine_join_bundle(),
                    runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                        .expect("valid runtime NATS URL"),
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

    assert_eq!(accepted.node_id, node_id("node_2"));
    assert_eq!(accepted.accepted.operation_id, operation_id("op_machine"));
}

#[tokio::test]
async fn operation_api_client_routes_machine_join_redeem_success() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(MachineJoinRedeemApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: MachineJoinRedeemResponse = OperationApiResponse::Ok {
                value: MachineJoinRedeemed {
                    operation_id: operation_id("op_machine"),
                    node_id: node_id("node_2"),
                    name: MachineName::try_new("edge_2").expect("valid machine name"),
                    gateway: FirstNodeGateway::Skip,
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
    assert_eq!(redeemed.node_id, node_id("node_2"));
    assert_eq!(redeemed.result, MachineJoinRedeemResult::Joined);
}

#[tokio::test]
async fn operation_api_client_routes_machine_list_success() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(MachineListApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |_request| async move {
            let response: MachineListResponse = OperationApiResponse::Ok {
                value: MachineListResult {
                    machines: vec![machine_snapshot("node_2")],
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

    assert_eq!(result.machines, vec![machine_snapshot("node_2")]);
}

#[tokio::test]
async fn operation_api_client_routes_machine_inspect_success() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(MachineInspectApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .expect("service starts");

    runtime
        .bind_endpoint(endpoint, |request| async move {
            let request: MachineInspectRequest =
                serde_json::from_slice(&request.payload).expect("machine inspect request decodes");
            assert_eq!(request.node_id, node_id("node_2"));

            let response: MachineInspectResponse = OperationApiResponse::Ok {
                value: machine_snapshot("node_2"),
            };
            NatsServiceResponse::ok(serde_json::to_vec(&response).expect("response serializes"))
        })
        .await
        .expect("endpoint binds");
    let api = OperationApiClient::new(client);

    let result = api
        .machine_inspect(&MachineInspectRequest {
            node_id: node_id("node_2"),
        })
        .await
        .expect("machine inspect responds");

    assert_eq!(result, machine_snapshot("node_2"));
}

#[tokio::test]
async fn operation_api_client_routes_service_list_success() {
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(ServiceListApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
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
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(ServiceInspectApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
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
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
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
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
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
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(DeploySubmitApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
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
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(OpsStatusApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
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
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let spec = test_api_service(OpsWatchApi::ENDPOINT);
    let endpoint = spec.endpoints.first().expect("test endpoint is present");
    let mut runtime = start_nats_service(client.clone(), &spec)
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
    let server = nats_server::run_basic_server();
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let api = OperationApiClient::new(client);

    let error = api
        .deploy_submit(&deploy_submit_request())
        .await
        .expect_err("missing service returns request failure");

    assert_eq!(
        error,
        OperationApiClientError::Request {
            endpoint: OperationApiEndpoint::DeploySubmit,
            failure: OperationApiRequestFailure::NoResponders,
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
        idempotency_key: OperationIdempotencyKey::try_new("idem_1").expect("valid idempotency key"),
        target: deploy_target("svc_api"),
    }
}

fn machine_add_request() -> MachineAddRequest {
    MachineAddRequest {
        operation_id: operation_id("op_machine"),
        idempotency_key: OperationIdempotencyKey::try_new("idem_machine")
            .expect("valid idempotency key"),
        node_id: node_id("node_2"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        gateway: MachineAddGateway::Skip,
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
                server_id: ployz_core::install::MachineJoinTrustedNatsServerId::try_new("server_1")
                    .expect("valid nats server id"),
                config_sha256: ployz_core::install::InstallSha256Digest::try_new(
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                )
                .expect("valid nats config digest"),
            },
            core_iroh: ployz_core::install::MachineJoinCoreIrohEndpoint {
                node_id: ployz_core::ids::NodeId::try_new("core_1").expect("valid core node id"),
                public_key: ployz_core::install::MachineJoinIrohPublicKey::try_new(
                    "core-public-key",
                )
                .expect("valid core iroh public key"),
                direct_addresses: Vec::new(),
                relay_url: None,
            },
            ployzd: MachineJoinPloyzdArtifact {
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
            ebpf_bytecode: MachineJoinArtifact {
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
            ebpf_ctl: MachineJoinArtifact {
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

fn machine_snapshot(node_id: &str) -> MachineSnapshot {
    let node_id = self::node_id(node_id);
    MachineSnapshot {
        active: ActiveMachineState {
            node_id: node_id.clone(),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            activated_by: operation_id("op_machine"),
        },
        public_ip: Some(NodePublicIpObservation {
            node_id: node_id.clone(),
            public_ip: "203.0.113.10".parse().expect("valid public ip"),
        }),
        gateway: Some(GatewayStatusObservation {
            node_id,
            listen_addr: "127.0.0.1:8080".parse().expect("valid gateway listen addr"),
            serving: GatewayServingStatus::Current,
            route_count: 2,
        }),
        observed_container_count: 3,
    }
}

fn machine_join_secret_delivery() -> ployz_core::install::MachineJoinSecretDelivery {
    ployz_core::install::MachineJoinSecretDelivery {
        nats_credentials: ployz_core::install::MachineJoinNatsCredentials::try_new(
            "user-jwt-and-seed",
        )
        .expect("valid nats credentials"),
        core_iroh_ticket: ployz_core::install::MachineJoinIrohTicket::try_new("core-ticket")
            .expect("valid core iroh ticket"),
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

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn operation_lease(operation_id: &str, owner_id: &str, expires_at: u64) -> OperationOwnerLease {
    OperationOwnerLease::new(
        OperationId::try_new(operation_id).expect("valid operation id"),
        OperationOwnerId::try_new(owner_id).expect("valid owner id"),
        OperationLeaseExpiresAt::try_new(expires_at).expect("valid lease expiry"),
    )
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn ops_watch_request() -> OperationEventReplayRequest {
    OperationEventReplayRequest {
        operation_id: operation_id("op_123"),
        start_sequence: event_sequence(1),
        limit: OperationEventReplayLimit::try_new(10).expect("valid replay limit"),
    }
}

use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use ployz_core::deploy::{DeployRequest, ImageReference, ReplicaCount};
use ployz_core::ids::{NodeId, OperationId, RevisionId, ServiceId};
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
    MachineBootstrapUrl, MachineJoinArtifact, MachineJoinClusterName, MachineJoinCoreIrohEndpoint,
    MachineJoinIrohPublicKey, MachineJoinIrohTicket, MachineJoinMaterial,
    MachineJoinNatsCredentials, MachineJoinPloyzdArtifact, MachineJoinRuntimeNatsUrl,
    MachineJoinSecretDelivery, MachineJoinTemplate, MachineJoinTrustedNats,
    MachineJoinTrustedNatsServerId,
};
use ployz_core::ops::{
    DeployOperationState, EventSequence, OperationIdempotencyKey, OperationStatus,
};
use ployz_core::state::{
    ActiveServiceCommitRequest, ExpectedActiveService, GatewayServingStatus,
    GatewayStatusObservation, NodePublicIpObservation,
};
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_sdk_types::{
    DeploySubmitRequest, MachineAddGateway, MachineAddRequest, MachineInspectRequest,
    MachineJoinRedeemRequest, MachineJoinReportOutcome, MachineJoinReportRequest,
    MachineListRequest, OpsStatusRequest, ServiceInspectRequest, ServiceListRequest,
};
use ployzd::config::ControlProcessConfig;
use ployzd::controllers::MachineAddBootstrapConfig;
use ployzd::nats_process::NatsServerRuntime;
use ployzd::node_runtime::start_node_runtime_with_ports;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

mod support;

#[tokio::test]
async fn control_runtime_bootstraps_nats_and_serves_operation_api() {
    let nats = TestNats::start().await;
    let config = control_config();
    let runtime =
        ployzd::control_runtime::start_control_runtime_with_client(nats.client.clone(), &config)
            .await
            .expect("control runtime starts");
    let api = OperationApiClient::new(nats.client.clone());

    let accepted = api
        .deploy_submit(&DeploySubmitRequest {
            operation_id: operation_id("op_control_runtime"),
            target: deploy_target("svc_api"),
            idempotency_key: idempotency_key("idem_control_runtime"),
        })
        .await
        .expect("operation API accepts deploy");

    assert_eq!(accepted.operation_id, operation_id("op_control_runtime"));
    assert_eq!(accepted.start_sequence, event_sequence(1));
    nats.jetstream
        .get_key_value("KV_CORE")
        .await
        .expect("control runtime created KV_CORE");
    nats.jetstream
        .get_key_value("KV_OPS")
        .await
        .expect("control runtime created KV_OPS");
    nats.jetstream
        .get_stream("PLZ_OPS")
        .await
        .expect("control runtime created PLZ_OPS");
    nats.jetstream
        .get_object_store("PLZ_BUNDLES")
        .await
        .expect("control runtime created PLZ_BUNDLES");

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_uses_configured_machine_bootstrap_url() {
    let nats = TestNats::start().await;
    let config = control_config().with_machine_bootstrap(
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new("https://example.test/ployz.sh")
                .expect("valid bootstrap url"),
        )
        .with_join_template(machine_join_template()),
    );
    let runtime =
        ployzd::control_runtime::start_control_runtime_with_client(nats.client.clone(), &config)
            .await
            .expect("control runtime starts");
    let api = OperationApiClient::new(nats.client.clone());

    let accepted = api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id("op_machine"),
            idempotency_key: idempotency_key("idem_machine"),
            node_id: node_id("node_2"),
            name: ployz_sdk_types::MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: MachineAddGateway::Skip,
        })
        .await
        .expect("machine add succeeds");

    assert_eq!(
        accepted.bootstrap_url.as_str(),
        "https://example.test/ployz.sh"
    );
    let retry = api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id("op_machine_retry"),
            idempotency_key: idempotency_key("idem_machine"),
            node_id: node_id("node_2"),
            name: ployz_sdk_types::MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: MachineAddGateway::Skip,
        })
        .await
        .expect("machine add retry succeeds");
    assert_eq!(retry.accepted.operation_id, operation_id("op_machine"));
    assert_eq!(retry.join_token, accepted.join_token);

    let redeemed = api
        .machine_join_redeem(&MachineJoinRedeemRequest {
            join_token: accepted.join_token.clone(),
        })
        .await
        .expect("join token redeems");
    assert_eq!(redeemed.node_id, node_id("node_2"));

    api.machine_join_report(&MachineJoinReportRequest {
        join_token: accepted.join_token.clone(),
        outcome: MachineJoinReportOutcome::Completed,
    })
    .await
    .expect("join completion reports");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");
    observations
        .replace_node_public_ip(&NodePublicIpObservation {
            node_id: node_id("node_2"),
            public_ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2)),
        })
        .await
        .expect("public ip stores");
    observations
        .replace_gateway_status(&GatewayStatusObservation {
            node_id: node_id("node_2"),
            listen_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            serving: GatewayServingStatus::Current,
            route_count: 0,
        })
        .await
        .expect("gateway status stores");

    let inspected = api
        .machine_inspect(&MachineInspectRequest {
            node_id: node_id("node_2"),
        })
        .await
        .expect("machine inspects");
    assert_eq!(inspected.active.node_id, node_id("node_2"));
    assert_eq!(inspected.active.name.as_str(), "edge_2");
    assert_eq!(
        inspected
            .public_ip
            .as_ref()
            .expect("public ip exists")
            .public_ip,
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2))
    );
    assert_eq!(
        inspected
            .gateway
            .as_ref()
            .expect("gateway status exists")
            .serving,
        GatewayServingStatus::Current
    );
    assert_eq!(
        api.machine_list(&MachineListRequest {})
            .await
            .expect("machines list")
            .machines,
        vec![inspected]
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_serves_active_service_queries() {
    let nats = TestNats::start().await;
    let config = control_config();
    let runtime =
        ployzd::control_runtime::start_control_runtime_with_client(nats.client.clone(), &config)
            .await
            .expect("control runtime starts");
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");
    core_state
        .commit_active_service(&ActiveServiceCommitRequest {
            service_id: service_id("svc_api"),
            expected_current: ExpectedActiveService::Absent,
            target_revision: revision_id("rev_2"),
        })
        .await
        .expect("service state stores");
    let api = OperationApiClient::new(nats.client.clone());

    let listed = api
        .service_list(&ServiceListRequest {})
        .await
        .expect("services list");
    let [service] = listed.services.as_slice() else {
        panic!("expected one listed service, got {:?}", listed.services);
    };
    assert_eq!(service.active.service_id, service_id("svc_api"));
    assert_eq!(service.active.active_revision, revision_id("rev_2"));

    let inspected = api
        .service_inspect(&ServiceInspectRequest {
            service_id: service_id("svc_api"),
        })
        .await
        .expect("service inspects");
    assert_eq!(inspected.active.service_id, service_id("svc_api"));
    assert_eq!(inspected.active.active_revision, revision_id("rev_2"));

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_refuses_machine_add_without_join_template() {
    let nats = TestNats::start().await;
    let result = ployzd::control_runtime::start_control_runtime_with_client(
        nats.client.clone(),
        &control_config_without_join_template(),
    )
    .await;

    assert!(matches!(
        result,
        Err(ployzd::control_runtime::ControlRuntimeError::MissingMachineJoinTemplate)
    ));
}

#[tokio::test]
async fn control_runtime_runs_deploy_submit_and_commits_active_state() {
    let nats = TestNats::start().await;
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect_err("control has not bootstrapped observations yet");
    assert!(matches!(
        observations,
        ployz_nats::observations::ObservationStoreError::OpenBucket { .. }
    ));

    let config = control_config()
        .with_deploy_nodes(vec![node_id("node_a")])
        .with_deploy_step_timeout(Duration::from_secs(2));
    let runtime =
        ployzd::control_runtime::start_control_runtime_with_client(nats.client.clone(), &config)
            .await
            .expect("control runtime starts");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");
    let node_runtime = start_node_runtime_with_ports(
        nats.client.clone(),
        node_id("node_a"),
        support::ObservingContainerRunner::new(node_id("node_a"), observations.clone()),
        support::ReadyWireGuardEbpf,
        support::ObservingContainerRunner::new(node_id("node_a"), observations.clone()),
    )
    .await
    .expect("node runtime starts");
    let api = OperationApiClient::new(nats.client.clone());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_run"),
        target: deploy_target("svc_api"),
        idempotency_key: idempotency_key("idem_run"),
    };

    let accepted = api
        .deploy_submit(&request)
        .await
        .expect("operation API accepts deploy");

    assert_eq!(accepted.operation_id, operation_id("op_run"));
    let status = wait_for_terminal_deploy_status(&api, operation_id("op_run")).await;
    assert!(matches!(
        status,
        OperationStatus::Deploy {
            state: DeployOperationState::Completed,
            ..
        }
    ));
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");
    assert_eq!(
        core_state
            .active_service(&service_id("svc_api"))
            .await
            .expect("active service reads")
            .expect("active service committed")
            .active_revision,
        revision_id("rev_2")
    );
    let duplicate = api
        .deploy_submit(&request)
        .await
        .expect("duplicate operation API submit returns original operation");
    assert_eq!(duplicate.operation_id, operation_id("op_run"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        observations
            .node_snapshot(&node_id("node_a"))
            .await
            .expect("node observations read")
            .expect("node snapshot exists")
            .containers()
            .len(),
        1
    );

    node_runtime
        .shutdown()
        .await
        .expect("node runtime shuts down");
    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_refuses_bootstrap_resource_drift() {
    let nats = TestNats::start().await;
    nats.jetstream
        .create_stream(jetstream::stream::Config {
            name: "PLZ_OPS".to_owned(),
            subjects: vec!["wrong.>".to_owned()],
            storage: StorageType::File,
            ..Default::default()
        })
        .await
        .expect("create drifted PLZ_OPS stream");

    let config = control_config();
    let error = match ployzd::control_runtime::start_control_runtime_with_client(
        nats.client.clone(),
        &config,
    )
    .await
    {
        Ok(runtime) => {
            runtime.shutdown().await.expect("unexpected runtime stops");
            panic!("control runtime should refuse drift");
        }
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ployzd::control_runtime::ControlRuntimeError::AssureBootstrap(
            ployz_nats::bootstrap::BootstrapAssuranceError::RefuseResource { .. }
        )
    ));
}

struct TestNats {
    _server: nats_server::Server,
    client: async_nats::Client,
    jetstream: jetstream::Context,
}

impl TestNats {
    async fn start() -> Self {
        let config = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ployz-nats/tests/configs/jetstream.conf"
        );
        let server = nats_server::run_server(config);
        let client = async_nats::connect(server.client_url())
            .await
            .expect("connect to test nats");
        let jetstream = jetstream::new(client.clone());

        Self {
            _server: server,
            client,
            jetstream,
        }
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn control_config() -> ControlProcessConfig {
    control_config_without_join_template().with_machine_bootstrap(
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new("https://get.ployz.dev/ployz.sh")
                .expect("valid bootstrap url"),
        )
        .with_join_template(machine_join_template()),
    )
}

fn control_config_without_join_template() -> ControlProcessConfig {
    ControlProcessConfig::new(
        NatsServerRuntime::External(NatsClientUrl::loopback(4222)),
        ployz_core::ids::NodeId::try_new("core_1").expect("valid node id"),
    )
}

fn machine_join_template() -> MachineJoinTemplate {
    MachineJoinTemplate {
        join_bundle: ployz_core::install::MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                    .expect("valid runtime nats url"),
                trusted_nats: MachineJoinTrustedNats {
                    server_id: MachineJoinTrustedNatsServerId::try_new("server_1")
                        .expect("valid nats server id"),
                    config_sha256: InstallSha256Digest::try_new(
                        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    )
                    .expect("valid nats config digest"),
                },
                core_iroh: MachineJoinCoreIrohEndpoint {
                    node_id: NodeId::try_new("core_1").expect("valid core node id"),
                    public_key: MachineJoinIrohPublicKey::try_new("core-public-key")
                        .expect("valid core iroh public key"),
                    direct_addresses: Vec::new(),
                    relay_url: None,
                },
                ployzd: MachineJoinPloyzdArtifact {
                    version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                    source: InstallArtifactSource::try_new("/tmp/ployzd").expect("valid source"),
                    sha256: InstallSha256Digest::try_new(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid digest"),
                    install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployzd")
                        .expect("valid install path"),
                },
                ebpf_bytecode: MachineJoinArtifact {
                    version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                    source: InstallArtifactSource::try_new("/tmp/ployz-ebpf-tc")
                        .expect("valid source"),
                    sha256: InstallSha256Digest::try_new(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid digest"),
                    install_path: AbsoluteInstallPath::try_new(
                        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                    )
                    .expect("valid install path"),
                },
                ebpf_ctl: MachineJoinArtifact {
                    version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                    source: InstallArtifactSource::try_new("/tmp/ployz-ebpf-ctl")
                        .expect("valid source"),
                    sha256: InstallSha256Digest::try_new(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid digest"),
                    install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployz-ebpf-ctl")
                        .expect("valid install path"),
                },
            },
        },
        secret_delivery: MachineJoinSecretDelivery {
            nats_credentials: MachineJoinNatsCredentials::try_new("user-jwt-and-seed")
                .expect("valid nats credentials"),
            core_iroh_ticket: MachineJoinIrohTicket::try_new("core-ticket")
                .expect("valid core iroh ticket"),
        },
    }
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        service_id: self::service_id(service_id),
        target_revision: revision_id("rev_2"),
        image: image("ghcr.io/acme/api:rev-2"),
        replicas: replicas(1),
        route: None,
    }
}

async fn wait_for_terminal_deploy_status(
    api: &OperationApiClient,
    operation_id: OperationId,
) -> OperationStatus {
    for _ in 0..80 {
        let status = api
            .ops_status(&OpsStatusRequest {
                operation_id: operation_id.clone(),
            })
            .await
            .expect("status is readable")
            .status;
        let OperationStatus::Deploy { state, .. } = &status else {
            panic!("expected deploy status");
        };
        if state.is_terminal() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    panic!("deploy did not reach terminal status");
}

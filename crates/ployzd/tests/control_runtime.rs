//! Control runtime against a secured (TLS + NKey-authorized) NATS server.
//!
//! Machine-add credential minting runs as bounded operation work: these
//! tests drive the full mint → render → reload → verify → material-ready
//! sequence against the fixture's real `nats-server`, including the
//! ADR-0015 single-writer fence and the ADR-0001 authority-file
//! durability rules.

use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use ployz_core::deploy::{DeployRequest, DeployRoute, ImageReference, ReplicaCount};
use ployz_core::ids::{NodeId, OperationId, RevisionId, ServiceId};
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
    MachineBootstrapUrl, MachineJoinArtifact, MachineJoinClusterName, MachineJoinMaterial,
    MachineJoinPloyzdArtifact, MachineJoinRuntimeNatsUrl, MachineJoinTemplate,
    MachineJoinTrustedNats,
};
use ployz_core::machine::{MachineAddFailure, MachineAddOperationState};
use ployz_core::nats_config::{
    NatsCaCertificatePem, NatsServerName, NatsUserPublicKey, parse_authorized_users,
    render_authorized_users,
};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, EventSequence, OperationIdempotencyKey,
    OperationStatus, RouteHostname, RoutePort, RouteTarget,
};
use ployz_core::security::NatsPrincipal;
use ployz_core::state::{
    ActiveMachineState, ActiveServiceCommitRequest, ExpectedActiveService, GatewayServingStatus,
    GatewayStatusObservation, NodePublicIpObservation,
};
use ployz_nats::connect::connect_authenticated;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    DeploySubmitRequest, MachineAddAccepted, MachineAddGateway, MachineAddRequest,
    MachineInspectRequest, MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineJoinToken, MachineListRequest,
    OpsStatusRequest, ServiceInspectRequest, ServiceListRequest,
};
use ployz_test_support::nats::SecuredTestNats;
use ployzd::config::{ControlNatsAuthorizationConfig, ControlProcessConfig};
use ployzd::controllers::MachineAddBootstrapConfig;
use ployzd::gateway_process_runtime::start_gateway_process_runtime_with_client;
use ployzd::nats_authorization::{
    NatsReloadEvidence, NatsReloadOutcome, NatsReloadRunner, SignalNatsReloadRunner,
};
use ployzd::nats_process::NatsServerRuntime;
use ployzd::node_runtime::start_node_runtime_with_ports;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

mod support;

const FIXTURE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn control_runtime_bootstraps_nats_and_serves_operation_api() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

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
async fn control_runtime_does_not_mutate_machine_state_on_startup() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");

    assert!(
        core_state
            .active_machines()
            .await
            .expect("active machines read")
            .is_empty()
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// The machine-add handler returns its operation id + join material before
/// any reload occurs; minting runs as owned operation work afterwards and
/// produces a usable per-machine NKey seed.
#[tokio::test]
async fn machine_add_accepts_before_reload_then_mints_material() {
    let nats = TestNats::start().await;
    let reload = RecordingReload::gated_signal(nats.nats.server_pid());
    let config = nats.control_config();
    let runtime = nats
        .start_control_with_reload(&config, reload.clone())
        .await;
    let api = nats.api();

    let accepted = machine_add(&api, "op_machine", "idem_machine", "node_2").await;

    // The handler returned while the (gated) reload had not run: accepting
    // is fast and never includes render/reload/verify work.
    assert_eq!(reload.outcomes().len(), 0);
    reload.release();

    let redeemed = redeem_when_ready(&api, &accepted.join_token).await;
    assert!(
        redeemed
            .secret_delivery
            .nats_credentials
            .secret()
            .starts_with("SU"),
        "minted material is an NKey user seed"
    );
    assert!(!reload.outcomes().is_empty(), "reload runner was invoked");

    // Idempotent replay returns the already-issued token and the
    // already-minted material — it never mints twice.
    let retry = api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id("op_machine_retry"),
            idempotency_key: idempotency_key("idem_machine"),
            node_id: node_id("node_2"),
            name: ployz_sdk_types::MachineName::try_new("node_2").expect("valid machine name"),
            gateway: MachineAddGateway::Skip,
        })
        .await
        .expect("machine add retry succeeds");
    assert_eq!(retry.join_token, accepted.join_token);
    let replayed = redeem_when_ready(&api, &accepted.join_token).await;
    assert_eq!(
        replayed.secret_delivery.nats_credentials.secret(),
        redeemed.secret_delivery.nats_credentials.secret(),
        "replay returns the original minted material"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// Two machine-adds mint distinct credentials.
#[tokio::test]
async fn machine_add_mints_unique_credentials_per_machine() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    let first = machine_add(&api, "op_machine_a", "idem_machine_a", "node_a").await;
    let second = machine_add(&api, "op_machine_b", "idem_machine_b", "node_b").await;
    let first_redeemed = redeem_when_ready(&api, &first.join_token).await;
    let second_redeemed = redeem_when_ready(&api, &second.join_token).await;

    assert_ne!(
        first_redeemed.secret_delivery.nats_credentials.secret(),
        second_redeemed.secret_delivery.nats_credentials.secret(),
        "each machine gets its own minted seed"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// ADR-0015 fence: two concurrent machine-adds queue their renders through
/// the single writer; both complete and both public keys are present in
/// the rendered `authorized-users.conf`.
#[tokio::test]
async fn concurrent_machine_adds_both_render_their_keys() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    let (left, right) = tokio::join!(
        machine_add(&api, "op_fence_a", "idem_fence_a", "node_fa"),
        machine_add(&api, "op_fence_b", "idem_fence_b", "node_fb"),
    );
    let left_redeemed = redeem_when_ready(&api, &left.join_token).await;
    let right_redeemed = redeem_when_ready(&api, &right.join_token).await;

    let rendered = std::fs::read_to_string(nats.nats.authorized_users_path())
        .expect("authorized-users file is readable");
    let users = parse_authorized_users(&rendered).expect("rendered authority file parses");
    let left_key = public_key_of(left_redeemed.secret_delivery.nats_credentials.secret());
    let right_key = public_key_of(right_redeemed.secret_delivery.nats_credentials.secret());
    assert!(
        rendered_principal_key(&users, "node_fa") == Some(left_key),
        "node_fa's minted public key is rendered"
    );
    assert!(
        rendered_principal_key(&users, "node_fb") == Some(right_key),
        "node_fb's minted public key is rendered"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// ADR-0001 durability: with a pre-existing file containing an unknown
/// user and an empty KV set, startup adopts the entry and a subsequent
/// render does not shrink the file.
#[tokio::test]
async fn startup_adopts_existing_authorized_users_and_renders_never_shrink() {
    let nats = TestNats::start().await;
    // Plant an unknown principal in the recovery-evidence file before
    // control ever runs (KV is empty: fresh JetStream).
    let ghost = nkeys::KeyPair::new_user();
    let ghost_public =
        NatsUserPublicKey::try_new(ghost.public_key()).expect("generated public key is valid");
    let existing = std::fs::read_to_string(nats.nats.authorized_users_path())
        .expect("fixture authority file is readable");
    let mut users = parse_authorized_users(&existing).expect("fixture authority file parses");
    users.push(ployz_core::nats_config::NatsAuthorizedUser {
        principal: NatsPrincipal::Node {
            node_id: node_id("node_ghost"),
        },
        nkey_public: ghost_public.clone(),
    });
    std::fs::write(
        nats.nats.authorized_users_path(),
        render_authorized_users(&users),
    )
    .expect("authority file with unknown user writes");

    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    // Trigger a render through a real machine-add.
    let accepted = machine_add(&api, "op_adopt", "idem_adopt", "node_new").await;
    redeem_when_ready(&api, &accepted.join_token).await;

    let rendered = std::fs::read_to_string(nats.nats.authorized_users_path())
        .expect("authorized-users file is readable");
    let users = parse_authorized_users(&rendered).expect("rendered authority file parses");
    assert_eq!(
        rendered_principal_key(&users, "node_ghost"),
        Some(ghost_public),
        "adopted unknown user survives the render (never shrink)"
    );
    assert!(
        rendered_principal_key(&users, "node_new").is_some(),
        "minted user is rendered alongside the adopted one"
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// Reload failure is a typed terminal failure with command evidence —
/// not a retry loop.
#[tokio::test]
async fn machine_add_reload_failure_is_a_typed_terminal_failure() {
    let nats = TestNats::start().await;
    let reload = RecordingReload::failing();
    let config = nats.control_config();
    let runtime = nats
        .start_control_with_reload(&config, reload.clone())
        .await;
    let api = nats.api();

    machine_add(&api, "op_reload_fail", "idem_reload_fail", "node_rf").await;
    let status = wait_for_terminal_machine_add_status(&api, operation_id("op_reload_fail")).await;

    let OperationStatus::MachineAdd {
        state: MachineAddOperationState::Failed { failure },
        ..
    } = status
    else {
        panic!("expected failed machine add, got {status:?}");
    };
    let MachineAddFailure::NatsReloadFailed { message } = failure else {
        panic!("expected typed reload failure, got {failure:?}");
    };
    assert!(
        message.as_str().contains("reload refused by test"),
        "failure carries the reload command evidence: {message:?}"
    );
    assert!(!reload.outcomes().is_empty(), "reload runner was invoked");

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

/// Redeem before the mint worker reaches material-ready is the typed
/// not-ready response, and the keeper-style bounded retry succeeds later.
#[tokio::test]
async fn machine_join_redeem_waits_for_material_ready() {
    let nats = TestNats::start().await;
    let reload = RecordingReload::gated_signal(nats.nats.server_pid());
    let config = nats.control_config();
    let runtime = nats
        .start_control_with_reload(&config, reload.clone())
        .await;
    let api = nats.api();

    let accepted = machine_add(&api, "op_wait", "idem_wait", "node_w").await;
    let not_ready = api
        .machine_join_redeem(&MachineJoinRedeemRequest {
            join_token: accepted.join_token.clone(),
        })
        .await
        .expect_err("redeem before material-ready is refused");
    assert!(
        matches!(
            not_ready,
            OperationApiClientError::Domain {
                error: MachineJoinRedeemError::MaterialNotReady { ref operation_id },
                ..
            } if operation_id == &accepted.accepted.operation_id
        ),
        "expected typed not-ready, got {not_ready:?}"
    );

    reload.release();
    redeem_when_ready(&api, &accepted.join_token).await;

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_uses_configured_machine_bootstrap_url() {
    let nats = TestNats::start().await;
    let config = nats.control_config().with_machine_bootstrap(
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new("https://example.test/ployz.sh")
                .expect("valid bootstrap url"),
        )
        .with_join_template(machine_join_template(&nats)),
    );
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

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

    let redeemed = redeem_when_ready(&api, &accepted.join_token).await;
    assert_eq!(redeemed.node_id, node_id("node_2"));

    api.machine_join_report(&MachineJoinReportRequest {
        join_token: accepted.join_token.clone(),
        outcome: MachineJoinReportOutcome::Completed,
    })
    .await
    .expect("join completion reports");
    // The minted per-machine seed is a working Node credential: connect
    // with it and publish this machine's observations.
    let minted_seed = ployz_core::nats_config::NatsUserSeed::try_new(
        redeemed.secret_delivery.nats_credentials.secret(),
    )
    .expect("minted material is a valid seed");
    let minted_client = connect_authenticated(
        &nats.nats.config_with_seed(
            NatsPrincipal::Node {
                node_id: node_id("node_2"),
            },
            minted_seed,
        ),
        FIXTURE_CONNECT_TIMEOUT,
    )
    .await
    .expect("minted node credential connects");
    let node_jetstream = jetstream::new(minted_client);
    let observations = AsyncNatsObservationStore::from_jetstream(&node_jetstream)
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
    let machines = api
        .machine_list(&MachineListRequest {})
        .await
        .expect("machines list")
        .machines;
    assert_eq!(machines, vec![inspected]);

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_serves_active_service_queries() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
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
    let api = nats.api();

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
    let result = ployzd::control_runtime::start_control_runtime_with_client_and_reload(
        nats.client.clone(),
        &nats.control_config_without_join_template(),
        nats.reload_runner(),
    )
    .await;

    assert!(matches!(
        result,
        Err(ployzd::control_runtime::ControlRuntimeError::MissingMachineJoinTemplate)
    ));
}

#[tokio::test]
async fn control_runtime_runs_deploy_submit_and_commits_active_state() {
    let nats = TestNats::start_with_nodes(&[node_id("node_a")]).await;
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect_err("control has not bootstrapped observations yet");
    assert!(matches!(
        observations,
        ployz_nats::observations::ObservationStoreError::OpenBucket { .. }
    ));

    let config = nats
        .control_config()
        .with_deploy_nodes(vec![node_id("node_a")])
        .with_deploy_step_timeout(Duration::from_secs(2));
    let runtime = nats.start_control(&config).await;
    let node_client = nats.node_client(&node_id("node_a")).await;
    let node_jetstream = jetstream::new(node_client.clone());
    let observations = AsyncNatsObservationStore::from_jetstream(&node_jetstream)
        .await
        .expect("open observations");
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");
    core_state
        .replace_active_machine(&active_machine("node_a"))
        .await
        .expect("active machine stores");
    let node_runtime = start_node_runtime_with_ports(
        node_client.clone(),
        node_id("node_a"),
        support::ObservingContainerRunner::new(node_id("node_a"), observations.clone()),
        support::ReadyWireGuardEbpf,
        support::ObservingContainerRunner::new(node_id("node_a"), observations.clone()),
    )
    .await
    .expect("node runtime starts");
    let api = nats.api();
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
    assert!(
        matches!(
            status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
                ..
            }
        ),
        "expected deploy to complete, got {status:?}"
    );
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
async fn control_runtime_routed_deploy_serves_through_gateway() {
    let nats = TestNats::start_with_nodes(&[node_id("node_a"), node_id("node_gateway")]).await;
    let config = nats
        .control_config()
        .with_deploy_nodes(vec![node_id("node_a")])
        .with_deploy_step_timeout(Duration::from_secs(2));
    let runtime = nats.start_control(&config).await;
    let node_client = nats.node_client(&node_id("node_a")).await;
    let node_jetstream = jetstream::new(node_client.clone());
    let observations = AsyncNatsObservationStore::from_jetstream(&node_jetstream)
        .await
        .expect("open observations");
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");
    core_state
        .replace_active_machine(&active_machine("node_a"))
        .await
        .expect("active machine stores");
    observations
        .replace_node_public_ip(&NodePublicIpObservation {
            node_id: node_id("node_a"),
            public_ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
        })
        .await
        .expect("node public ip stores");
    let node_runtime = start_node_runtime_with_ports(
        node_client.clone(),
        node_id("node_a"),
        support::ObservingContainerRunner::new(node_id("node_a"), observations.clone()),
        support::ReadyWireGuardEbpf,
        support::ObservingContainerRunner::new(node_id("node_a"), observations.clone()),
    )
    .await
    .expect("node runtime starts");
    let gateway_client = nats.node_client(&node_id("node_gateway")).await;
    let gateway = start_gateway_process_runtime_with_client(
        gateway_client,
        Duration::from_millis(10),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        node_id("node_gateway"),
    )
    .await
    .expect("gateway runtime starts");
    let upstream = support::TestHttpUpstream::start("smoke").await;
    let api = nats.api();

    let accepted = api
        .deploy_submit(&DeploySubmitRequest {
            operation_id: operation_id("op_routed"),
            target: deploy_target_with_route(
                "svc_api",
                gateway.listen_addr().port(),
                upstream.port(),
            ),
            idempotency_key: idempotency_key("idem_routed"),
        })
        .await
        .expect("operation API accepts routed deploy");

    assert_eq!(accepted.operation_id, operation_id("op_routed"));
    let status = wait_for_terminal_deploy_status(&api, operation_id("op_routed")).await;
    assert!(
        matches!(
            status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
                ..
            }
        ),
        "expected routed deploy to complete, got {status:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ready = gateway.served_projection().is_some_and(|projection| {
            matches!(
                projection.routes.as_slice(),
                [route] if !route.upstreams.is_empty()
            )
        });
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "gateway never served the routed projection; health: {:?}",
            gateway.health()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut client = TcpStream::connect(gateway.listen_addr())
        .await
        .expect("connect gateway");
    client
        .write_all(b"GET /smoke HTTP/1.1\r\nHost: api.example.com\r\n\r\n")
        .await
        .expect("write gateway request");
    client.shutdown().await.expect("finish gateway request");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read gateway response");

    assert_eq!(
        response,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    assert_eq!(
        upstream.request().await,
        "GET /smoke HTTP/1.1\r\nHost: api.example.com\r\n\r\n"
    );

    gateway.shutdown().await;
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

    let config = nats.control_config();
    let error = match ployzd::control_runtime::start_control_runtime_with_client_and_reload(
        nats.client.clone(),
        &config,
        nats.reload_runner(),
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
    nats: SecuredTestNats,
    client: async_nats::Client,
    user_client: async_nats::Client,
    jetstream: jetstream::Context,
    work_dir: tempfile::TempDir,
}

impl TestNats {
    async fn start() -> Self {
        Self::start_with_nodes(&[]).await
    }

    async fn start_with_nodes(node_ids: &[NodeId]) -> Self {
        let nats = SecuredTestNats::start_with_nodes(node_ids)
            .await
            .expect("secured test nats starts");
        let client = connect_authenticated(&nats.controller_config(), FIXTURE_CONNECT_TIMEOUT)
            .await
            .expect("controller connects");
        let user_client = connect_authenticated(&nats.user_config(), FIXTURE_CONNECT_TIMEOUT)
            .await
            .expect("operator connects");
        let jetstream = jetstream::new(client.clone());
        let work_dir = tempfile::TempDir::new().expect("test work dir creates");

        Self {
            nats,
            client,
            user_client,
            jetstream,
            work_dir,
        }
    }

    fn api(&self) -> OperationApiClient {
        OperationApiClient::new(self.user_client.clone())
    }

    async fn node_client(&self, node_id: &NodeId) -> async_nats::Client {
        let config = self
            .nats
            .node_config(node_id)
            .expect("fixture knows the node user");
        connect_authenticated(&config, FIXTURE_CONNECT_TIMEOUT)
            .await
            .expect("node connects")
    }

    fn reload_runner(&self) -> RecordingReload {
        RecordingReload::signal(self.nats.server_pid())
    }

    fn control_config(&self) -> ControlProcessConfig {
        self.control_config_without_join_template()
            .with_machine_bootstrap(
                MachineAddBootstrapConfig::new(
                    MachineBootstrapUrl::try_new("https://get.ployz.dev/ployz.sh")
                        .expect("valid bootstrap url"),
                )
                .with_join_template(machine_join_template(self)),
            )
    }

    fn control_config_without_join_template(&self) -> ControlProcessConfig {
        ControlProcessConfig::new(
            NatsServerRuntime::External(self.nats.client_url().clone()),
            node_id("core_1"),
            self.nats.controller_config(),
        )
        .with_nats_authorization(ControlNatsAuthorizationConfig {
            authorized_users_file: self.nats.authorized_users_path().to_path_buf(),
            node_seed_file: self.work_dir.path().join("node.seed"),
        })
    }

    async fn start_control(
        &self,
        config: &ControlProcessConfig,
    ) -> ployzd::control_runtime::RunningControlRuntime {
        self.start_control_with_reload(config, self.reload_runner())
            .await
    }

    async fn start_control_with_reload(
        &self,
        config: &ControlProcessConfig,
        reload: RecordingReload,
    ) -> ployzd::control_runtime::RunningControlRuntime {
        ployzd::control_runtime::start_control_runtime_with_client_and_reload(
            self.client.clone(),
            config,
            reload,
        )
        .await
        .expect("control runtime starts")
    }
}

/// Records reload outcomes; signals the fixture server, fails on purpose,
/// or blocks behind a release gate to prove handler/reload ordering.
#[derive(Clone)]
struct RecordingReload {
    behavior: ReloadBehavior,
    outcomes: Arc<Mutex<Vec<NatsReloadOutcome>>>,
}

#[derive(Clone)]
enum ReloadBehavior {
    Signal(u32),
    GatedSignal { pid: u32, released: Arc<AtomicBool> },
    Fail,
}

impl RecordingReload {
    fn signal(pid: u32) -> Self {
        Self {
            behavior: ReloadBehavior::Signal(pid),
            outcomes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn gated_signal(pid: u32) -> Self {
        Self {
            behavior: ReloadBehavior::GatedSignal {
                pid,
                released: Arc::new(AtomicBool::new(false)),
            },
            outcomes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn failing() -> Self {
        Self {
            behavior: ReloadBehavior::Fail,
            outcomes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn release(&self) {
        if let ReloadBehavior::GatedSignal { released, .. } = &self.behavior {
            released.store(true, Ordering::SeqCst);
        }
    }

    fn outcomes(&self) -> Vec<NatsReloadOutcome> {
        self.outcomes
            .lock()
            .expect("reload outcome lock is not poisoned")
            .clone()
    }
}

impl NatsReloadRunner for RecordingReload {
    fn reload(&self) -> NatsReloadOutcome {
        let outcome = match &self.behavior {
            ReloadBehavior::Signal(pid) => SignalNatsReloadRunner::new(*pid).reload(),
            ReloadBehavior::GatedSignal { pid, released } => {
                while !released.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                SignalNatsReloadRunner::new(*pid).reload()
            }
            ReloadBehavior::Fail => NatsReloadOutcome::Failed(NatsReloadEvidence {
                command: "test-reload".to_owned(),
                output: "reload refused by test".to_owned(),
            }),
        };
        self.outcomes
            .lock()
            .expect("reload outcome lock is not poisoned")
            .push(outcome.clone());
        outcome
    }
}

async fn machine_add(
    api: &OperationApiClient,
    operation: &str,
    idempotency: &str,
    node: &str,
) -> MachineAddAccepted {
    api.machine_add(&MachineAddRequest {
        operation_id: operation_id(operation),
        idempotency_key: idempotency_key(idempotency),
        node_id: node_id(node),
        name: ployz_sdk_types::MachineName::try_new(node).expect("valid machine name"),
        gateway: MachineAddGateway::Skip,
    })
    .await
    .expect("machine add accepts")
}

/// Keeper-style bounded redeem retry: not-ready is retried, anything else
/// is a test failure.
async fn redeem_when_ready(
    api: &OperationApiClient,
    join_token: &MachineJoinToken,
) -> MachineJoinRedeemed {
    for _ in 0..200 {
        match api
            .machine_join_redeem(&MachineJoinRedeemRequest {
                join_token: join_token.clone(),
            })
            .await
        {
            Ok(redeemed) => return redeemed,
            Err(OperationApiClientError::Domain {
                error: MachineJoinRedeemError::MaterialNotReady { .. },
                ..
            }) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => panic!("redeem failed: {error:?}"),
        }
    }
    panic!("machine-add material did not become ready");
}

fn public_key_of(seed: &str) -> NatsUserPublicKey {
    let pair = nkeys::KeyPair::from_seed(seed).expect("minted seed parses");
    NatsUserPublicKey::try_new(pair.public_key()).expect("derived public key is valid")
}

fn rendered_principal_key(
    users: &[ployz_core::nats_config::NatsAuthorizedUser],
    node: &str,
) -> Option<NatsUserPublicKey> {
    users
        .iter()
        .find(|user| {
            user.principal
                == NatsPrincipal::Node {
                    node_id: node_id(node),
                }
        })
        .map(|user| user.nkey_public.clone())
}

async fn wait_for_terminal_machine_add_status(
    api: &OperationApiClient,
    operation_id: OperationId,
) -> OperationStatus {
    for _ in 0..200 {
        let status = api
            .ops_status(&OpsStatusRequest {
                operation_id: operation_id.clone(),
            })
            .await
            .expect("status is readable")
            .status;
        let OperationStatus::MachineAdd { state, .. } = &status else {
            panic!("expected machine add status");
        };
        if state.is_terminal() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    panic!("machine add did not reach terminal status");
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn machine_join_template(nats: &TestNats) -> MachineJoinTemplate {
    let ca_pem = std::fs::read_to_string(nats.nats.ca_path()).expect("fixture CA is readable");
    MachineJoinTemplate {
        join_bundle: ployz_core::install::MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(
                    nats.nats.client_url().as_str(),
                )
                .expect("valid runtime nats url"),
                trusted_nats: MachineJoinTrustedNats {
                    server_name: NatsServerName::try_new("server_1")
                        .expect("valid nats server name"),
                    ca_pem: NatsCaCertificatePem::try_new(ca_pem).expect("valid ca pem"),
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

fn active_machine(value: &str) -> ActiveMachineState {
    ActiveMachineState {
        node_id: node_id(value),
        name: ployz_sdk_types::MachineName::try_new(value).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
    }
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

fn deploy_target_with_route(
    service_id: &str,
    gateway_port: u16,
    endpoint_port: u16,
) -> DeployRequest {
    DeployRequest {
        route: Some(DeployRoute {
            target: RouteTarget::try_new(
                route_hostname("api.example.com"),
                route_port(gateway_port),
            ),
            endpoint_port: route_port(endpoint_port),
        }),
        ..deploy_target(service_id)
    }
}

fn route_hostname(value: &str) -> RouteHostname {
    RouteHostname::try_new(value).expect("valid route hostname")
}

fn route_port(value: u16) -> RoutePort {
    RoutePort::try_new(value).expect("valid route port")
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

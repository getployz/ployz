//! Gated Docker-in-Docker harness tests.
//!
//! Run with: `PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test dind_cluster
//! -- --test-threads=1`. Requires the machine image from
//! `scripts/build-dind-machine-image.sh` and Docker with `--privileged`
//! support. `PLOYZ_DIND_KEEP=1` keeps the cluster running for debugging;
//! `scripts/dind-clean.sh` sweeps leftovers.
//!
//! The cluster-formation scenarios drive the same product path the proven
//! `scripts/local-dataplane-proof.sh` recipe drives: keeper first-node
//! install through `ployzctl init --run-keeper-install` (join template
//! written with a placeholder CA first, re-rendered with the keeper-minted
//! cluster CA afterwards — the documented join-template/CA ordering), then
//! `ployzctl init activate-first-node`, then machine-add + the
//! `scripts/ployz.sh` join flow on an edge machine.

use ployz_core::deploy::{DeployRequest, DeployRoute, ImageReference, ReplicaCount};
use ployz_core::ids::{NodeId, OperationId, RevisionId, ServiceId};
use ployz_core::machine::{MachineAddOperationState, MachineCredentialProvisioningStep};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, DeployRunningStage, EventSequence,
    OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationIdempotencyKey, OperationStatus, RouteHostname,
    RoutePort, RouteTarget,
};
use ployz_core::permissions::inbox_subscribe_scope;
use ployz_core::security::NatsPrincipal;
use ployz_core::state::KV_CORE_BUCKET;
use ployz_core::subjects::{NodeServiceEndpoint, node_service};
use ployz_e2e::bollard::Docker;
use ployz_e2e::bollard::query_parameters::{
    ListContainersOptionsBuilder, ListNetworksOptionsBuilder,
};
use ployz_e2e::dind::{
    self, ARTIFACTS_MOUNT_PATH, DindCluster, DindClusterSpec, DindMachine, DindMachineRole,
    ExecOutcome, MACHINE_NATS_PORT, MachineSpec, exec_in_container, read_file_from_container,
    shell_quote, write_file_in_container,
};
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust, authenticated_connect_options,
    connect_authenticated, connect_with_timeout,
};
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    DeploySubmitRequest, MachineAddAccepted, MachineAddGateway, MachineAddRequest,
    MachineInspectRequest, MachineJoinRedeemError, MachineJoinRedeemRequest, MachineListRequest,
    MachineName, MachineSnapshot, OpsStatusRequest,
};
use ployz_test_support::nats::SecuredTestNats;
use ployzctl::commands::machine::MachineAddOutput;
use ployzd::docker::labels::{
    CONTAINER_TYPE_LABEL, MANAGED_LABEL, OPERATION_ID_LABEL, REVISION_LABEL, SERVICE_ID_LABEL,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Where keeper install leaves the cluster CA and the seeds on the core.
const NATS_MATERIAL_DIR: &str = "/var/lib/ployz/nats";
/// The ployzd-control-owned authority file (recovery evidence).
const AUTHORIZED_USERS_FILE: &str = "/etc/nats/authorized-users.conf";
/// Where the keeper join commit leaves the redeemed per-machine seed
/// (keeper state dir `/var/lib/ployz` + `join-material.d`).
const EDGE_NATS_CREDS_FILE: &str = "/var/lib/ployz/join-material.d/nats.creds";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The workload image `scripts/build-dind-machine-image.sh` bakes into the
/// machine image; the inner Docker daemons load it at boot.
const WORKLOAD_IMAGE: &str = "nginx:1.27-alpine";
/// Route hostname the smoke deploy registers on both gateways.
const ROUTE_HOSTNAME: &str = "smoke.local";
/// Port nginx listens on inside its workload container.
const WORKLOAD_ENDPOINT_PORT: u16 = 80;
/// Per-request budget for HTTP probes against a published gateway port.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Budget for the routed two-machine deploy to reach a terminal state
/// (includes the image pull check and WireGuard/eBPF preparation).
const DEPLOY_TERMINAL_BUDGET: Duration = Duration::from_secs(300);
/// Budget for one server-side permission violation to arrive on the client
/// event channel.
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Smoke test: one machine boots to systemd + inner-docker readiness with the
/// artifact mount in place, and teardown leaves nothing labeled behind.
#[tokio::test]
async fn boots_machine_image() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let spec = DindClusterSpec {
        artifact_dir: dind::artifact_dir(),
        machines: vec![MachineSpec {
            role: DindMachineRole::Core,
            image: dind::machine_image(),
        }],
    };
    let cluster = DindCluster::provision(&docker, spec)
        .await
        .expect("provision one-machine DinD cluster");

    // Provisioning already waited for readiness; assert it holds from the
    // outside through the same exec surface scenarios will use.
    let system_state = exec_in_container(
        &docker,
        &cluster.core().container_id,
        &["systemctl", "is-system-running"],
    )
    .await;
    let system_ready = matches!(
        &system_state,
        Ok(outcome) if matches!(outcome.stdout.trim(), "running" | "degraded")
    );
    if !system_ready {
        fail_with_evidence(
            &cluster,
            &format!("core systemd not ready: {system_state:?}"),
        )
        .await;
    }

    let inner_docker =
        exec_in_container(&docker, &cluster.core().container_id, &["docker", "info"]).await;
    let inner_docker_ready = matches!(&inner_docker, Ok(outcome) if outcome.success());
    if !inner_docker_ready {
        fail_with_evidence(
            &cluster,
            &format!("inner docker not ready: {inner_docker:?}"),
        )
        .await;
    }

    let artifacts = exec_in_container(
        &docker,
        &cluster.core().container_id,
        &["test", "-x", "/opt/ployz/artifacts/ployzd"],
    )
    .await;
    let artifacts_mounted = matches!(&artifacts, Ok(outcome) if outcome.success());
    if !artifacts_mounted {
        fail_with_evidence(
            &cluster,
            &format!("artifact mount missing executable ployzd: {artifacts:?}"),
        )
        .await;
    }

    if dind::keep_requested() {
        eprintln!(
            "PLOYZ_DIND_KEEP=1: keeping run {} (network {}, core container {})",
            cluster.run_id(),
            cluster.network_name(),
            cluster.core().container_id,
        );
        return;
    }

    let run_label = format!("{}={}", dind::RUN_LABEL, cluster.run_id());
    cluster.teardown().await.expect("teardown DinD cluster");

    let filters = HashMap::from([("label".to_owned(), vec![run_label])]);
    let leftover_containers = docker
        .list_containers(Some(
            ListContainersOptionsBuilder::new()
                .all(true)
                .filters(&filters)
                .build(),
        ))
        .await
        .expect("list containers after teardown");
    assert!(
        leftover_containers.is_empty(),
        "teardown left labeled containers behind: {leftover_containers:?}"
    );
    let leftover_networks = docker
        .list_networks(Some(
            ListNetworksOptionsBuilder::new().filters(&filters).build(),
        ))
        .await
        .expect("list networks after teardown");
    assert!(
        leftover_networks.is_empty(),
        "teardown left labeled networks behind: {leftover_networks:?}"
    );
}

/// Captures evidence for the whole cluster, then panics with the message and
/// the evidence location.
async fn fail_with_evidence(cluster: &DindCluster, message: &str) -> ! {
    match cluster.capture_evidence().await {
        Ok(dir) => panic!("{message}; evidence: {}", dir.display()),
        Err(error) => panic!("{message}; evidence capture also failed: {error}"),
    }
}

/// Scenario 1 — init + activate-first-node forms a TLS-authenticated core
/// through product commands only, mints the first node's credential as
/// operation work, and hands the awaiting node/gateway processes their seed
/// without a unit restart.
#[tokio::test]
async fn scenario_init_and_activate_first_node() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = install_core_cluster(&docker, 0).await;
    let cluster = &core.cluster;

    // Before activate: node and gateway units run in the visible
    // awaiting-credentials state (node.seed does not exist yet) — B3.
    let node_unit = "ployzd-node-core_1";
    let gateway_unit = "ployzd-gateway";
    for unit in [node_unit, gateway_unit] {
        assert_unit_active(&core, cluster.core(), unit).await;
    }
    let node_seed_before = exec_on(
        &core,
        cluster.core(),
        &["test", "-f", "/var/lib/ployz/nats/node.seed"],
    )
    .await;
    if node_seed_before.success() {
        fail_with_evidence(cluster, "node.seed must not exist before activate").await;
    }
    assert_journal_contains(&core, &[node_unit, gateway_unit], "awaiting-credentials").await;
    let node_pid_before = unit_main_pid(&core, cluster.core(), node_unit).await;
    let gateway_pid_before = unit_main_pid(&core, cluster.core(), gateway_unit).await;

    // Activate through the product CLI inside the machine, authenticated
    // with the keeper-minted operator credential.
    let activate = exec_sh(
        &core,
        cluster.core(),
        &format!(
            "PLOYZ_NATS_CA_FILE={NATS_MATERIAL_DIR}/ca.pem \
             PLOYZ_NATS_NKEY_SEED_FILE={NATS_MATERIAL_DIR}/operator.seed \
             {ARTIFACTS_MOUNT_PATH}/ployzctl --nats tls://127.0.0.1:{MACHINE_NATS_PORT} \
             init activate-first-node --node core_1 --gateway"
        ),
    )
    .await;
    if !activate.success() {
        fail_with_evidence(
            cluster,
            &format!(
                "activate-first-node failed (exit {}): {}\n{}",
                activate.exit_code, activate.stdout, activate.stderr
            ),
        )
        .await;
    }
    let Some(activation_id) = parse_operation_line(&activate.stdout) else {
        fail_with_evidence(
            cluster,
            &format!("no operation id in activate output: {}", activate.stdout),
        )
        .await;
    };

    // The activation operation is a completed machine-add with the full
    // mint event sequence recorded as operation events.
    let status = operation_status(&core, &activation_id).await;
    let OperationStatus::MachineAdd { state, .. } = status else {
        fail_with_evidence(
            cluster,
            &format!("activation is not a machine add: {status:?}"),
        )
        .await;
    };
    if state != MachineAddOperationState::Completed {
        fail_with_evidence(cluster, &format!("activation not completed: {state:?}")).await;
    }
    let events = terminal_operation_events(&core, &activation_id).await;
    assert_machine_add_event_sequence(cluster, &events, &node_id("core_1")).await;

    // Data plane truth: all four units active.
    for unit in ["nats-server", "ployzd-control", node_unit, gateway_unit] {
        assert_unit_active(&core, cluster.core(), unit).await;
    }

    // node.seed exists now, written by ployzd control (B3 sequencing).
    let node_seed = read_file_from_container(
        &docker,
        &cluster.core().container_id,
        "/var/lib/ployz/nats/node.seed",
    )
    .await
    .expect("node.seed exists after activate");
    assert!(
        node_seed.trim().starts_with("SU"),
        "node.seed is an NKey user seed"
    );

    // The awaiting node/gateway picked the seed up in-process: they now
    // publish observations (the machine snapshot fills in) while their unit
    // MainPIDs never changed — no restart was required or issued.
    wait_for_machine_observations(&core, &node_id("core_1")).await;
    let node_pid_after = unit_main_pid(&core, cluster.core(), node_unit).await;
    let gateway_pid_after = unit_main_pid(&core, cluster.core(), gateway_unit).await;
    assert_eq!(
        node_pid_before, node_pid_after,
        "node unit must not restart across activate"
    );
    assert_eq!(
        gateway_pid_before, gateway_pid_after,
        "gateway unit must not restart across activate"
    );

    // The core node's minted public key landed in the authority file next
    // to the install-time principals.
    let authorized =
        read_file_from_container(&docker, &cluster.core().container_id, AUTHORIZED_USERS_FILE)
            .await
            .expect("authorized-users.conf is readable");
    for principal in ["controller", "user", "join", "node_core_1"] {
        assert!(
            authorized.contains(&format!("# ployz-principal: {principal}")),
            "authorized-users.conf must contain {principal}: {authorized}"
        );
    }

    // Bootstrap KV buckets and streams exist on the secured server.
    assert_bootstrap_resources_exist(&core).await;

    finish(core).await;
}

/// Scenario 2 — machine add returns its operation id before the mint's
/// reload lands, and the printed join bundle material drives the real
/// `scripts/ployz.sh` join flow on an edge machine over direct TLS NATS.
#[tokio::test]
async fn scenario_machine_add_via_join_bundle() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = install_core_cluster(&docker, 1).await;
    let cluster = &core.cluster;
    activate_first_node(&core).await;

    let add_operation = operation_id("op_add_edge_2");
    let accepted = core
        .api
        .machine_add(&MachineAddRequest {
            operation_id: add_operation.clone(),
            idempotency_key: idempotency_key("idem_add_edge_2"),
            node_id: node_id("edge_2"),
            name: machine_name("edge-2"),
            gateway: MachineAddGateway::Install,
        })
        .await
        .expect("machine add submits");
    assert_eq!(accepted.accepted.operation_id, add_operation);

    // The submit response carried the operation id before the mint's
    // reload landed: the immediately-replayed event page has no `reloaded`
    // event yet (minting is bounded operation work after acceptance).
    let early_events = operation_events(&core, &add_operation).await;
    assert!(
        !early_events.iter().any(|event| matches!(
            event,
            OperationEvent::MachineAddCredentialProvisioned {
                step: MachineCredentialProvisioningStep::Reloaded,
                ..
            }
        )),
        "reload must land after acceptance, not inside the submit handler: {early_events:?}"
    );

    // The install line is the product's own render of the join material;
    // the edge joins with exactly what it prints.
    let install = parse_install_line(&core, accepted.clone());
    assert_eq!(
        install.nats_url,
        format!("tls://{}:{MACHINE_NATS_PORT}", core.core_ip),
        "join bundle must point at the core's direct TLS NATS endpoint"
    );
    let printed_ca = decode_base64(&install.nats_ca_b64);
    assert_eq!(
        printed_ca.trim(),
        core.material.ca_pem.trim(),
        "install line must carry the cluster CA"
    );

    let [edge] = cluster.edges() else {
        panic!("scenario requires exactly one edge machine");
    };
    run_edge_join(&core, edge, &install).await;

    // Join operation completed with the mint sequence ordered around
    // acceptance, and the machine is active.
    let status = operation_status(&core, &add_operation).await;
    let OperationStatus::MachineAdd { state, .. } = status else {
        fail_with_evidence(
            cluster,
            &format!("machine add is not a machine add: {status:?}"),
        )
        .await;
    };
    if state != MachineAddOperationState::Completed {
        fail_with_evidence(cluster, &format!("machine add not completed: {state:?}")).await;
    }
    let events = terminal_operation_events(&core, &add_operation).await;
    assert_machine_add_event_sequence(cluster, &events, &node_id("edge_2")).await;

    // nats_connection readiness evidence: the edge's node process connects
    // with its minted credential and publishes observations.
    wait_for_machine_observations(&core, &node_id("edge_2")).await;

    // The edge holds its own minted seed — not the controller's.
    let edge_creds = read_file_from_container(&docker, &edge.container_id, EDGE_NATS_CREDS_FILE)
        .await
        .expect("edge nats.creds exists after join");
    assert!(
        edge_creds.trim().starts_with("SU"),
        "edge nats.creds is an NKey user seed"
    );
    assert_ne!(
        edge_creds.trim(),
        core.material.controller_seed.trim(),
        "edge credential must differ from the controller seed"
    );

    // Never-shrink: the edge key is appended alongside every prior user.
    let authorized =
        read_file_from_container(&docker, &cluster.core().container_id, AUTHORIZED_USERS_FILE)
            .await
            .expect("authorized-users.conf is readable");
    for principal in ["controller", "user", "join", "node_core_1", "node_edge_2"] {
        assert!(
            authorized.contains(&format!("# ployz-principal: {principal}")),
            "authorized-users.conf must keep {principal}: {authorized}"
        );
    }

    // No separate gateway credential exists: the edge gateway role env
    // points its seed file at the machine's Node creds.
    let gateway_env =
        read_file_from_container(&docker, &edge.container_id, "/etc/ployz/ployzd-gateway.env")
            .await
            .expect("edge gateway env file exists");
    assert!(
        gateway_env.contains(&format!("PLOYZ_NATS_NKEY_SEED_FILE={EDGE_NATS_CREDS_FILE}")),
        "edge gateway must authenticate with the Node creds: {gateway_env}"
    );

    // The join token is single-use: re-redeeming it is refused and the
    // failure is typed, not a fresh secret.
    let join_client = connect_core_client(&core, NatsPrincipal::Join, &core.material.join_seed)
        .await
        .expect("join principal connects");
    let redeem_again = OperationApiClient::new(join_client)
        .machine_join_redeem(&MachineJoinRedeemRequest {
            join_token: accepted.join_token.clone(),
        })
        .await;
    match redeem_again {
        Err(OperationApiClientError::Domain {
            error: MachineJoinRedeemError::OperationNotPending { operation_id, .. },
            ..
        }) => assert_eq!(operation_id, add_operation),
        other => {
            fail_with_evidence(
                cluster,
                &format!("token re-redeem must be refused as not-pending: {other:?}"),
            )
            .await;
        }
    }

    finish(core).await;
}

/// Scenarios 3–5 on one formed two-machine cluster (one cluster fits the
/// host at a time, scenario 4 needs scenario 3's serving deploy, and
/// scenario 5 needs the same healthy cluster as its blast-radius control):
///
/// - **Scenario 3 — cross-machine deploy:** the baked nginx image deploys
///   with a replica on each machine and a route, driven through the
///   host-side API client; the operation events, the inner Docker reality
///   on both machines, and HTTP through both published gateway ports all
///   agree.
/// - **Scenario 4 — daemon-restart invisibility:** restarting
///   `ployzd-control` (core) and the edge node unit neither interrupts
///   gateway HTTP nor replaces workload containers, and the operations API
///   answers afterwards with unmutated machine state.
/// - **Scenario 5 — auth rejection:** unauthorized seeds, plaintext
///   clients, and over-reaching Node/Join principals are refused by the
///   real cluster — which keeps serving afterwards.
#[tokio::test]
async fn scenario_deploy_restart_invisibility_and_auth_rejection() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = install_core_cluster(&docker, 1).await;
    let cluster = &core.cluster;
    activate_first_node(&core).await;
    let [edge] = cluster.edges() else {
        panic!("scenario requires exactly one edge machine");
    };
    add_and_join_edge(&core, edge).await;
    wait_for_machine_observations(&core, &node_id("core_1")).await;
    wait_for_machine_observations(&core, &node_id("edge_2")).await;

    scenario_cross_machine_deploy(&core, edge).await;
    scenario_daemon_restart_invisibility(&core, edge).await;
    scenario_auth_rejection(&core, edge).await;

    finish(core).await;
}

// ---------------------------------------------------------------------------
// Scenario 3 — cross-machine deploy
// ---------------------------------------------------------------------------

/// Deploys the baked workload image with one replica per machine and a
/// route, through the host-side operator API client, and asserts the
/// committed deploy event vocabulary, Docker reality, and HTTP service.
async fn scenario_cross_machine_deploy(core: &CoreContext, edge: &DindMachine) {
    let cluster = &core.cluster;
    let deploy_operation = operation_id("op_dind_deploy");
    let accepted = match core
        .api
        .deploy_submit(&DeploySubmitRequest {
            operation_id: deploy_operation.clone(),
            idempotency_key: idempotency_key("idem_dind_deploy"),
            target: smoke_deploy_target(),
        })
        .await
    {
        Ok(accepted) => accepted,
        Err(error) => {
            fail_with_evidence(cluster, &format!("deploy submit failed: {error}")).await;
        }
    };
    assert_eq!(accepted.operation_id, deploy_operation);

    let status = wait_for_terminal_deploy_status(core, &deploy_operation).await;
    let completed = matches!(
        &status,
        OperationStatus::Deploy {
            state: DeployOperationState::Completed {
                outcome: DeployCompletionOutcome::Completed,
            },
            ..
        }
    );
    if !completed {
        fail_with_evidence(cluster, &format!("deploy did not complete: {status:?}")).await;
    }
    let events = terminal_operation_events(core, &deploy_operation).await;
    assert_deploy_event_sequence(cluster, &events, &deploy_operation).await;

    // Docker is execution reality: each machine runs exactly one managed
    // workload container carrying the product's label schema.
    for machine in [cluster.core(), edge] {
        let containers = managed_workload_containers(core, machine).await;
        let [container] = containers.as_slice() else {
            fail_with_evidence(
                cluster,
                &format!(
                    "expected one managed container on {}, got {containers:?}",
                    machine.name
                ),
            )
            .await;
        };
        for expected in [
            format!("{SERVICE_ID_LABEL}=svc_smoke"),
            format!("{REVISION_LABEL}=rev_local"),
            format!("{OPERATION_ID_LABEL}={}", deploy_operation.as_str()),
            format!("{CONTAINER_TYPE_LABEL}=service"),
        ] {
            if !container.labels.contains(expected.as_str()) {
                fail_with_evidence(
                    cluster,
                    &format!(
                        "managed container on {} is missing label {expected}: {container:?}",
                        machine.name
                    ),
                )
                .await;
            }
        }
    }

    // The route serves through BOTH gateways via the published ports.
    for machine in [cluster.core(), edge] {
        assert_gateway_serves(core, machine).await;
    }
}

/// The smoke service: the baked workload image, one replica per machine,
/// routed on every gateway's listen port.
fn smoke_deploy_target() -> DeployRequest {
    DeployRequest {
        service_id: service_id("svc_smoke"),
        target_revision: revision_id("rev_local"),
        image: ImageReference::try_new(WORKLOAD_IMAGE).expect("valid workload image reference"),
        replicas: ReplicaCount::try_new(2).expect("valid replica count"),
        route: Some(DeployRoute {
            target: RouteTarget::try_new(
                route_hostname(ROUTE_HOSTNAME),
                route_port(dind::MACHINE_GATEWAY_PORT),
            ),
            endpoint_port: route_port(WORKLOAD_ENDPOINT_PORT),
        }),
    }
}

/// Polls the operation status through the host-side API until the deploy is
/// terminal.
async fn wait_for_terminal_deploy_status(
    core: &CoreContext,
    operation_id: &OperationId,
) -> OperationStatus {
    let deadline = Instant::now() + DEPLOY_TERMINAL_BUDGET;
    loop {
        let status = operation_status(core, operation_id).await;
        let OperationStatus::Deploy { state, .. } = &status else {
            fail_with_evidence(
                &core.cluster,
                &format!("operation {operation_id:?} is not a deploy: {status:?}"),
            )
            .await;
        };
        if state.is_terminal() {
            return status;
        }
        if Instant::now() >= deadline {
            fail_with_evidence(
                &core.cluster,
                &format!("deploy {operation_id:?} not terminal in budget: {status:?}"),
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The committed deploy event vocabulary (the `operations.rs` sequence) for
/// a two-machine routed deploy: submitted → planning → plan → WireGuard/eBPF
/// preparation over both nodes → container starts on both nodes → health →
/// commit → completed, in order.
async fn assert_deploy_event_sequence(
    cluster: &DindCluster,
    events: &[OperationEvent],
    deploy_operation: &OperationId,
) {
    let submitted = position(events, |event| {
        matches!(
            event,
            OperationEvent::DeploySubmitted { operation_id, .. }
                if operation_id == deploy_operation
        )
    });
    let planning_started = position(events, |event| {
        matches!(event, OperationEvent::DeployPlanningStarted { .. })
    });
    let plan_created = position(events, |event| {
        matches!(event, OperationEvent::DeployPlanCreated { .. })
    });
    let preparing = position(events, |event| {
        matches!(
            event,
            OperationEvent::DeployRunning {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
                ..
            }
        )
    });
    let prepared = position(events, |event| {
        matches!(
            event,
            OperationEvent::DeployWireGuardEbpfPrepared { report, .. }
                if report
                    .nodes
                    .iter()
                    .map(|node| node.node_id().clone())
                    .collect::<Vec<_>>()
                    == vec![node_id("core_1"), node_id("edge_2")]
        )
    });
    let starting = position(events, |event| {
        matches!(
            event,
            OperationEvent::DeployRunning {
                stage: DeployRunningStage::StartingContainers,
                ..
            }
        )
    });
    let waiting = position(events, |event| {
        matches!(
            event,
            OperationEvent::DeployRunning {
                stage: DeployRunningStage::WaitingForHealth,
                ..
            }
        )
    });
    let health_started = position(events, |event| {
        matches!(event, OperationEvent::DeployHealthCheckStarted { .. })
    });
    let committing = position(events, |event| {
        matches!(
            event,
            OperationEvent::DeployRunning {
                stage: DeployRunningStage::ActiveServiceCommit,
                ..
            }
        )
    });
    let completed = position(events, |event| {
        matches!(
            event,
            OperationEvent::DeployCompleted {
                outcome: DeployCompletionOutcome::Completed,
                ..
            }
        )
    });

    let ordered = [
        submitted,
        planning_started,
        plan_created,
        preparing,
        prepared,
        starting,
        waiting,
        health_started,
        committing,
        completed,
    ];
    let mut resolved = Vec::with_capacity(ordered.len());
    for entry in ordered {
        match entry {
            Some(index) => resolved.push(index),
            None => {
                fail_with_evidence(
                    cluster,
                    &format!("missing deploy event for {deploy_operation:?}: {events:?}"),
                )
                .await;
            }
        }
    }
    if !resolved.is_sorted() {
        fail_with_evidence(
            cluster,
            &format!("deploy events out of order for {deploy_operation:?}: {events:?}"),
        )
        .await;
    }

    // One container start per machine, both inside the StartingContainers →
    // WaitingForHealth window (their relative order is placement-dependent).
    let [.., starting_index, waiting_index, _, _, _] = resolved.as_slice() else {
        unreachable!("resolved has ten entries");
    };
    for expected_node in [node_id("core_1"), node_id("edge_2")] {
        let started = position(events, |event| {
            matches!(
                event,
                OperationEvent::DeployContainerStarted { node_id, .. }
                    if *node_id == expected_node
            )
        });
        let Some(started) = started else {
            fail_with_evidence(
                cluster,
                &format!("no container start on {expected_node:?}: {events:?}"),
            )
            .await;
        };
        if started < *starting_index || started > *waiting_index {
            fail_with_evidence(
                cluster,
                &format!(
                    "container start on {expected_node:?} outside the starting window: {events:?}"
                ),
            )
            .await;
        }
    }
}

/// One managed workload container as the inner Docker daemon reports it.
#[derive(Debug)]
struct ManagedWorkloadContainer {
    id: String,
    labels: String,
}

/// Lists the running managed workload containers inside one machine's inner
/// Docker daemon (the product's `plz.managed` label schema).
async fn managed_workload_containers(
    core: &CoreContext,
    machine: &DindMachine,
) -> Vec<ManagedWorkloadContainer> {
    let outcome = exec_on(
        core,
        machine,
        &[
            "docker",
            "ps",
            "--no-trunc",
            "--filter",
            &format!("label={MANAGED_LABEL}=true"),
            "--format",
            "{{.ID}}\t{{.Labels}}",
        ],
    )
    .await;
    if !outcome.success() {
        fail_with_evidence(
            &core.cluster,
            &format!("inner docker ps on {} failed: {outcome:?}", machine.name),
        )
        .await;
    }
    outcome
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| match line.split_once('\t') {
            Some((id, labels)) => ManagedWorkloadContainer {
                id: id.to_owned(),
                labels: labels.to_owned(),
            },
            None => ManagedWorkloadContainer {
                id: line.to_owned(),
                labels: String::new(),
            },
        })
        .collect()
}

/// One plain HTTP GET against a published gateway port with the route's
/// host header; the error carries enough context for evidence.
async fn gateway_http_get(addr: SocketAddr, host: &str) -> Result<String, String> {
    let request = async {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|error| format!("connect {addr}: {error}"))?;
        stream
            .write_all(
                format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await
            .map_err(|error| format!("write {addr}: {error}"))?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .map_err(|error| format!("read {addr}: {error}"))?;
        Ok(response)
    };
    match tokio::time::timeout(HTTP_TIMEOUT, request).await {
        Ok(result) => result,
        Err(_elapsed) => Err(format!("http get {addr} timed out")),
    }
}

/// The route host header: hostname plus the gateway's in-machine listen
/// port (the route target port), not the published host port.
fn route_host_header() -> String {
    format!("{ROUTE_HOSTNAME}:{}", dind::MACHINE_GATEWAY_PORT)
}

/// Asserts the smoke route answers through one machine's published gateway
/// port with the workload's response body.
async fn assert_gateway_serves(core: &CoreContext, machine: &DindMachine) {
    match gateway_http_get(machine.published.gateway, &route_host_header()).await {
        Ok(response) if response.contains("Welcome to nginx") => {}
        Ok(response) => {
            fail_with_evidence(
                &core.cluster,
                &format!(
                    "gateway on {} answered without the workload body: {response}",
                    machine.name
                ),
            )
            .await;
        }
        Err(error) => {
            fail_with_evidence(
                &core.cluster,
                &format!("gateway on {} did not answer: {error}", machine.name),
            )
            .await;
        }
    }
}

// ---------------------------------------------------------------------------
// Scenario 4 — daemon-restart invisibility
// ---------------------------------------------------------------------------

/// Restarts the control daemon on the core and the node daemon on the edge
/// while the deploy is serving: gateway HTTP must keep answering throughout,
/// workload containers must be adopted (same IDs), and the operations API
/// must answer afterwards with unmutated machine state.
async fn scenario_daemon_restart_invisibility(core: &CoreContext, edge: &DindMachine) {
    let cluster = &core.cluster;
    let core_machine = cluster.core();

    let core_snapshot_before = wait_for_settled_snapshot(core, &node_id("core_1")).await;
    let edge_snapshot_before = wait_for_settled_snapshot(core, &node_id("edge_2")).await;
    let core_containers_before = workload_container_ids(core, core_machine).await;
    let edge_containers_before = workload_container_ids(core, edge).await;

    // Restart the daemons concurrently with the HTTP polling below.
    let docker = core.docker.clone();
    let core_container = core_machine.container_id.clone();
    let edge_container = edge.container_id.clone();
    let restart = tokio::spawn(async move {
        let control = exec_in_container(
            &docker,
            &core_container,
            &["systemctl", "restart", "ployzd-control"],
        )
        .await;
        let node = exec_in_container(
            &docker,
            &edge_container,
            &["systemctl", "restart", "ployzd-node-edge_2"],
        )
        .await;
        (control, node)
    });

    // Gateway HTTP keeps answering during the whole restart window.
    let mut polls: u32 = 0;
    loop {
        let finished = restart.is_finished();
        for machine in [core_machine, edge] {
            assert_gateway_serves(core, machine).await;
        }
        polls += 1;
        if finished {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let restarted = match restart.await {
        Ok(outcomes) => outcomes,
        Err(error) => fail_with_evidence(cluster, &format!("restart task panicked: {error}")).await,
    };
    match restarted {
        (Ok(control), Ok(node)) if control.success() && node.success() => {}
        other => {
            fail_with_evidence(cluster, &format!("daemon restarts failed: {other:?}")).await;
        }
    }
    assert!(
        polls >= 2,
        "gateways must be polled during the restart window, not only after it"
    );
    assert_unit_active(core, core_machine, "ployzd-control").await;
    assert_unit_active(core, edge, "ployzd-node-edge_2").await;

    // Adopt-not-recreate: the inner Docker container IDs are unchanged.
    let core_containers_after = workload_container_ids(core, core_machine).await;
    let edge_containers_after = workload_container_ids(core, edge).await;
    assert_eq!(
        core_containers_before, core_containers_after,
        "core workload containers must survive the control restart"
    );
    assert_eq!(
        edge_containers_before, edge_containers_after,
        "edge workload containers must survive the node restart"
    );

    // The operations API answers after reconnect with unmutated machine
    // state (gateway status is the restarted processes' own live
    // observation, so the comparison covers identity, public ip, and the
    // observed container set).
    wait_for_machine_snapshot(core, &node_id("core_1"), &core_snapshot_before).await;
    wait_for_machine_snapshot(core, &node_id("edge_2"), &edge_snapshot_before).await;
}

/// Sorted running managed-container IDs inside one machine.
async fn workload_container_ids(core: &CoreContext, machine: &DindMachine) -> Vec<String> {
    let mut ids = managed_workload_containers(core, machine)
        .await
        .into_iter()
        .map(|container| container.id)
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

/// Polls until the machine snapshot reflects the completed deploy (one
/// observed workload container plus the standing observations), so the
/// post-restart comparison starts from settled truth.
async fn wait_for_settled_snapshot(core: &CoreContext, machine: &NodeId) -> MachineSnapshot {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = String::from("<no inspect yet>");
    while Instant::now() < deadline {
        match core
            .api
            .machine_inspect(&MachineInspectRequest {
                node_id: machine.clone(),
            })
            .await
        {
            Ok(snapshot) => {
                if snapshot.public_ip.is_some()
                    && snapshot.gateway.is_some()
                    && snapshot.observed_container_count == 1
                {
                    return snapshot;
                }
                last = format!("{snapshot:?}");
            }
            Err(error) => last = format!("{error}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    fail_with_evidence(
        &core.cluster,
        &format!("machine {machine:?} never settled after the deploy: {last}"),
    )
    .await
}

/// Polls until the machine snapshot matches the pre-restart truth again:
/// same active state, same public ip, same observed container count.
async fn wait_for_machine_snapshot(core: &CoreContext, machine: &NodeId, before: &MachineSnapshot) {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last = String::from("<no inspect yet>");
    while Instant::now() < deadline {
        match core
            .api
            .machine_inspect(&MachineInspectRequest {
                node_id: machine.clone(),
            })
            .await
        {
            Ok(snapshot) => {
                let MachineSnapshot {
                    active,
                    public_ip,
                    gateway,
                    observed_container_count,
                } = &snapshot;
                if *active == before.active
                    && *public_ip == before.public_ip
                    && gateway.is_some()
                    && *observed_container_count == before.observed_container_count
                {
                    return;
                }
                last = format!("{snapshot:?}");
            }
            Err(error) => last = format!("{error}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    fail_with_evidence(
        &core.cluster,
        &format!(
            "machine {machine:?} state never matched the pre-restart truth \
             (expected {before:?}): {last}"
        ),
    )
    .await
}

// ---------------------------------------------------------------------------
// Scenario 5 — auth rejection
// ---------------------------------------------------------------------------

/// The four committed rejections against the real cluster, plus the proof
/// that none of them hurt the data plane.
async fn scenario_auth_rejection(core: &CoreContext, edge: &DindMachine) {
    let cluster = &core.cluster;

    // (a) A fresh random NKey seed with the correct cluster CA is refused.
    let unauthorized_seed =
        SecuredTestNats::fresh_unauthorized_seed().expect("fresh unauthorized seed");
    let rejected = connect_core_client(core, NatsPrincipal::User, unauthorized_seed.secret()).await;
    if rejected.is_ok() {
        fail_with_evidence(cluster, "a seed outside the authorized user set connected").await;
    }

    // (b) A plaintext client cannot complete a handshake with the TLS port.
    let plaintext_url = NatsClientUrl::try_new(format!("nats://{}", cluster.core().published.nats))
        .expect("valid plaintext url");
    let plaintext = connect_with_timeout(&plaintext_url, CONNECT_TIMEOUT).await;
    if plaintext.is_ok() {
        fail_with_evidence(cluster, "a plaintext client reached the TLS-only port").await;
    }

    // (c) The edge node's minted seed is fenced to its own scope: publishing
    // into the core node's service scope and writing core KV subjects both
    // draw server-side permission violations.
    let edge_seed =
        read_file_from_container(&core.docker, &edge.container_id, EDGE_NATS_CREDS_FILE)
            .await
            .expect("edge nats.creds is readable");
    let edge_node_config = host_client_config(
        cluster,
        &core.material,
        NatsPrincipal::Node {
            node_id: node_id("edge_2"),
        },
        edge_seed.trim(),
    );
    let (edge_client, mut edge_events) = connect_with_event_capture(core, &edge_node_config).await;
    for subject in [
        node_service(&node_id("core_1"), NodeServiceEndpoint::Inspect),
        format!("$KV.{KV_CORE_BUCKET}.machines.active.core_1"),
    ] {
        if let Err(error) = edge_client
            .publish(subject.clone(), "evidence".into())
            .await
        {
            fail_with_evidence(
                cluster,
                &format!("publish to {subject} was not accepted client-side: {error}"),
            )
            .await;
        }
        if let Err(error) = edge_client.flush().await {
            fail_with_evidence(cluster, &format!("edge client flush failed: {error}")).await;
        }
        match next_permission_violation(&mut edge_events).await {
            Some(violation) if violation.contains("Publish") => {}
            other => {
                fail_with_evidence(
                    cluster,
                    &format!("expected a publish violation for {subject}, got {other:?}"),
                )
                .await;
            }
        }
    }

    // (d) The cluster's Join seed cannot sniff inboxes: the shared legacy
    // inbox scope and the core node's own prefix are both refused.
    let join_config = host_client_config(
        cluster,
        &core.material,
        NatsPrincipal::Join,
        &core.material.join_seed,
    );
    let (join_client, mut join_events) = connect_with_event_capture(core, &join_config).await;
    let core_node_inbox = inbox_subscribe_scope(&NatsPrincipal::Node {
        node_id: node_id("core_1"),
    });
    for scope in ["_INBOX.>", core_node_inbox.as_str()] {
        let subscribed = join_client.subscribe(scope.to_owned()).await;
        if let Err(error) = subscribed {
            fail_with_evidence(
                cluster,
                &format!("subscribe {scope} was not accepted client-side: {error}"),
            )
            .await;
        }
        if let Err(error) = join_client.flush().await {
            fail_with_evidence(cluster, &format!("join client flush failed: {error}")).await;
        }
        match next_permission_violation(&mut join_events).await {
            Some(violation) if violation.contains("Subscription") => {}
            other => {
                fail_with_evidence(
                    cluster,
                    &format!("expected a subscription violation for {scope}, got {other:?}"),
                )
                .await;
            }
        }
    }

    // The cluster shrugged all of it off: both gateways still serve and the
    // control API still answers.
    for machine in [cluster.core(), edge] {
        assert_gateway_serves(core, machine).await;
    }
    if let Err(error) = core.api.machine_list(&MachineListRequest {}).await {
        fail_with_evidence(
            cluster,
            &format!("control API unhealthy after rejected clients: {error}"),
        )
        .await;
    }
}

/// Connects with the exact product option set plus an event capture channel
/// so the test can observe server-side permission violations.
async fn connect_with_event_capture(
    core: &CoreContext,
    config: &NatsConnectConfig,
) -> (
    async_nats::Client,
    tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) {
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let connected = authenticated_connect_options(config)
        .event_callback(move |event| {
            let events_tx = events_tx.clone();
            async move {
                events_tx.send(event).ok();
            }
        })
        .connect(config.url.as_str())
        .await;
    match connected {
        Ok(client) => (client, events_rx),
        Err(error) => {
            fail_with_evidence(
                &core.cluster,
                &format!("authenticated connect with event capture failed: {error}"),
            )
            .await
        }
    }
}

/// Waits for the next server-side permission violation on the event
/// channel; `None` when none arrives in budget.
async fn next_permission_violation(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) -> Option<String> {
    tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let event = events.recv().await?;
            if let async_nats::Event::ServerError(async_nats::ServerError::Other(message)) = event
                && message
                    .to_ascii_lowercase()
                    .contains("permissions violation")
            {
                return Some(message);
            }
        }
    })
    .await
    .ok()
    .flatten()
}

// ---------------------------------------------------------------------------
// Core-cluster formation (the proof-script recipe, host-driven)
// ---------------------------------------------------------------------------

/// One formed core: provisioned machines, keeper-installed secured NATS +
/// ployzd units on the core, the cluster material copied to the host, and an
/// authenticated host-side operator API client through the published port.
struct CoreContext {
    cluster: DindCluster,
    docker: Docker,
    core_ip: String,
    material: ClusterMaterial,
    api: OperationApiClient,
}

/// Cluster material copied out of the core container for host-side clients.
struct ClusterMaterial {
    /// Owns the on-host material files for the lifetime of the scenario.
    _dir: tempfile::TempDir,
    ca_file: PathBuf,
    ca_pem: String,
    operator_seed: String,
    controller_seed: String,
    join_seed: String,
}

/// Provisions the machines and forms the secured core exactly the way the
/// proof script does: placeholder-CA join template → keeper install via
/// `ployzctl init --run-keeper-install` → re-render the template with the
/// keeper-minted CA and restart the (disposable) control role → wait for
/// the control API over the published TLS port.
async fn install_core_cluster(docker: &Docker, edge_count: usize) -> CoreContext {
    let mut machines = vec![MachineSpec {
        role: DindMachineRole::Core,
        image: dind::machine_image(),
    }];
    for _ in 0..edge_count {
        machines.push(MachineSpec {
            role: DindMachineRole::Edge,
            image: dind::machine_image(),
        });
    }
    let cluster = DindCluster::provision(
        docker,
        DindClusterSpec {
            artifact_dir: dind::artifact_dir(),
            machines,
        },
    )
    .await
    .expect("provision DinD cluster");

    let core_ip = cluster.core().bridge_ip.to_string();
    let core_nats_url = format!("tls://{core_ip}:{MACHINE_NATS_PORT}");
    let core = cluster.core().clone();

    // The dataplane mounts bpffs on every machine (proof-script recipe).
    for machine in std::iter::once(&core).chain(cluster.edges()) {
        let mounted = exec_in_container(
            docker,
            &machine.container_id,
            &[
                "sh",
                "-c",
                "mountpoint -q /sys/fs/bpf || mount -t bpf bpf /sys/fs/bpf",
            ],
        )
        .await
        .expect("exec bpf mount");
        if !mounted.success() {
            fail_with_evidence(&cluster, &format!("bpf mount failed: {mounted:?}")).await;
        }
    }

    let shas = ArtifactShas::read(docker, &cluster, &core).await;

    // Join template first carries a syntactically valid placeholder CA: it
    // must parse when ployzd-control first starts, before the cluster CA
    // exists. It is replaced with the real CA right after keeper install.
    let placeholder_ca =
        "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n".to_owned();
    write_join_template(
        docker,
        &cluster,
        &core,
        &core_nats_url,
        &placeholder_ca,
        &shas,
    )
    .await;

    let install_spec = serde_json::json!({
        "node_id": "core_1",
        "gateway": "install",
        "node_public_ip": core_ip,
        "machine_bootstrap_url": "https://local.invalid/ployz.sh",
        "machine_join_template_file": "/etc/ployz/machine-join-template.json",
        "artifacts": {
            "ployzd": shas.ployzd_descriptor(),
            "ebpf_bytecode": shas.ebpf_bytecode_descriptor(),
            "ebpf_ctl": shas.ebpf_ctl_descriptor(),
            "nats_server": {
                "version": "local",
                "source": "/usr/local/bin/nats-server",
                "sha256": shas.nats_server,
                "binary": "/usr/local/bin/nats-server",
                "config": "/etc/nats/nats-server.conf"
            }
        }
    });
    write_file_in_container(
        docker,
        &core.container_id,
        "/tmp/ployz-first-node-install.json",
        &serde_json::to_string_pretty(&install_spec).expect("install spec serializes"),
        "0644",
    )
    .await
    .expect("write first-node install spec");

    let init = exec_in_container(
        docker,
        &core.container_id,
        &[
            &format!("{ARTIFACTS_MOUNT_PATH}/ployzctl"),
            "init",
            "--run-keeper-install",
            "--install-spec",
            "/tmp/ployz-first-node-install.json",
            "--keeper-binary",
            &format!("{ARTIFACTS_MOUNT_PATH}/ployz-keeper"),
        ],
    )
    .await
    .expect("exec ployzctl init");
    if !init.success() {
        fail_with_evidence(
            &cluster,
            &format!(
                "keeper first-node install failed (exit {}): {}\n{}",
                init.exit_code, init.stdout, init.stderr
            ),
        )
        .await;
    }

    // The keeper minted the cluster CA during install; the join template
    // can only carry the real CA now. Re-render it and restart the
    // (disposable) control role so machine-add bundles hand out the
    // trusted CA.
    let ca_pem = read_file_from_container(
        docker,
        &core.container_id,
        &format!("{NATS_MATERIAL_DIR}/ca.pem"),
    )
    .await
    .expect("read cluster CA");
    write_join_template(docker, &cluster, &core, &core_nats_url, &ca_pem, &shas).await;
    let restart = exec_in_container(
        docker,
        &core.container_id,
        &["systemctl", "restart", "ployzd-control"],
    )
    .await
    .expect("restart ployzd-control");
    if !restart.success() {
        fail_with_evidence(&cluster, &format!("control restart failed: {restart:?}")).await;
    }

    // Copy the host-side material out of the core container.
    let dir = tempfile::TempDir::new().expect("create host material dir");
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&ca_file, &ca_pem).expect("write host CA file");
    let mut seeds = Vec::with_capacity(3);
    for name in ["operator.seed", "controller.seed", "join.seed"] {
        let contents = read_file_from_container(
            docker,
            &core.container_id,
            &format!("{NATS_MATERIAL_DIR}/{name}"),
        )
        .await
        .expect("read seed from core");
        seeds.push(contents.trim().to_owned());
    }
    let [operator_seed, controller_seed, join_seed] = seeds.try_into().expect("three seeds read");
    let material = ClusterMaterial {
        _dir: dir,
        ca_file,
        ca_pem,
        operator_seed,
        controller_seed,
        join_seed,
    };

    // Authenticated host-side operator client through the published
    // 127.0.0.1 port (127.0.0.1 is in the server-cert SANs).
    CoreContext {
        docker: docker.clone(),
        core_ip,
        api: wait_for_operator_api(&cluster, &material).await,
        material,
        cluster,
    }
}

/// Pinned artifact digests computed inside the core machine, with the
/// in-machine source/install paths the keeper consumes.
struct ArtifactShas {
    ployzd: String,
    ebpf_bytecode: String,
    ebpf_ctl: String,
    nats_server: String,
}

impl ArtifactShas {
    async fn read(docker: &Docker, cluster: &DindCluster, core: &DindMachine) -> Self {
        Self {
            ployzd: sha256_of(
                docker,
                cluster,
                core,
                &format!("{ARTIFACTS_MOUNT_PATH}/ployzd"),
            )
            .await,
            ebpf_bytecode: sha256_of(
                docker,
                cluster,
                core,
                &format!("{ARTIFACTS_MOUNT_PATH}/ployz-ebpf-tc"),
            )
            .await,
            ebpf_ctl: sha256_of(
                docker,
                cluster,
                core,
                &format!("{ARTIFACTS_MOUNT_PATH}/ployz-ebpf-ctl"),
            )
            .await,
            nats_server: sha256_of(docker, cluster, core, "/usr/local/bin/nats-server").await,
        }
    }

    fn ployzd_descriptor(&self) -> serde_json::Value {
        serde_json::json!({
            "version": "local",
            "source": format!("{ARTIFACTS_MOUNT_PATH}/ployzd"),
            "sha256": self.ployzd,
            "install_path": "/usr/local/bin/ployzd"
        })
    }

    fn ebpf_bytecode_descriptor(&self) -> serde_json::Value {
        serde_json::json!({
            "version": "local",
            "source": format!("{ARTIFACTS_MOUNT_PATH}/ployz-ebpf-tc"),
            "sha256": self.ebpf_bytecode,
            "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
        })
    }

    fn ebpf_ctl_descriptor(&self) -> serde_json::Value {
        serde_json::json!({
            "version": "local",
            "source": format!("{ARTIFACTS_MOUNT_PATH}/ployz-ebpf-ctl"),
            "sha256": self.ebpf_ctl,
            "install_path": "/usr/local/bin/ployz-ebpf-ctl"
        })
    }
}

async fn sha256_of(
    docker: &Docker,
    cluster: &DindCluster,
    machine: &DindMachine,
    path: &str,
) -> String {
    let outcome = exec_in_container(docker, &machine.container_id, &["sha256sum", path])
        .await
        .expect("exec sha256sum");
    if !outcome.success() {
        fail_with_evidence(cluster, &format!("sha256sum {path} failed: {outcome:?}")).await;
    }
    let Some(digest) = outcome.stdout.split_whitespace().next() else {
        fail_with_evidence(cluster, &format!("empty sha256sum output for {path}")).await;
    };
    digest.to_owned()
}

async fn write_join_template(
    docker: &Docker,
    cluster: &DindCluster,
    core: &DindMachine,
    runtime_nats_url: &str,
    ca_pem: &str,
    shas: &ArtifactShas,
) {
    let template = serde_json::json!({
        "join_bundle": {
            "material": {
                "cluster_name": "dind-e2e",
                "runtime_nats_url": runtime_nats_url,
                "trusted_nats": { "ca_pem": ca_pem },
                "ployzd": shas.ployzd_descriptor(),
                "ebpf_bytecode": shas.ebpf_bytecode_descriptor(),
                "ebpf_ctl": shas.ebpf_ctl_descriptor(),
            }
        }
    });
    let written = write_file_in_container(
        docker,
        &core.container_id,
        "/etc/ployz/machine-join-template.json",
        &serde_json::to_string_pretty(&template).expect("join template serializes"),
        "0644",
    )
    .await;
    if written.is_err() {
        fail_with_evidence(
            cluster,
            &format!("writing join template failed: {written:?}"),
        )
        .await;
    }
}

/// Connects an authenticated operator client through the published port and
/// waits until the control API answers `machine list`.
async fn wait_for_operator_api(
    cluster: &DindCluster,
    material: &ClusterMaterial,
) -> OperationApiClient {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last_error = String::from("<no attempt>");
    while Instant::now() < deadline {
        let connect = host_client_config(
            cluster,
            material,
            NatsPrincipal::User,
            &material.operator_seed,
        );
        match connect_authenticated(&connect, CONNECT_TIMEOUT).await {
            Ok(client) => {
                let api = OperationApiClient::new(client);
                match api.machine_list(&MachineListRequest {}).await {
                    Ok(_) => return api,
                    Err(error) => last_error = format!("machine list: {error}"),
                }
            }
            Err(error) => last_error = format!("connect: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    fail_with_evidence(
        cluster,
        &format!("control API did not become ready: {last_error}"),
    )
    .await
}

fn host_client_config(
    cluster: &DindCluster,
    material: &ClusterMaterial,
    principal: NatsPrincipal,
    seed: &str,
) -> NatsConnectConfig {
    let published = cluster.core().published.nats;
    NatsConnectConfig {
        url: NatsClientUrl::try_new(format!("tls://{published}")).expect("valid published url"),
        auth: NatsClientAuth::NkeySeed(
            NatsUserSeed::try_new(seed.trim()).expect("valid NKey user seed"),
        ),
        trust: NatsTlsTrust::ClusterCa(material.ca_file.clone()),
        principal,
    }
}

async fn connect_core_client(
    core: &CoreContext,
    principal: NatsPrincipal,
    seed: &str,
) -> Result<async_nats::Client, ployz_nats::connect::NatsConnectError> {
    let config = host_client_config(&core.cluster, &core.material, principal, seed);
    connect_authenticated(&config, CONNECT_TIMEOUT).await
}

/// Activates the first node through the in-machine product CLI; scenario 2
/// uses this as plumbing (scenario 1 owns the detailed assertions).
async fn activate_first_node(core: &CoreContext) -> OperationId {
    let activate = exec_sh(
        core,
        core.cluster.core(),
        &format!(
            "PLOYZ_NATS_CA_FILE={NATS_MATERIAL_DIR}/ca.pem \
             PLOYZ_NATS_NKEY_SEED_FILE={NATS_MATERIAL_DIR}/operator.seed \
             {ARTIFACTS_MOUNT_PATH}/ployzctl --nats tls://127.0.0.1:{MACHINE_NATS_PORT} \
             init activate-first-node --node core_1 --gateway"
        ),
    )
    .await;
    if !activate.success() {
        fail_with_evidence(
            &core.cluster,
            &format!(
                "activate-first-node failed (exit {}): {}\n{}",
                activate.exit_code, activate.stdout, activate.stderr
            ),
        )
        .await;
    }
    let Some(operation_id) = parse_operation_line(&activate.stdout) else {
        fail_with_evidence(
            &core.cluster,
            &format!("no operation id in activate output: {}", activate.stdout),
        )
        .await;
    };
    operation_id
}

/// Adds the edge machine through the host-side API and runs the
/// `scripts/ployz.sh` join flow on it; scenarios 3–5 use this as plumbing
/// (scenario 2 owns the detailed join assertions).
async fn add_and_join_edge(core: &CoreContext, edge: &DindMachine) {
    let add_operation = operation_id("op_add_edge_2");
    let accepted = match core
        .api
        .machine_add(&MachineAddRequest {
            operation_id: add_operation.clone(),
            idempotency_key: idempotency_key("idem_add_edge_2"),
            node_id: node_id("edge_2"),
            name: machine_name("edge-2"),
            gateway: MachineAddGateway::Install,
        })
        .await
    {
        Ok(accepted) => accepted,
        Err(error) => {
            fail_with_evidence(&core.cluster, &format!("machine add failed: {error}")).await;
        }
    };
    let install = parse_install_line(core, accepted);
    run_edge_join(core, edge, &install).await;
    let status = operation_status(core, &add_operation).await;
    let completed = matches!(
        &status,
        OperationStatus::MachineAdd {
            state: MachineAddOperationState::Completed,
            ..
        }
    );
    if !completed {
        fail_with_evidence(
            &core.cluster,
            &format!("machine add did not complete: {status:?}"),
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// Edge join (scripts/ployz.sh flow with the printed install material)
// ---------------------------------------------------------------------------

/// The env the product prints into the machine-add install command line.
#[derive(Debug)]
struct InstallLine {
    nats_url: String,
    nats_ca_b64: String,
    join_seed: String,
    join_token: String,
}

/// Renders the product's machine-add output (the same text `ployzctl
/// machine add` prints) and parses the install env + join token out of it.
fn parse_install_line(core: &CoreContext, accepted: MachineAddAccepted) -> InstallLine {
    let join_seed =
        NatsUserSeed::try_new(core.material.join_seed.trim()).expect("valid cluster join seed");
    let rendered = MachineAddOutput::from_accepted(accepted, join_seed).render();
    let Some(install) = rendered
        .lines()
        .find_map(|line| line.strip_prefix("install "))
    else {
        panic!("machine-add output has no install line: {rendered}");
    };
    let Some(token) = rendered
        .lines()
        .find_map(|line| line.strip_prefix("join-token "))
    else {
        panic!("machine-add output has no join-token line: {rendered}");
    };
    InstallLine {
        nats_url: install_line_env(install, "PLOYZ_NATS_URL"),
        nats_ca_b64: install_line_env(install, "PLOYZ_NATS_CA_B64"),
        join_seed: install_line_env(install, "PLOYZ_JOIN_NKEY_SEED"),
        join_token: token.trim().to_owned(),
    }
}

/// Reads one env assignment off the install command line (values may or may
/// not be shell-quoted) — the proof script's `install_line_env`.
fn install_line_env(line: &str, name: &str) -> String {
    let Some((_, rest)) = line.split_once(&format!("{name}=")) else {
        panic!("install line is missing {name}: {line}");
    };
    let value = rest.split_whitespace().next().unwrap_or_default();
    value
        .trim_start_matches('\'')
        .trim_end_matches('\'')
        .to_owned()
}

/// Runs the real `scripts/ployz.sh` join flow on the edge machine with
/// exactly the material the product printed.
async fn run_edge_join(core: &CoreContext, edge: &DindMachine, install: &InstallLine) {
    let ployz_sh = std::fs::read_to_string(repo_path("scripts/ployz.sh"))
        .expect("read scripts/ployz.sh from the repo");
    write_file_in_container(
        &core.docker,
        &edge.container_id,
        "/tmp/ployz.sh",
        &ployz_sh,
        "0755",
    )
    .await
    .expect("write ployz.sh into edge");

    let keeper_sha = sha256_of(
        &core.docker,
        &core.cluster,
        edge,
        &format!("{ARTIFACTS_MOUNT_PATH}/ployz-keeper"),
    )
    .await;
    let join = exec_sh(
        core,
        edge,
        &format!(
            "PLOYZ_KEEPER_URL=file://{ARTIFACTS_MOUNT_PATH}/ployz-keeper \
             PLOYZ_KEEPER_SHA256={keeper_sha} \
             PLOYZ_NATS_URL={} PLOYZ_NATS_CA_B64={} PLOYZ_JOIN_NKEY_SEED={} \
             PLOYZ_NODE_PUBLIC_IP={} \
             sh /tmp/ployz.sh --join-token {}",
            shell_quote(&install.nats_url),
            shell_quote(&install.nats_ca_b64),
            shell_quote(&install.join_seed),
            edge.bridge_ip,
            shell_quote(&install.join_token),
        ),
    )
    .await;
    if !join.success() {
        fail_with_evidence(
            &core.cluster,
            &format!(
                "edge join failed (exit {}): {}\n{}",
                join.exit_code, join.stdout, join.stderr
            ),
        )
        .await;
    }
}

fn repo_path(relative: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

// ---------------------------------------------------------------------------
// Assertions
// ---------------------------------------------------------------------------

async fn exec_on(core: &CoreContext, machine: &DindMachine, command: &[&str]) -> ExecOutcome {
    match exec_in_container(&core.docker, &machine.container_id, command).await {
        Ok(outcome) => outcome,
        Err(error) => {
            fail_with_evidence(&core.cluster, &format!("exec {command:?} failed: {error}")).await
        }
    }
}

async fn exec_sh(core: &CoreContext, machine: &DindMachine, script: &str) -> ExecOutcome {
    exec_on(core, machine, &["sh", "-c", script]).await
}

async fn assert_unit_active(core: &CoreContext, machine: &DindMachine, unit: &str) {
    let outcome = exec_on(core, machine, &["systemctl", "is-active", unit]).await;
    if outcome.stdout.trim() != "active" {
        fail_with_evidence(
            &core.cluster,
            &format!("unit {unit} on {} is not active: {outcome:?}", machine.name),
        )
        .await;
    }
}

async fn unit_main_pid(core: &CoreContext, machine: &DindMachine, unit: &str) -> String {
    let outcome = exec_on(
        core,
        machine,
        &["systemctl", "show", "-p", "MainPID", "--value", unit],
    )
    .await;
    let pid = outcome.stdout.trim().to_owned();
    if !outcome.success() || pid.is_empty() || pid == "0" {
        fail_with_evidence(
            &core.cluster,
            &format!(
                "unit {unit} on {} has no main pid: {outcome:?}",
                machine.name
            ),
        )
        .await;
    }
    pid
}

/// Polls the units' journal until the marker shows up (journald may lag the
/// unit's stderr by a moment).
async fn assert_journal_contains(core: &CoreContext, units: &[&str], marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = String::new();
    while Instant::now() < deadline {
        let mut command = vec!["journalctl", "--no-pager"];
        for unit in units {
            command.push("-u");
            command.push(unit);
        }
        let outcome = exec_on(core, core.cluster.core(), &command).await;
        if outcome.stdout.contains(marker) {
            return;
        }
        last = outcome.stdout;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    fail_with_evidence(
        &core.cluster,
        &format!("journal of {units:?} never contained {marker:?}: {last}"),
    )
    .await
}

async fn operation_status(core: &CoreContext, operation_id: &OperationId) -> OperationStatus {
    match core
        .api
        .ops_status(&OpsStatusRequest {
            operation_id: operation_id.clone(),
        })
        .await
    {
        Ok(snapshot) => snapshot.status,
        Err(error) => {
            fail_with_evidence(
                &core.cluster,
                &format!("ops status {operation_id:?} failed: {error}"),
            )
            .await
        }
    }
}

async fn operation_events(core: &CoreContext, operation_id: &OperationId) -> Vec<OperationEvent> {
    match core
        .api
        .ops_watch(&OperationEventReplayRequest {
            operation_id: operation_id.clone(),
            start_sequence: event_sequence(1),
            limit: event_replay_limit(64),
        })
        .await
    {
        Ok(page) => page.events.into_iter().map(|event| event.event).collect(),
        Err(error) => {
            fail_with_evidence(
                &core.cluster,
                &format!("ops watch {operation_id:?} failed: {error}"),
            )
            .await
        }
    }
}

/// Replays the full event history of a terminal operation.
async fn terminal_operation_events(
    core: &CoreContext,
    operation_id: &OperationId,
) -> Vec<OperationEvent> {
    match core
        .api
        .ops_watch(&OperationEventReplayRequest {
            operation_id: operation_id.clone(),
            start_sequence: event_sequence(1),
            limit: event_replay_limit(64),
        })
        .await
    {
        Ok(page) => {
            if page.cursor != OperationEventReplayCursor::Terminal {
                fail_with_evidence(
                    &core.cluster,
                    &format!("operation {operation_id:?} replay is not terminal: {page:?}"),
                )
                .await;
            }
            page.events.into_iter().map(|event| event.event).collect()
        }
        Err(error) => {
            fail_with_evidence(
                &core.cluster,
                &format!("ops watch {operation_id:?} failed: {error}"),
            )
            .await
        }
    }
}

/// The committed machine-add event vocabulary: submitted, then the five
/// mint steps in order, then joined, then completed — with acceptance
/// strictly before the reload.
async fn assert_machine_add_event_sequence(
    cluster: &DindCluster,
    events: &[OperationEvent],
    expected_node: &NodeId,
) {
    let submitted = position(events, |event| {
        matches!(
            event,
            OperationEvent::MachineAddSubmitted { node_id, .. } if node_id == expected_node
        )
    });
    let steps = [
        MachineCredentialProvisioningStep::Minted,
        MachineCredentialProvisioningStep::Rendered,
        MachineCredentialProvisioningStep::Reloaded,
        MachineCredentialProvisioningStep::Verified,
        MachineCredentialProvisioningStep::MaterialReady,
    ]
    .map(|expected_step| {
        position(events, |event| {
            matches!(
                event,
                OperationEvent::MachineAddCredentialProvisioned { step, node_id, .. }
                    if *step == expected_step && node_id == expected_node
            )
        })
    });
    let joined = position(events, |event| {
        matches!(
            event,
            OperationEvent::MachineAddJoined { node_id, .. } if node_id == expected_node
        )
    });
    let completed = position(events, |event| {
        matches!(
            event,
            OperationEvent::MachineAddCompleted { node_id, .. } if node_id == expected_node
        )
    });

    let mut order = vec![submitted];
    order.extend(steps);
    order.push(joined);
    order.push(completed);
    let mut resolved = Vec::with_capacity(order.len());
    for entry in order {
        match entry {
            Some(index) => resolved.push(index),
            None => {
                fail_with_evidence(
                    cluster,
                    &format!("missing machine-add event for {expected_node:?}: {events:?}"),
                )
                .await;
            }
        }
    }
    if !resolved.is_sorted() {
        fail_with_evidence(
            cluster,
            &format!("machine-add events out of order for {expected_node:?}: {events:?}"),
        )
        .await;
    }
}

fn position(
    events: &[OperationEvent],
    predicate: impl Fn(&OperationEvent) -> bool,
) -> Option<usize> {
    events.iter().position(predicate)
}

/// Waits until the machine snapshot carries node observations (public ip
/// from the node process and gateway status from the gateway process) —
/// proof both processes connected with the machine's Node credential.
async fn wait_for_machine_observations(core: &CoreContext, machine: &NodeId) {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last = String::from("<no inspect yet>");
    while Instant::now() < deadline {
        match core
            .api
            .machine_inspect(&MachineInspectRequest {
                node_id: machine.clone(),
            })
            .await
        {
            Ok(snapshot) => {
                if snapshot.public_ip.is_some() && snapshot.gateway.is_some() {
                    return;
                }
                last = format!("{snapshot:?}");
            }
            Err(error) => last = format!("{error}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    fail_with_evidence(
        &core.cluster,
        &format!("machine {machine:?} never published observations: {last}"),
    )
    .await
}

/// Bootstrap evidence on the secured server: the KV buckets and streams the
/// control plane runs on exist (read with the Controller credential, whose
/// profile carries `$JS.API.>`).
async fn assert_bootstrap_resources_exist(core: &CoreContext) {
    let client = match connect_core_client(
        core,
        NatsPrincipal::Controller,
        &core.material.controller_seed,
    )
    .await
    {
        Ok(client) => client,
        Err(error) => {
            fail_with_evidence(
                &core.cluster,
                &format!("controller principal could not connect: {error}"),
            )
            .await
        }
    };
    let jetstream = async_nats::jetstream::new(client);
    for bucket in ["KV_CORE", "KV_OPS"] {
        if let Err(error) = jetstream.get_key_value(bucket).await {
            fail_with_evidence(
                &core.cluster,
                &format!("bootstrap KV bucket {bucket} missing: {error}"),
            )
            .await;
        }
    }
    for stream in ["PLZ_OPS", "PLZ_JOBS"] {
        if let Err(error) = jetstream.get_stream(stream).await {
            fail_with_evidence(
                &core.cluster,
                &format!("bootstrap stream {stream} missing: {error}"),
            )
            .await;
        }
    }
}

fn decode_base64(value: &str) -> String {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .expect("install line CA is valid base64");
    String::from_utf8(bytes).expect("install line CA is UTF-8")
}

fn parse_operation_line(output: &str) -> Option<OperationId> {
    output.lines().find_map(|line| {
        line.strip_prefix("operation ")
            .and_then(|id| OperationId::try_new(id.trim()).ok())
    })
}

/// Tears the cluster down unless `PLOYZ_DIND_KEEP=1`.
async fn finish(core: CoreContext) {
    if dind::keep_requested() {
        eprintln!(
            "PLOYZ_DIND_KEEP=1: keeping run {} (network {}, core container {})",
            core.cluster.run_id(),
            core.cluster.network_name(),
            core.cluster.core().container_id,
        );
        return;
    }
    core.cluster
        .teardown()
        .await
        .expect("teardown DinD cluster");
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}

fn machine_name(value: &str) -> MachineName {
    MachineName::try_new(value).expect("valid machine name")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn route_hostname(value: &str) -> RouteHostname {
    RouteHostname::try_new(value).expect("valid route hostname")
}

fn route_port(value: u16) -> RoutePort {
    RoutePort::try_new(value).expect("valid route port")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn event_replay_limit(value: u16) -> OperationEventReplayLimit {
    OperationEventReplayLimit::try_new(value).expect("valid event replay limit")
}

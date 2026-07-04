//! Gated Docker-in-Docker harness tests.
//!
//! Run with: `PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test dind_cluster
//! -- --test-threads=1`. Requires the machine image from
//! `scripts/build-dind-machine-image.sh` and Docker with `--privileged`
//! support. `PLOYZ_DIND_KEEP=1` keeps the cluster running for debugging;
//! `scripts/dind-clean.sh` sweeps leftovers.
//!
//! Scenarios form the core through the product quick-start command
//! `ployzctl machine init root@core` (driven over a docker-exec-backed
//! stand-in ssh), and scenarios 2–5 join the edge through the product
//! `ployzctl machine add root@edge`.
//!
//! Every scenario body runs inside [`support::dind::with_evidence`], so any
//! failed assertion captures whole-cluster evidence before the panic
//! propagates.

mod support;

use ployz_core::deploy::{
    DeployRequest, DeployRoute, DeployServiceSpec, ImageReference, ReplicaCount,
};
use ployz_core::ids::MachineId;
use ployz_core::machine::{MachineAddOperationState, MachineCredentialProvisioningStep};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, OperationEvent, OperationStatus, RouteTarget,
};
use ployz_core::permissions::inbox_subscribe_scope;
use ployz_core::security::NatsPrincipal;
use ployz_core::state::KV_CORE_BUCKET;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_e2e::bollard::query_parameters::{
    ListContainersOptionsBuilder, ListNetworksOptionsBuilder,
};
use ployz_e2e::dind::{
    self, DindCluster, DindClusterSpec, DindMachine, DindMachineRole, MACHINE_NATS_PORT,
    MachineSpec, exec_in_container, read_file_from_container,
};
use ployz_nats::connect::{NatsClientUrl, connect_with_timeout};
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    DeploySubmitRequest, MachineJoinRedeemError, MachineJoinRedeemRequest, MachineListRequest,
    MachineSnapshot,
};
use ployz_test_support::ids::{
    idempotency_key, machine_id, namespace_id, namespace_revision_id, operation_id, route_hostname,
    route_port, service_id,
};
use ployz_test_support::nats::SecuredTestNats;
use ployzd::docker::labels::{
    CONTAINER_TYPE_LABEL, NAMESPACE_REVISION_ENTRY_LABEL, OPERATION_ID_LABEL, SERVICE_ID_LABEL,
};
use std::collections::HashMap;
use std::time::Duration;
use support::dind::assert::{
    assert_deploy_event_sequence, assert_machine_add_event_sequence, assert_unit_active,
    connect_with_event_capture, decode_base64, gateway_http_get, managed_workload_containers,
    next_permission_violation, operation_events, operation_status, terminal_operation_events,
    wait_for_inspect, wait_for_machine_observations, wait_for_terminal_deploy_status,
};
use support::dind::formation::{
    CoreContext, add_and_join_edge, connect_core_client, finish, host_client_config,
    init_core_cluster, submit_machine_add,
};
use support::dind::join::{parse_install_line, run_edge_join};
use support::dind::{AUTHORIZED_USERS_FILE, CONNECT_TIMEOUT, EDGE_NATS_CREDS_FILE, with_evidence};

/// The workload image `scripts/build-dind-machine-image.sh` bakes into the
/// machine image; the inner Docker daemons load it at boot.
const WORKLOAD_IMAGE: &str = "nginx:1.27-alpine";
/// Route hostname the smoke deploy registers on both gateways.
const ROUTE_HOSTNAME: &str = "smoke.local";
/// Port nginx listens on inside its workload container.
const WORKLOAD_ENDPOINT_PORT: u16 = 80;
/// Budget for the routed two-machine deploy to reach a terminal state
/// (includes the image pull check and WireGuard/eBPF preparation).
const DEPLOY_TERMINAL_BUDGET: Duration = Duration::from_secs(300);

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
    with_evidence(&cluster, async {
        let system_state = exec_in_container(
            &docker,
            &cluster.core().container_id,
            &["systemctl", "is-system-running"],
        )
        .await;
        assert!(
            matches!(
                &system_state,
                Ok(outcome) if matches!(outcome.stdout.trim(), "running" | "degraded")
            ),
            "core systemd not ready: {system_state:?}"
        );

        let inner_docker =
            exec_in_container(&docker, &cluster.core().container_id, &["docker", "info"]).await;
        assert!(
            matches!(&inner_docker, Ok(outcome) if outcome.success()),
            "inner docker not ready: {inner_docker:?}"
        );

        let artifacts = exec_in_container(
            &docker,
            &cluster.core().container_id,
            &["test", "-x", "/opt/ployz/artifacts/ployzd"],
        )
        .await;
        assert!(
            matches!(&artifacts, Ok(outcome) if outcome.success()),
            "artifact mount missing executable ployzd: {artifacts:?}"
        );
    })
    .await;

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

/// Scenario 1 — machine init forms a TLS-authenticated core through product
/// commands only and activates the first machine.
#[tokio::test]
async fn scenario_init_and_activate_first_machine() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 0).await;
    with_evidence(&core.cluster, async {
        let cluster = &core.cluster;
        let machine_unit = "ployzd-machine-core_1";
        let gateway_unit = "ployzd-gateway";
        for unit in ["nats-server", "ployzd-control", machine_unit, gateway_unit] {
            assert_unit_active(&core, cluster.core(), unit).await;
        }

        let machine_seed = read_file_from_container(
            &docker,
            &cluster.core().container_id,
            "/var/lib/ployz/nats/machine.seed",
        )
        .await
        .expect("machine.seed exists after activate");
        assert!(
            machine_seed.trim().starts_with("SU"),
            "machine.seed is an NKey user seed"
        );

        wait_for_machine_observations(&core, &machine_id("core_1")).await;

        let authorized =
            read_file_from_container(&docker, &cluster.core().container_id, AUTHORIZED_USERS_FILE)
                .await
                .expect("authorized-users.conf is readable");
        for principal in ["controller", "user", "join", "machine_core_1"] {
            assert!(
                authorized.contains(&format!("# ployz-principal: {principal}")),
                "authorized-users.conf must contain {principal}: {authorized}"
            );
        }

        assert_bootstrap_resources_exist(&core).await;
    })
    .await;

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
    // Product machine init forms and activates the core; this scenario owns
    // the machine-add/join details through the explicit low-level path.
    let core = init_core_cluster(&docker, 1).await;
    with_evidence(&core.cluster, async {
        let cluster = &core.cluster;

        let add_operation = operation_id("op_add_edge_2");
        let accepted = submit_machine_add(&core).await;
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
            format!("tls://{}:{MACHINE_NATS_PORT}", core.core_ip()),
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
            panic!("machine add is not a machine add: {status:?}");
        };
        assert_eq!(
            state,
            MachineAddOperationState::Completed,
            "machine add not completed"
        );
        let events = terminal_operation_events(&core, &add_operation).await;
        assert_machine_add_event_sequence(&events, &machine_id("edge_2"));

        // nats_connection readiness evidence: the edge's machine process connects
        // with its minted credential and publishes observations.
        wait_for_machine_observations(&core, &machine_id("edge_2")).await;

        // The edge holds its own minted seed — not the controller's.
        let edge_creds =
            read_file_from_container(&docker, &edge.container_id, EDGE_NATS_CREDS_FILE)
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
        for principal in [
            "controller",
            "user",
            "join",
            "machine_core_1",
            "machine_edge_2",
        ] {
            assert!(
                authorized.contains(&format!("# ployz-principal: {principal}")),
                "authorized-users.conf must keep {principal}: {authorized}"
            );
        }

        // No separate gateway credential exists: the edge gateway role env
        // points its seed file at the machine's Machine creds.
        let gateway_env =
            read_file_from_container(&docker, &edge.container_id, "/etc/ployz/ployzd-gateway.env")
                .await
                .expect("edge gateway env file exists");
        assert!(
            gateway_env.contains(&format!("PLOYZ_NATS_NKEY_SEED_FILE={EDGE_NATS_CREDS_FILE}")),
            "edge gateway must authenticate with the Machine creds: {gateway_env}"
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
                panic!("token re-redeem must be refused as not-pending: {other:?}");
            }
        }
    })
    .await;

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
///   `ployzd-control` (core) and the edge machine unit neither interrupts
///   gateway HTTP nor replaces workload containers, and the operations API
///   answers afterwards with unmutated machine state.
/// - **Scenario 5 — auth rejection:** unauthorized seeds, plaintext
///   clients, and over-reaching Machine/Join principals are refused by the
///   real cluster — which keeps serving afterwards.
#[tokio::test]
async fn scenario_deploy_restart_invisibility_and_auth_rejection() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    // The product quick-start path: machine init forms and activates the
    // core, machine add joins the edge.
    let core = init_core_cluster(&docker, 1).await;
    with_evidence(&core.cluster, async {
        let cluster = &core.cluster;
        let [edge] = cluster.edges() else {
            panic!("scenario requires exactly one edge machine");
        };
        add_and_join_edge(&core, edge).await;
        wait_for_machine_observations(&core, &machine_id("core_1")).await;
        wait_for_machine_observations(&core, &machine_id("edge_2")).await;

        scenario_cross_machine_deploy(&core, edge).await;
        scenario_daemon_restart_invisibility(&core, edge).await;
        scenario_auth_rejection(&core, edge).await;
    })
    .await;

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
    let accepted = core
        .api
        .deploy_submit(&DeploySubmitRequest {
            idempotency_key: idempotency_key("idem_dind_deploy"),
            target: smoke_deploy_target(),
        })
        .await
        .expect("deploy submits");
    let deploy_operation = accepted.operation_id;

    let status =
        wait_for_terminal_deploy_status(core, &deploy_operation, DEPLOY_TERMINAL_BUDGET).await;
    assert!(
        matches!(
            &status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
                ..
            }
        ),
        "deploy did not complete: {status:?}"
    );
    let events = terminal_operation_events(core, &deploy_operation).await;
    assert_deploy_event_sequence(&events, &deploy_operation);

    // Docker is execution reality: each machine runs exactly one managed
    // workload container carrying the product's exact label values.
    for machine in [cluster.core(), edge] {
        let containers = managed_workload_containers(core, machine).await;
        let [container] = containers.as_slice() else {
            panic!(
                "expected one managed container on {}, got {containers:?}",
                machine.name
            );
        };
        for (key, value) in [
            (SERVICE_ID_LABEL, "svc_smoke".to_owned()),
            (NAMESPACE_REVISION_ENTRY_LABEL, "rev_local".to_owned()),
            (OPERATION_ID_LABEL, deploy_operation.as_str().to_owned()),
            (CONTAINER_TYPE_LABEL, "service".to_owned()),
        ] {
            assert_eq!(
                container.labels.get(key),
                Some(&value),
                "managed container on {} must carry {key}={value}: {container:?}",
                machine.name
            );
        }
    }

    // The route serves through BOTH gateways via the published ports.
    for machine in [cluster.core(), edge] {
        assert_gateway_serves(machine).await;
    }
}

/// The smoke service: the baked workload image, one replica per machine,
/// routed on every gateway's listen port.
fn smoke_deploy_target() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("smoke"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_smoke"),
            image: ImageReference::try_new(WORKLOAD_IMAGE).expect("valid workload image reference"),
            replicas: ReplicaCount::try_new(2).expect("valid replica count"),
            routes: vec![DeployRoute {
                target: RouteTarget::new(
                    route_hostname(ROUTE_HOSTNAME),
                    route_port(dind::MACHINE_GATEWAY_PORT),
                ),
                endpoint_port: route_port(WORKLOAD_ENDPOINT_PORT),
            }],
        }],
    }
}

/// The route host header: hostname plus the gateway's in-machine listen
/// port (the route target port), not the published host port.
fn route_host_header() -> String {
    format!("{ROUTE_HOSTNAME}:{}", dind::MACHINE_GATEWAY_PORT)
}

/// Asserts the smoke route answers through one machine's published gateway
/// port with the workload's response body.
async fn assert_gateway_serves(machine: &DindMachine) {
    match gateway_http_get(machine.published.gateway, &route_host_header()).await {
        Ok(response) if response.contains("Welcome to nginx") => {}
        Ok(response) => panic!(
            "gateway on {} answered without the workload body: {response}",
            machine.name
        ),
        Err(error) => panic!("gateway on {} did not answer: {error}", machine.name),
    }
}

// ---------------------------------------------------------------------------
// Scenario 4 — daemon-restart invisibility
// ---------------------------------------------------------------------------

/// Restarts the control daemon on the core and the machine daemon on the edge
/// while the deploy is serving: gateway HTTP must keep answering throughout,
/// workload containers must be adopted (same IDs), and the operations API
/// must answer afterwards with unmutated machine state.
async fn scenario_daemon_restart_invisibility(core: &CoreContext, edge: &DindMachine) {
    let core_machine = core.cluster.core();

    let core_snapshot_before = wait_for_settled_snapshot(core, &machine_id("core_1")).await;
    let edge_snapshot_before = wait_for_settled_snapshot(core, &machine_id("edge_2")).await;
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
        let machine = exec_in_container(
            &docker,
            &edge_container,
            &["systemctl", "restart", "ployzd-machine-edge_2"],
        )
        .await;
        (control, machine)
    });

    // Gateway HTTP keeps answering during the whole restart window.
    let mut polls: u32 = 0;
    loop {
        let finished = restart.is_finished();
        for machine in [core_machine, edge] {
            assert_gateway_serves(machine).await;
        }
        polls += 1;
        if finished {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let restarted = restart.await.expect("restart task does not panic");
    match restarted {
        (Ok(control), Ok(machine)) if control.success() && machine.success() => {}
        other => panic!("daemon restarts failed: {other:?}"),
    }
    assert!(
        polls >= 2,
        "gateways must be polled during the restart window, not only after it"
    );
    assert_unit_active(core, core_machine, "ployzd-control").await;
    assert_unit_active(core, edge, "ployzd-machine-edge_2").await;

    // Adopt-not-recreate: the inner Docker container IDs are unchanged.
    let core_containers_after = workload_container_ids(core, core_machine).await;
    let edge_containers_after = workload_container_ids(core, edge).await;
    assert_eq!(
        core_containers_before, core_containers_after,
        "core workload containers must survive the control restart"
    );
    assert_eq!(
        edge_containers_before, edge_containers_after,
        "edge workload containers must survive the machine restart"
    );

    // The operations API answers after reconnect with unmutated machine
    // state (gateway status is the restarted processes' own live
    // observation, so the comparison covers identity, public ip, and the
    // observed container set).
    wait_for_matching_snapshot(core, &machine_id("core_1"), &core_snapshot_before).await;
    wait_for_matching_snapshot(core, &machine_id("edge_2"), &edge_snapshot_before).await;
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
async fn wait_for_settled_snapshot(core: &CoreContext, machine: &MachineId) -> MachineSnapshot {
    wait_for_inspect(
        core,
        machine,
        Duration::from_secs(60),
        "never settled after the deploy",
        |snapshot| {
            snapshot.public_ip.is_some()
                && snapshot.gateway.is_some()
                && snapshot.observed_container_count == 1
        },
    )
    .await
}

/// Polls until the machine snapshot matches the pre-restart truth again:
/// same active state, same public ip, same observed container count.
async fn wait_for_matching_snapshot(
    core: &CoreContext,
    machine: &MachineId,
    before: &MachineSnapshot,
) {
    wait_for_inspect(
        core,
        machine,
        Duration::from_secs(120),
        &format!("state never matched the pre-restart truth (expected {before:?})"),
        |snapshot| {
            let MachineSnapshot {
                active,
                public_ip,
                gateway,
                observed_container_count,
            } = snapshot;
            *active == before.active
                && *public_ip == before.public_ip
                && gateway.is_some()
                && *observed_container_count == before.observed_container_count
        },
    )
    .await;
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
    assert!(
        rejected.is_err(),
        "a seed outside the authorized user set connected"
    );

    // (b) A plaintext client cannot complete a handshake with the TLS port.
    let plaintext_url = NatsClientUrl::try_new(format!("nats://{}", cluster.core().published.nats))
        .expect("valid plaintext url");
    let plaintext = connect_with_timeout(&plaintext_url, CONNECT_TIMEOUT).await;
    assert!(
        plaintext.is_err(),
        "a plaintext client reached the TLS-only port"
    );

    // (c) The edge machine's minted seed is fenced to its own scope: publishing
    // into the core machine's service scope and writing core KV subjects both
    // draw server-side permission violations.
    let edge_seed =
        read_file_from_container(&core.docker, &edge.container_id, EDGE_NATS_CREDS_FILE)
            .await
            .expect("edge nats.creds is readable");
    let edge_machine_config = host_client_config(
        cluster,
        &core.material,
        NatsPrincipal::Machine {
            machine_id: machine_id("edge_2"),
        },
        edge_seed.trim(),
    );
    let (edge_client, mut edge_events) = connect_with_event_capture(&edge_machine_config).await;
    for subject in [
        machine_service(&machine_id("core_1"), MachineServiceEndpoint::Inspect),
        format!("$KV.{KV_CORE_BUCKET}.machines.active.core_1"),
    ] {
        edge_client
            .publish(subject.clone(), "evidence".into())
            .await
            .unwrap_or_else(|error| {
                panic!("publish to {subject} was not accepted client-side: {error}")
            });
        edge_client.flush().await.expect("edge client flush");
        match next_permission_violation(&mut edge_events).await {
            Some(violation) if violation.contains("Publish") => {}
            other => panic!("expected a publish violation for {subject}, got {other:?}"),
        }
    }

    // (d) The cluster's Join seed cannot sniff inboxes: the shared legacy
    // inbox scope and the core machine's own prefix are both refused.
    let join_config = host_client_config(
        cluster,
        &core.material,
        NatsPrincipal::Join,
        &core.material.join_seed,
    );
    let (join_client, mut join_events) = connect_with_event_capture(&join_config).await;
    let core_machine_inbox = inbox_subscribe_scope(&NatsPrincipal::Machine {
        machine_id: machine_id("core_1"),
    });
    for scope in ["_INBOX.>", core_machine_inbox.as_str()] {
        join_client
            .subscribe(scope.to_owned())
            .await
            .unwrap_or_else(|error| {
                panic!("subscribe {scope} was not accepted client-side: {error}")
            });
        join_client.flush().await.expect("join client flush");
        match next_permission_violation(&mut join_events).await {
            Some(violation) if violation.contains("Subscription") => {}
            other => panic!("expected a subscription violation for {scope}, got {other:?}"),
        }
    }

    // The cluster shrugged all of it off: both gateways still serve and the
    // control API still answers.
    for machine in [cluster.core(), edge] {
        assert_gateway_serves(machine).await;
    }
    core.api
        .machine_list(&MachineListRequest {})
        .await
        .expect("control API healthy after rejected clients");
}

// ---------------------------------------------------------------------------
// Scenario 1 detail assertions
// ---------------------------------------------------------------------------

/// Bootstrap evidence on the secured server: the KV buckets and streams the
/// control plane runs on exist (read with the Controller credential, whose
/// profile carries `$JS.API.>`).
async fn assert_bootstrap_resources_exist(core: &CoreContext) {
    let client = connect_core_client(
        core,
        NatsPrincipal::Controller,
        &core.material.controller_seed,
    )
    .await
    .expect("controller principal connects");
    let jetstream = async_nats::jetstream::new(client);
    for bucket in ["KV_CORE", "KV_OPS", "KV_OBS"] {
        jetstream
            .get_key_value(bucket)
            .await
            .unwrap_or_else(|error| panic!("bootstrap KV bucket {bucket} missing: {error}"));
    }
    for stream in ["PLZ_OPS"] {
        jetstream
            .get_stream(stream)
            .await
            .unwrap_or_else(|error| panic!("bootstrap stream {stream} missing: {error}"));
    }
}

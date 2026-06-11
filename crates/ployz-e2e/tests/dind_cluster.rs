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

use ployz_core::ids::{NodeId, OperationId};
use ployz_core::machine::{MachineAddOperationState, MachineCredentialProvisioningStep};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::{
    EventSequence, OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationIdempotencyKey, OperationStatus,
};
use ployz_core::security::NatsPrincipal;
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
    NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust, connect_authenticated,
};
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    MachineAddAccepted, MachineAddGateway, MachineAddRequest, MachineInspectRequest,
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineListRequest, MachineName,
    OpsStatusRequest,
};
use ployzctl::commands::machine::MachineAddOutput;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Where keeper install leaves the cluster CA and the seeds on the core.
const NATS_MATERIAL_DIR: &str = "/var/lib/ployz/nats";
/// The ployzd-control-owned authority file (recovery evidence).
const AUTHORIZED_USERS_FILE: &str = "/etc/nats/authorized-users.conf";
/// Where the keeper join commit leaves the redeemed per-machine seed
/// (keeper state dir `/var/lib/ployz` + `join-material.d`).
const EDGE_NATS_CREDS_FILE: &str = "/var/lib/ployz/join-material.d/nats.creds";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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
        assert_unit_active(&core, unit).await;
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
    let node_pid_before = unit_main_pid(&core, node_unit).await;
    let gateway_pid_before = unit_main_pid(&core, gateway_unit).await;

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
        assert_unit_active(&core, unit).await;
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
    let node_pid_after = unit_main_pid(&core, node_unit).await;
    let gateway_pid_after = unit_main_pid(&core, gateway_unit).await;
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

async fn assert_unit_active(core: &CoreContext, unit: &str) {
    let outcome = exec_on(core, core.cluster.core(), &["systemctl", "is-active", unit]).await;
    if outcome.stdout.trim() != "active" {
        fail_with_evidence(
            &core.cluster,
            &format!("unit {unit} is not active: {outcome:?}"),
        )
        .await;
    }
}

async fn unit_main_pid(core: &CoreContext, unit: &str) -> String {
    let outcome = exec_on(
        core,
        core.cluster.core(),
        &["systemctl", "show", "-p", "MainPID", "--value", unit],
    )
    .await;
    let pid = outcome.stdout.trim().to_owned();
    if !outcome.success() || pid.is_empty() || pid == "0" {
        fail_with_evidence(
            &core.cluster,
            &format!("unit {unit} has no main pid: {outcome:?}"),
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

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn event_replay_limit(value: u16) -> OperationEventReplayLimit {
    OperationEventReplayLimit::try_new(value).expect("valid event replay limit")
}

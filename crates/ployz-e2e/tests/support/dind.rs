//! Shared plumbing for the gated Docker-in-Docker cluster scenarios:
//! core-cluster formation (the `scripts/local-dataplane-proof.sh` recipe,
//! host-driven), the edge join flow, evidence capture, and the
//! assertion/polling helpers the scenario bodies share.

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use futures_util::FutureExt as _;
use ployz_core::ids::{NodeId, OperationId};
use ployz_core::machine::{MachineAddOperationState, MachineCredentialProvisioningStep};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::{
    DeployCompletionOutcome, DeployRunningStage, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayPage, OperationEventReplayRequest, OperationStatus,
};
use ployz_core::security::NatsPrincipal;
use ployz_e2e::bollard::Docker;
use ployz_e2e::dind::{
    self, ARTIFACTS_MOUNT_PATH, DindCluster, DindClusterSpec, DindMachine, DindMachineRole,
    ExecOutcome, MACHINE_NATS_PORT, MachineSpec, exec_in_container, read_file_from_container,
    shell_quote, write_file_in_container,
};
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsConnectError, NatsTlsTrust,
    authenticated_connect_options, connect_authenticated,
};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_sdk_types::{
    MachineAddAccepted, MachineAddGateway, MachineAddRequest, MachineInspectRequest,
    MachineListRequest, MachineName, MachineSnapshot, OpsStatusRequest,
};
use ployzctl::commands::machine::MachineAddOutput;
use ployzd::docker::labels::MANAGED_LABEL;

use super::ids::{event_replay_limit, event_sequence, idempotency_key, node_id, operation_id};

/// Where keeper install leaves the cluster CA and the seeds on the core.
pub const NATS_MATERIAL_DIR: &str = "/var/lib/ployz/nats";
/// The ployzd-control-owned authority file (recovery evidence).
pub const AUTHORIZED_USERS_FILE: &str = "/etc/nats/authorized-users.conf";
/// Where the keeper join commit leaves the redeemed per-machine seed
/// (keeper state dir `/var/lib/ployz` + `join-material.d`).
pub const EDGE_NATS_CREDS_FILE: &str = "/var/lib/ployz/join-material.d/nats.creds";
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-request budget for HTTP probes against a published gateway port.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Budget for one server-side permission violation to arrive on the client
/// event channel.
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Evidence capture
// ---------------------------------------------------------------------------

/// Runs a scenario body and, when any assertion inside it panics, captures
/// whole-cluster evidence before resuming the panic. This makes every plain
/// `assert!`/`panic!`/`.expect` in the body an evidence-capturing failure.
pub async fn with_evidence<T>(cluster: &DindCluster, scenario: impl Future<Output = T>) -> T {
    match AssertUnwindSafe(scenario).catch_unwind().await {
        Ok(value) => value,
        Err(panic) => {
            match cluster.capture_evidence().await {
                Ok(dir) => eprintln!("scenario failed; evidence: {}", dir.display()),
                Err(error) => eprintln!("scenario failed; evidence capture also failed: {error}"),
            }
            std::panic::resume_unwind(panic)
        }
    }
}

// ---------------------------------------------------------------------------
// Core-cluster formation (the proof-script recipe, host-driven)
// ---------------------------------------------------------------------------

/// One formed core: provisioned machines, keeper-installed secured NATS +
/// ployzd units on the core, the cluster material copied to the host, and an
/// authenticated host-side operator API client through the published port.
pub struct CoreContext {
    pub cluster: DindCluster,
    pub docker: Docker,
    pub material: ClusterMaterial,
    pub api: OperationApiClient,
}

impl CoreContext {
    /// The core machine's bridge IP (the address join bundles point at).
    #[must_use]
    pub fn core_ip(&self) -> IpAddr {
        self.cluster.core().bridge_ip
    }

    /// Execs a command inside one machine; panics on transport failure (the
    /// command's own exit code is the caller's to assert).
    pub async fn exec_on(&self, machine: &DindMachine, command: &[&str]) -> ExecOutcome {
        exec_in_container(&self.docker, &machine.container_id, command)
            .await
            .unwrap_or_else(|error| panic!("exec {command:?} on {} failed: {error}", machine.name))
    }

    /// Execs a shell script inside one machine.
    pub async fn exec_sh(&self, machine: &DindMachine, script: &str) -> ExecOutcome {
        self.exec_on(machine, &["sh", "-c", script]).await
    }
}

/// Cluster material copied out of the core container for host-side clients.
pub struct ClusterMaterial {
    /// Owns the on-host material files for the lifetime of the scenario.
    _dir: tempfile::TempDir,
    pub ca_file: PathBuf,
    pub ca_pem: String,
    pub operator_seed: String,
    pub controller_seed: String,
    pub join_seed: String,
}

/// Provisions the machines and forms the secured core exactly the way the
/// proof script does: placeholder-CA join template → keeper install via
/// `ployzctl init --run-keeper-install` → re-render the template with the
/// keeper-minted CA and restart the (disposable) control role → wait for
/// the control API over the published TLS port. Formation failures capture
/// whole-cluster evidence.
pub async fn install_core_cluster(docker: &Docker, edge_count: usize) -> CoreContext {
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

    let (material, api) = with_evidence(&cluster, form_core(docker, &cluster)).await;
    CoreContext {
        docker: docker.clone(),
        material,
        api,
        cluster,
    }
}

/// The formation work between provisioning and the ready operator API.
async fn form_core(
    docker: &Docker,
    cluster: &DindCluster,
) -> (ClusterMaterial, OperationApiClient) {
    let core = cluster.core().clone();
    let core_ip = core.bridge_ip;
    let core_nats_url = format!("tls://{core_ip}:{MACHINE_NATS_PORT}");

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
        assert!(mounted.success(), "bpf mount failed: {mounted:?}");
    }

    let shas = ArtifactShas::read(docker, &core).await;

    // Join template first carries a syntactically valid placeholder CA: it
    // must parse when ployzd-control first starts, before the cluster CA
    // exists. It is replaced with the real CA right after keeper install.
    let placeholder_ca =
        "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n".to_owned();
    write_join_template(docker, &core, &core_nats_url, &placeholder_ca, &shas).await;

    let install_spec = serde_json::json!({
        "node_id": "core_1",
        "gateway": "install",
        "node_public_ip": core_ip.to_string(),
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
    assert!(
        init.success(),
        "keeper first-node install failed (exit {}): {}\n{}",
        init.exit_code,
        init.stdout,
        init.stderr
    );

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
    write_join_template(docker, &core, &core_nats_url, &ca_pem, &shas).await;
    let restart = exec_in_container(
        docker,
        &core.container_id,
        &["systemctl", "restart", "ployzd-control"],
    )
    .await
    .expect("restart ployzd-control");
    assert!(restart.success(), "control restart failed: {restart:?}");

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
    let api = wait_for_operator_api(cluster, &material).await;
    (material, api)
}

/// Pinned artifact digests computed inside the core machine, with the
/// in-machine source/install paths the keeper consumes.
pub struct ArtifactShas {
    pub ployzd: String,
    pub ebpf_bytecode: String,
    pub ebpf_ctl: String,
    pub nats_server: String,
}

impl ArtifactShas {
    pub async fn read(docker: &Docker, core: &DindMachine) -> Self {
        Self {
            ployzd: sha256_of(docker, core, &format!("{ARTIFACTS_MOUNT_PATH}/ployzd")).await,
            ebpf_bytecode: sha256_of(
                docker,
                core,
                &format!("{ARTIFACTS_MOUNT_PATH}/ployz-ebpf-tc"),
            )
            .await,
            ebpf_ctl: sha256_of(
                docker,
                core,
                &format!("{ARTIFACTS_MOUNT_PATH}/ployz-ebpf-ctl"),
            )
            .await,
            nats_server: sha256_of(docker, core, "/usr/local/bin/nats-server").await,
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

pub async fn sha256_of(docker: &Docker, machine: &DindMachine, path: &str) -> String {
    let outcome = exec_in_container(docker, &machine.container_id, &["sha256sum", path])
        .await
        .expect("exec sha256sum");
    assert!(outcome.success(), "sha256sum {path} failed: {outcome:?}");
    let Some(digest) = outcome.stdout.split_whitespace().next() else {
        panic!("empty sha256sum output for {path}");
    };
    digest.to_owned()
}

async fn write_join_template(
    docker: &Docker,
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
    write_file_in_container(
        docker,
        &core.container_id,
        "/etc/ployz/machine-join-template.json",
        &serde_json::to_string_pretty(&template).expect("join template serializes"),
        "0644",
    )
    .await
    .expect("write join template");
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
    panic!("control API did not become ready: {last_error}")
}

#[must_use]
pub fn host_client_config(
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

pub async fn connect_core_client(
    core: &CoreContext,
    principal: NatsPrincipal,
    seed: &str,
) -> Result<async_nats::Client, NatsConnectError> {
    let config = host_client_config(&core.cluster, &core.material, principal, seed);
    connect_authenticated(&config, CONNECT_TIMEOUT).await
}

/// Activates the first node through the in-machine product CLI,
/// authenticated with the keeper-minted operator credential, and returns the
/// activation operation id.
pub async fn activate_first_node(core: &CoreContext) -> OperationId {
    let activate = core
        .exec_sh(
            core.cluster.core(),
            &format!(
                "PLOYZ_NATS_CA_FILE={NATS_MATERIAL_DIR}/ca.pem \
                 PLOYZ_NATS_NKEY_SEED_FILE={NATS_MATERIAL_DIR}/operator.seed \
                 {ARTIFACTS_MOUNT_PATH}/ployzctl --nats tls://127.0.0.1:{MACHINE_NATS_PORT} \
                 init activate-first-node --node core_1 --gateway"
            ),
        )
        .await;
    assert!(
        activate.success(),
        "activate-first-node failed (exit {}): {}\n{}",
        activate.exit_code,
        activate.stdout,
        activate.stderr
    );
    let Some(operation_id) = parse_operation_line(&activate.stdout) else {
        panic!("no operation id in activate output: {}", activate.stdout);
    };
    operation_id
}

/// Submits the canonical edge machine add (`edge_2`) and returns the
/// acceptance the product printed the join bundle from.
pub async fn submit_machine_add(core: &CoreContext) -> MachineAddAccepted {
    core.api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id("op_add_edge_2"),
            idempotency_key: idempotency_key("idem_add_edge_2"),
            node_id: node_id("edge_2"),
            name: machine_name("edge-2"),
            gateway: MachineAddGateway::Install,
        })
        .await
        .expect("machine add submits")
}

/// Adds the edge machine through the host-side API and runs the
/// `scripts/ployz.sh` join flow on it; scenarios 3–5 use this as plumbing
/// (scenario 2 owns the detailed join assertions).
pub async fn add_and_join_edge(core: &CoreContext, edge: &DindMachine) {
    let accepted = submit_machine_add(core).await;
    let add_operation = accepted.accepted.operation_id.clone();
    let install = parse_install_line(core, accepted);
    run_edge_join(core, edge, &install).await;
    let status = operation_status(core, &add_operation).await;
    assert!(
        matches!(
            &status,
            OperationStatus::MachineAdd {
                state: MachineAddOperationState::Completed,
                ..
            }
        ),
        "machine add did not complete: {status:?}"
    );
}

// ---------------------------------------------------------------------------
// Edge join (scripts/ployz.sh flow with the printed install material)
// ---------------------------------------------------------------------------

/// The env the product prints into the machine-add install command line.
#[derive(Debug)]
pub struct InstallLine {
    pub nats_url: String,
    pub nats_ca_b64: String,
    pub join_seed: String,
    pub join_token: String,
}

/// Renders the product's machine-add output (the same text `ployzctl
/// machine add` prints) and parses the install env + join token out of it.
#[must_use]
pub fn parse_install_line(core: &CoreContext, accepted: MachineAddAccepted) -> InstallLine {
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
pub async fn run_edge_join(core: &CoreContext, edge: &DindMachine, install: &InstallLine) {
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
        edge,
        &format!("{ARTIFACTS_MOUNT_PATH}/ployz-keeper"),
    )
    .await;
    let join = core
        .exec_sh(
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
    assert!(
        join.success(),
        "edge join failed (exit {}): {}\n{}",
        join.exit_code,
        join.stdout,
        join.stderr
    );
}

fn repo_path(relative: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

// ---------------------------------------------------------------------------
// Assertion and polling helpers
// ---------------------------------------------------------------------------

pub async fn assert_unit_active(core: &CoreContext, machine: &DindMachine, unit: &str) {
    let outcome = core
        .exec_on(machine, &["systemctl", "is-active", unit])
        .await;
    assert!(
        outcome.stdout.trim() == "active",
        "unit {unit} on {} is not active: {outcome:?}",
        machine.name
    );
}

pub async fn unit_main_pid(core: &CoreContext, machine: &DindMachine, unit: &str) -> String {
    let outcome = core
        .exec_on(
            machine,
            &["systemctl", "show", "-p", "MainPID", "--value", unit],
        )
        .await;
    let pid = outcome.stdout.trim().to_owned();
    assert!(
        outcome.success() && !pid.is_empty() && pid != "0",
        "unit {unit} on {} has no main pid: {outcome:?}",
        machine.name
    );
    pid
}

/// Polls the units' journal on the core machine until the marker shows up
/// (journald may lag the unit's stderr by a moment).
pub async fn assert_journal_contains(core: &CoreContext, units: &[&str], marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = String::new();
    while Instant::now() < deadline {
        let mut command = vec!["journalctl", "--no-pager"];
        for unit in units {
            command.push("-u");
            command.push(unit);
        }
        let outcome = core.exec_on(core.cluster.core(), &command).await;
        if outcome.stdout.contains(marker) {
            return;
        }
        last = outcome.stdout;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("journal of {units:?} never contained {marker:?}: {last}")
}

pub async fn operation_status(core: &CoreContext, operation_id: &OperationId) -> OperationStatus {
    core.api
        .ops_status(&OpsStatusRequest {
            operation_id: operation_id.clone(),
        })
        .await
        .unwrap_or_else(|error| panic!("ops status {operation_id:?} failed: {error}"))
        .status
}

/// Replays the operation's event page from the first sequence.
pub async fn operation_event_page(
    core: &CoreContext,
    operation_id: &OperationId,
) -> OperationEventReplayPage {
    core.api
        .ops_watch(&OperationEventReplayRequest {
            operation_id: operation_id.clone(),
            start_sequence: event_sequence(1),
            limit: event_replay_limit(64),
        })
        .await
        .unwrap_or_else(|error| panic!("ops watch {operation_id:?} failed: {error}"))
}

pub async fn operation_events(
    core: &CoreContext,
    operation_id: &OperationId,
) -> Vec<OperationEvent> {
    operation_event_page(core, operation_id)
        .await
        .events
        .into_iter()
        .map(|event| event.event)
        .collect()
}

/// Replays the full event history of a terminal operation.
pub async fn terminal_operation_events(
    core: &CoreContext,
    operation_id: &OperationId,
) -> Vec<OperationEvent> {
    let page = operation_event_page(core, operation_id).await;
    assert!(
        page.cursor == OperationEventReplayCursor::Terminal,
        "operation {operation_id:?} replay is not terminal: {page:?}"
    );
    page.events.into_iter().map(|event| event.event).collect()
}

/// One named step of an expected event sequence.
pub type LabeledEventPredicate<'a> = (&'static str, Box<dyn Fn(&OperationEvent) -> bool + 'a>);

/// Resolves each labeled step to its event index and asserts the steps
/// appear in order; the panic message names the missing or misordered step.
/// Returns the resolved indices for window checks on top of the order.
pub fn assert_events_in_order(
    what: &str,
    events: &[OperationEvent],
    steps: Vec<LabeledEventPredicate<'_>>,
) -> Vec<usize> {
    let mut resolved: Vec<(&'static str, usize)> = Vec::with_capacity(steps.len());
    for (label, predicate) in &steps {
        let Some(index) = events.iter().position(predicate) else {
            panic!("{what}: missing event `{label}`: {events:?}");
        };
        resolved.push((label, index));
    }
    for ((earlier_label, earlier), (later_label, later)) in
        resolved.iter().zip(resolved.iter().skip(1))
    {
        assert!(
            earlier <= later,
            "{what}: event `{later_label}` (index {later}) arrived before \
             `{earlier_label}` (index {earlier}): {events:?}"
        );
    }
    resolved.into_iter().map(|(_, index)| index).collect()
}

/// The committed machine-add event vocabulary: submitted, then the five
/// mint steps in order, then joined, then completed — with acceptance
/// strictly before the reload.
pub fn assert_machine_add_event_sequence(events: &[OperationEvent], expected_node: &NodeId) {
    let mut steps: Vec<LabeledEventPredicate<'_>> = vec![(
        "submitted",
        Box::new(move |event| {
            matches!(
                event,
                OperationEvent::MachineAddSubmitted { node_id, .. } if node_id == expected_node
            )
        }),
    )];
    for (label, expected_step) in [
        (
            "credential-minted",
            MachineCredentialProvisioningStep::Minted,
        ),
        (
            "credential-rendered",
            MachineCredentialProvisioningStep::Rendered,
        ),
        (
            "credential-reloaded",
            MachineCredentialProvisioningStep::Reloaded,
        ),
        (
            "credential-verified",
            MachineCredentialProvisioningStep::Verified,
        ),
        (
            "credential-material-ready",
            MachineCredentialProvisioningStep::MaterialReady,
        ),
    ] {
        steps.push((
            label,
            Box::new(move |event| {
                matches!(
                    event,
                    OperationEvent::MachineAddCredentialProvisioned { step, node_id, .. }
                        if *step == expected_step && node_id == expected_node
                )
            }),
        ));
    }
    steps.push((
        "joined",
        Box::new(move |event| {
            matches!(
                event,
                OperationEvent::MachineAddJoined { node_id, .. } if node_id == expected_node
            )
        }),
    ));
    steps.push((
        "completed",
        Box::new(move |event| {
            matches!(
                event,
                OperationEvent::MachineAddCompleted { node_id, .. } if node_id == expected_node
            )
        }),
    ));
    assert_events_in_order(&format!("machine add for {expected_node:?}"), events, steps);
}

/// Polls the operation status through the host-side API until the deploy is
/// terminal, within the budget.
pub async fn wait_for_terminal_deploy_status(
    core: &CoreContext,
    operation_id: &OperationId,
    budget: Duration,
) -> OperationStatus {
    let deadline = Instant::now() + budget;
    loop {
        let status = operation_status(core, operation_id).await;
        let OperationStatus::Deploy { state, .. } = &status else {
            panic!("operation {operation_id:?} is not a deploy: {status:?}");
        };
        if state.is_terminal() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "deploy {operation_id:?} not terminal in budget: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The committed deploy event vocabulary (the `operations.rs` sequence) for
/// the two-machine routed deploy: submitted → planning → plan →
/// WireGuard/eBPF preparation over both nodes → container starts on both
/// nodes → health → commit → completed, in order.
pub fn assert_deploy_event_sequence(events: &[OperationEvent], deploy_operation: &OperationId) {
    let steps: Vec<LabeledEventPredicate<'_>> = vec![
        (
            "submitted",
            Box::new(move |event| {
                matches!(
                    event,
                    OperationEvent::DeploySubmitted { operation_id, .. }
                        if operation_id == deploy_operation
                )
            }),
        ),
        (
            "planning-started",
            Box::new(|event| matches!(event, OperationEvent::DeployPlanningStarted { .. })),
        ),
        (
            "plan-created",
            Box::new(|event| matches!(event, OperationEvent::DeployPlanCreated { .. })),
        ),
        (
            "running:preparing-wireguard-ebpf",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployRunning {
                        stage: DeployRunningStage::PreparingWireGuardEbpf,
                        ..
                    }
                )
            }),
        ),
        (
            "wireguard-ebpf-prepared-on-both-nodes",
            Box::new(|event| {
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
            }),
        ),
        (
            "running:starting-containers",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployRunning {
                        stage: DeployRunningStage::StartingContainers,
                        ..
                    }
                )
            }),
        ),
        (
            "running:waiting-for-health",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployRunning {
                        stage: DeployRunningStage::WaitingForHealth,
                        ..
                    }
                )
            }),
        ),
        (
            "health-check-started",
            Box::new(|event| matches!(event, OperationEvent::DeployHealthCheckStarted { .. })),
        ),
        (
            "running:active-service-commit",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployRunning {
                        stage: DeployRunningStage::ActiveServiceCommit,
                        ..
                    }
                )
            }),
        ),
        (
            "completed",
            Box::new(|event| {
                matches!(
                    event,
                    OperationEvent::DeployCompleted {
                        outcome: DeployCompletionOutcome::Completed,
                        ..
                    }
                )
            }),
        ),
    ];
    let resolved = assert_events_in_order(&format!("deploy {deploy_operation:?}"), events, steps);

    // One container start per machine, both inside the StartingContainers →
    // WaitingForHealth window (their relative order is placement-dependent).
    let [.., starting_index, waiting_index, _, _, _] = resolved.as_slice() else {
        unreachable!("resolved has ten entries");
    };
    for expected_node in [node_id("core_1"), node_id("edge_2")] {
        let started = events.iter().position(|event| {
            matches!(
                event,
                OperationEvent::DeployContainerStarted { node_id, .. }
                    if *node_id == expected_node
            )
        });
        let Some(started) = started else {
            panic!("no container start on {expected_node:?}: {events:?}");
        };
        assert!(
            started >= *starting_index && started <= *waiting_index,
            "container start on {expected_node:?} outside the starting window: {events:?}"
        );
    }
}

/// One plain HTTP GET against a published gateway port with the route's
/// host header; the error carries enough context for evidence.
pub async fn gateway_http_get(addr: SocketAddr, host: &str) -> Result<String, String> {
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

/// Connects with the exact product option set plus an event capture channel
/// so the test can observe server-side permission violations.
pub async fn connect_with_event_capture(
    config: &NatsConnectConfig,
) -> (
    async_nats::Client,
    tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) {
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = authenticated_connect_options(config)
        .event_callback(move |event| {
            let events_tx = events_tx.clone();
            async move {
                events_tx.send(event).ok();
            }
        })
        .connect(config.url.as_str())
        .await
        .expect("authenticated connect with event capture");
    (client, events_rx)
}

/// Waits for the next server-side permission violation on the event
/// channel; `None` when none arrives in budget.
pub async fn next_permission_violation(
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

/// Polls `machine inspect` until the predicate holds, returning the matching
/// snapshot; panics with `what` and the last observation on budget overrun.
pub async fn wait_for_inspect(
    core: &CoreContext,
    node: &NodeId,
    budget: Duration,
    what: &str,
    predicate: impl Fn(&MachineSnapshot) -> bool,
) -> MachineSnapshot {
    let deadline = Instant::now() + budget;
    let mut last = String::from("<no inspect yet>");
    while Instant::now() < deadline {
        match core
            .api
            .machine_inspect(&MachineInspectRequest {
                node_id: node.clone(),
            })
            .await
        {
            Ok(snapshot) => {
                if predicate(&snapshot) {
                    return snapshot;
                }
                last = format!("{snapshot:?}");
            }
            Err(error) => last = format!("{error}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("machine {node:?} {what} within {budget:?}: {last}")
}

/// Waits until the machine snapshot carries node observations (public ip
/// from the node process and gateway status from the gateway process) —
/// proof both processes connected with the machine's Node credential.
pub async fn wait_for_machine_observations(core: &CoreContext, machine: &NodeId) {
    wait_for_inspect(
        core,
        machine,
        Duration::from_secs(120),
        "never published observations",
        |snapshot| snapshot.public_ip.is_some() && snapshot.gateway.is_some(),
    )
    .await;
}

/// One managed workload container as the inner Docker daemon reports it.
#[derive(Debug)]
pub struct ManagedWorkloadContainer {
    pub id: String,
    pub labels: HashMap<String, String>,
}

/// Lists the running managed workload containers inside one machine's inner
/// Docker daemon (the product's `plz.managed` label schema), with their
/// exact label maps from `docker inspect`.
pub async fn managed_workload_containers(
    core: &CoreContext,
    machine: &DindMachine,
) -> Vec<ManagedWorkloadContainer> {
    let filter = format!("label={MANAGED_LABEL}=true");
    let listed = core
        .exec_on(
            machine,
            &["docker", "ps", "--no-trunc", "--quiet", "--filter", &filter],
        )
        .await;
    assert!(
        listed.success(),
        "inner docker ps on {} failed: {listed:?}",
        machine.name
    );
    let ids: Vec<&str> = listed
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if ids.is_empty() {
        return Vec::new();
    }

    let mut command = vec![
        "docker",
        "inspect",
        "--format",
        "{{.Id}}\t{{json .Config.Labels}}",
    ];
    command.extend(ids);
    let inspected = core.exec_on(machine, &command).await;
    assert!(
        inspected.success(),
        "inner docker inspect on {} failed: {inspected:?}",
        machine.name
    );
    inspected
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let Some((id, labels_json)) = line.split_once('\t') else {
                panic!(
                    "docker inspect line on {} has no id/labels separator: {line}",
                    machine.name
                );
            };
            let labels: HashMap<String, String> =
                serde_json::from_str(labels_json).unwrap_or_else(|error| {
                    panic!(
                        "docker inspect labels on {} are not a JSON object ({error}): {line}",
                        machine.name
                    )
                });
            ManagedWorkloadContainer {
                id: id.to_owned(),
                labels,
            }
        })
        .collect()
}

#[must_use]
pub fn decode_base64(value: &str) -> String {
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

fn machine_name(value: &str) -> MachineName {
    MachineName::try_new(value).expect("valid machine name")
}

/// Tears the cluster down unless `PLOYZ_DIND_KEEP=1`.
pub async fn finish(core: CoreContext) {
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

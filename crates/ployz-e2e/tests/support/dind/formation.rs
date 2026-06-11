//! Core-cluster formation: the proof-script recipe, host-driven — plus the
//! cluster lifecycle (teardown) and host-side client plumbing.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ployz_core::ids::OperationId;
use ployz_core::machine::MachineAddOperationState;
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::OperationStatus;
use ployz_core::security::NatsPrincipal;
use ployz_e2e::bollard::Docker;
use ployz_e2e::dind::{
    self, ARTIFACTS_MOUNT_PATH, DindCluster, DindClusterSpec, DindMachine, DindMachineRole,
    ExecOutcome, MACHINE_NATS_PORT, MachineSpec, exec_in_container, read_file_from_container,
    write_file_in_container,
};
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsConnectError, NatsTlsTrust,
    connect_authenticated,
};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_sdk_types::{
    MachineAddAccepted, MachineAddGateway, MachineAddRequest, MachineListRequest, MachineName,
};

use super::super::ids::{idempotency_key, node_id, operation_id};
use super::assert::operation_status;
use super::join::{parse_install_line, run_edge_join};
use super::{CONNECT_TIMEOUT, NATS_MATERIAL_DIR, with_evidence};

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

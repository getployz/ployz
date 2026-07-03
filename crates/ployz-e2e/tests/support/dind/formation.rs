//! Core-cluster formation, cluster lifecycle, and host-side client plumbing.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ployz_core::install::{MachineBootstrapUrl, MachineJoinClusterName};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::roles::InstallRolePolicy;
use ployz_core::security::NatsPrincipal;
use ployz_e2e::bollard::Docker;
use ployz_e2e::dind::{
    self, ARTIFACTS_MOUNT_PATH, DindCluster, DindClusterSpec, DindMachine, DindMachineRole,
    ExecOutcome, MachineSpec, exec_in_container, read_file_from_container, write_file_in_container,
};
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsConnectError, NatsTlsTrust,
    connect_authenticated,
};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_sdk_types::{MachineAddAccepted, MachineAddRequest, MachineListRequest};
use ployzctl::bootstrap_command::BootstrapRelease;
use ployzctl::commands::PloyzctlCommand;
use ployzctl::commands::machine::{MachineAddRemoteCommand, MachineIdentity, MachineInitCommand};
use ployzctl::runtime::{PloyzctlRuntimeConfig, execute_command};
use ployzctl::ssh::SshTarget;

use super::join::{INSTALLER_WRAPPER_PATH, write_installer_wrapper};
use super::{CONNECT_TIMEOUT, NATS_MATERIAL_DIR, with_evidence};
use ployz_test_support::fs::make_executable;
use ployz_test_support::ids::machine_name;
use ployz_test_support::ids::{idempotency_key, machine_id, operation_id};

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
    /// File forms of the seeds for product CLI runs from the host.
    pub operator_seed_file: PathBuf,
    pub join_seed_file: PathBuf,
}

/// Provisions the machines and forms the secured core through the product
/// `ployzctl machine init root@core` command, driven in-process from the
/// host over a docker-exec-backed stand-in `ssh`. Includes first-machine
/// activation, so callers must not activate again.
pub async fn init_core_cluster(docker: &Docker, edge_count: usize) -> CoreContext {
    let cluster = provision_cluster(docker, edge_count).await;
    let (material, api) = with_evidence(&cluster, product_init_core(docker, &cluster)).await;
    CoreContext {
        docker: docker.clone(),
        material,
        api,
        cluster,
    }
}

async fn provision_cluster(docker: &Docker, edge_count: usize) -> DindCluster {
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
    DindCluster::provision(
        docker,
        DindClusterSpec {
            artifact_dir: dind::artifact_dir(),
            machines,
        },
    )
    .await
    .expect("provision DinD cluster")
}

/// The dataplane mounts bpffs on every machine (proof-script recipe).
async fn mount_bpf(docker: &Docker, cluster: &DindCluster) {
    for machine in std::iter::once(cluster.core()).chain(cluster.edges()) {
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
}

/// Copies the host-side material out of the core container (CA plus the
/// three install-time seeds), both as strings and as host files.
async fn collect_cluster_material(docker: &Docker, core: &DindMachine) -> ClusterMaterial {
    let dir = tempfile::TempDir::new().expect("create host material dir");
    let ca_pem = read_file_from_container(
        docker,
        &core.container_id,
        &format!("{NATS_MATERIAL_DIR}/ca.pem"),
    )
    .await
    .expect("read cluster CA");
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
    let operator_seed_file = dir.path().join("operator.seed");
    std::fs::write(&operator_seed_file, &operator_seed).expect("write host operator seed");
    let join_seed_file = dir.path().join("join.seed");
    std::fs::write(&join_seed_file, &join_seed).expect("write host join seed");
    ClusterMaterial {
        _dir: dir,
        ca_file,
        ca_pem,
        operator_seed,
        controller_seed,
        join_seed,
        operator_seed_file,
        join_seed_file,
    }
}

// ---------------------------------------------------------------------------
// Product quick-start path (machine init / machine add over stand-in ssh)
// ---------------------------------------------------------------------------

/// Where the harness writes the local release manifest inside a machine.
const RELEASE_MANIFEST_PATH: &str = "/tmp/ployz-release.env";

/// Host-side plumbing for one product CLI run against one machine: a
/// docker-exec-backed stand-in `ssh` plus an isolated cluster context path.
struct HostCliHarness {
    _dir: tempfile::TempDir,
    ssh_program: PathBuf,
    context_path: PathBuf,
}

impl HostCliHarness {
    fn new(machine: &DindMachine) -> Self {
        let dir = tempfile::TempDir::new().expect("create host cli harness dir");
        let ssh_program = dir.path().join("fake-ssh");
        // The product runs `ssh -o BatchMode=yes -o ConnectTimeout=10
        // <dest> <command>`; the stand-in routes the command into the
        // machine container.
        let script = format!(
            "#!/bin/sh\nexec docker exec -i {} sh -c \"$6\"\n",
            machine.container_id
        );
        std::fs::write(&ssh_program, script).expect("write stand-in ssh");
        make_executable(&ssh_program);
        Self {
            context_path: dir.path().join("ployz").join("context.json"),
            ssh_program,
            _dir: dir,
        }
    }

    /// Runtime config for the product CLI: published NATS port (bridge IPs
    /// are not reachable from every host), stand-in ssh, isolated context.
    /// `material` carries the credential files once the cluster exists.
    fn runtime_config(
        &self,
        cluster: &DindCluster,
        material: Option<&ClusterMaterial>,
    ) -> PloyzctlRuntimeConfig {
        let published = cluster.core().published.nats;
        PloyzctlRuntimeConfig {
            nats_url: Some(format!("tls://{published}")),
            nats_ca_file: material.map(|material| material.ca_file.clone()),
            nats_seed_file: material.map(|material| material.operator_seed_file.clone()),
            join_seed_file: material.map(|material| material.join_seed_file.clone()),
            nats_connect_timeout: None,
            keeper_install_timeout: None,
            ops_watch_timeout: None,
            ops_watch_poll_interval: None,
            ssh_program: Some(self.ssh_program.clone()),
            ssh_install_timeout: None,
            cluster_context_path: Some(self.context_path.clone()),
        }
    }
}

/// Writes the release manifest the product `machine init` resolves artifacts
/// from: the baked artifact mount as absolute-path sources with pinned shas.
async fn write_release_manifest(docker: &Docker, core: &DindMachine, shas: &ArtifactShas) {
    let manifest = format!(
        "PLOYZ_VERSION=local\n\
         PLOYZD_URL={ARTIFACTS_MOUNT_PATH}/ployzd\n\
         PLOYZD_SHA256={}\n\
         PLOYZ_EBPF_TC_URL={ARTIFACTS_MOUNT_PATH}/ployz-ebpf-tc\n\
         PLOYZ_EBPF_TC_SHA256={}\n\
         PLOYZ_EBPF_CTL_URL={ARTIFACTS_MOUNT_PATH}/ployz-ebpf-ctl\n\
         PLOYZ_EBPF_CTL_SHA256={}\n",
        shas.ployzd, shas.ebpf_bytecode, shas.ebpf_ctl,
    );
    write_file_in_container(
        docker,
        &core.container_id,
        RELEASE_MANIFEST_PATH,
        &manifest,
        "0644",
    )
    .await
    .expect("write release manifest");
}

/// Forms the core through the product `ployzctl machine init` command: the
/// CLI builds the install spec internally, runs the wrapped installer in
/// first-machine mode over the stand-in ssh, writes the join template/CA
/// ordering itself, records a local cluster context, and activates the
/// first machine.
async fn product_init_core(
    docker: &Docker,
    cluster: &DindCluster,
) -> (ClusterMaterial, OperationApiClient) {
    let core = cluster.core().clone();
    mount_bpf(docker, cluster).await;

    let shas = ArtifactShas::read(docker, &core).await;
    write_release_manifest(docker, &core, &shas).await;
    write_installer_wrapper(docker, &core).await;

    let harness = HostCliHarness::new(&core);
    let config = harness.runtime_config(cluster, None);
    let command = MachineInitCommand {
        target: SshTarget::parse(&format!("root@{}", core.bridge_ip))
            .expect("core bridge ip parses as ssh target"),
        // Container hostnames are not stable identities; the scenarios pin
        // the documented core name through the product `--name` override.
        identity_override: Some(
            MachineIdentity::from_name_override("core_1").expect("core name is a valid identity"),
        ),
        roles: InstallRolePolicy::install_all(),
        release: BootstrapRelease::Version("local".to_owned()),
        release_manifest_url: Some(format!("file://{RELEASE_MANIFEST_PATH}")),
        bootstrap_url: MachineBootstrapUrl::try_new("https://local.invalid/ployz.sh")
            .expect("valid bootstrap url"),
        cluster_name: MachineJoinClusterName::try_new("dind-e2e").expect("valid cluster name"),
        installer_script: Some(INSTALLER_WRAPPER_PATH.to_owned()),
        detach: false,
    };
    let output = execute_command(PloyzctlCommand::MachineInit(command), &config)
        .await
        .unwrap_or_else(|error| panic!("product machine init failed: {error}"));
    assert!(
        output.stdout.contains("first-machine core_1 active"),
        "machine init output: {}",
        output.stdout
    );

    let material = collect_cluster_material(docker, &core).await;
    let api = wait_for_operator_api(cluster, &material).await;
    (material, api)
}

/// Pinned artifact digests computed inside the core machine, with the
/// in-machine source/install paths the keeper consumes.
pub struct ArtifactShas {
    pub ployzd: String,
    pub ebpf_bytecode: String,
    pub ebpf_ctl: String,
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
        }
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

/// Submits the canonical edge machine add (`edge_2`) and returns the
/// acceptance the product printed the join bundle from.
pub async fn submit_machine_add(core: &CoreContext) -> MachineAddAccepted {
    core.api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id("op_add_edge_2"),
            idempotency_key: idempotency_key("idem_add_edge_2"),
            machine_id: machine_id("edge_2"),
            name: machine_name("edge-2"),
            roles: InstallRolePolicy::install_all(),
        })
        .await
        .expect("machine add submits")
}

/// Adds the edge machine through the product `ployzctl machine add
/// root@edge` command (submit + remote installer join + watch to
/// completion); scenarios 3–5 use this as plumbing (scenario 2 owns the
/// detailed join assertions through the explicit low-level path).
pub async fn add_and_join_edge(core: &CoreContext, edge: &DindMachine) {
    write_installer_wrapper(&core.docker, edge).await;
    let harness = HostCliHarness::new(edge);
    let config = harness.runtime_config(&core.cluster, Some(&core.material));
    let command = MachineAddRemoteCommand {
        target: SshTarget::parse(&format!("root@{}", edge.bridge_ip))
            .expect("edge bridge ip parses as ssh target"),
        identity_override: Some(
            MachineIdentity::from_name_override("edge_2").expect("edge name is a valid identity"),
        ),
        roles: InstallRolePolicy::install_all(),
        installer_script: Some(INSTALLER_WRAPPER_PATH.to_owned()),
        detach: false,
    };
    let output = execute_command(PloyzctlCommand::MachineAddRemote(command), &config)
        .await
        .unwrap_or_else(|error| panic!("product machine add failed: {error}"));
    assert!(
        output.stdout.contains("machine-add completed"),
        "machine add output: {}",
        output.stdout
    );
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

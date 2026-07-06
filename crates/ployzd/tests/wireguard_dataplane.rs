use ployz_core::dataplane::{
    DataplanePrepareRequest, EbpfForwardingReadyEvidence, PloyzNativeMeshComponent,
    WireGuardEbpfEndpointRoute, WireGuardEbpfPrepareError, WireGuardPeer, WireGuardPublicKey,
    WireGuardReadyEvidence,
};
use ployz_test_support::ids::{machine_id, operation_id};
use ployzd::config::{DEFAULT_DATAPLANE_BRIDGE_IFNAME, DEFAULT_DATAPLANE_WG_IFNAME};
use ployzd::adapters::host_dataplane::{PloyzNativeMeshHostConfig, PloyzNativeMeshPreparer};
use ployzd::deploy_worker::{DataplanePreparer, MachineContainerRuntime};
use ployzd::adapters::docker::runner::DockerManagedContainerRunner;
use ployzd::machine_runtime::client::{NatsMachineContainerRuntime, NatsMachineDataplanePreparer};
use ployzd::machine_runtime::protocol::MachineEnsureEndpointNetworkRpcRequest;
use ployzd::machine_runtime::runner::MachineContainerRunner;
use ployzd::machine_runtime::service::MachinePloyzNativeMeshPreparer;
use ployzd::machine_runtime::service::start_machine_runtime_service;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

const LOCAL_PROOF_ENV: &str = "PLOYZ_LOCAL_DATAPLANE_PROOF";
const EBPF_CTL_ENV: &str = "PLOYZ_LOCAL_DATAPLANE_EBPF_CTL";
const EBPF_BYTECODE_ENV: &str = "PLOYZ_LOCAL_DATAPLANE_EBPF_BYTECODE";
const ENDPOINT_NETWORK_SUBNET: &str = "10.42.1.0/24";

#[tokio::test]
async fn local_privileged_docker_dataplane_prepares_wireguard_ebpf_and_routes() {
    if !local_proof_enabled() {
        return;
    }
    let _guard = local_dataplane_lock().lock().await;
    require_root();

    let _cleanup = DataplaneCleanup;
    cleanup_dataplane();
    let ebpf_ctl = required_path_env(EBPF_CTL_ENV);
    let ebpf_bytecode = required_path_env(EBPF_BYTECODE_ENV);

    let runner = DockerManagedContainerRunner::local_defaults(
        ENDPOINT_NETWORK_SUBNET,
        DEFAULT_DATAPLANE_BRIDGE_IFNAME,
    )
    .expect("connect to local Docker daemon");
    runner
        .ensure_endpoint_network()
        .await
        .expect("endpoint Docker network is created");
    runner
        .ensure_endpoint_network()
        .await
        .expect("endpoint Docker network creation is idempotent");
    command_ok(
        "ip",
        &["link", "show", "dev", DEFAULT_DATAPLANE_BRIDGE_IFNAME],
    );

    let peer_key = generated_wireguard_public_key();
    let preparer =
        PloyzNativeMeshPreparer::new(PloyzNativeMeshHostConfig::with_default_key_material(
            machine_id("core_1"),
            ebpf_bytecode.clone(),
            ebpf_ctl.clone(),
            DEFAULT_DATAPLANE_BRIDGE_IFNAME.to_owned(),
            DEFAULT_DATAPLANE_WG_IFNAME.to_owned(),
        ))
        .with_command_timeout(Duration::from_secs(20));

    let ready = preparer
        .prepare_ployz_native_mesh(&endpoint_routes(), &[edge_peer_with_public_key(peer_key)])
        .await
        .expect("host dataplane prepares through real commands");

    assert_wireguard_peer_evidence(&ready.wireguard.evidence);
    assert!(ready.ebpf_forwarding.evidence.iter().any(|evidence| {
        matches!(
            evidence,
            EbpfForwardingReadyEvidence::PloyzTcBytecode { path, .. }
                if path == &ebpf_bytecode.display().to_string()
        )
    }));
    assert_ebpf_attached_evidence(&ready.ebpf_forwarding.evidence, &ebpf_ctl);
    assert_edge_route_evidence(&ready.ebpf_forwarding.evidence, &ebpf_ctl);
}

#[tokio::test]
async fn local_privileged_machine_service_prepares_real_docker_dataplane() {
    if !local_proof_enabled() {
        return;
    }
    let _guard = local_dataplane_lock().lock().await;
    require_root();

    let _cleanup = DataplaneCleanup;
    cleanup_dataplane();
    let ebpf_ctl = required_path_env(EBPF_CTL_ENV);
    let ebpf_bytecode = required_path_env(EBPF_BYTECODE_ENV);
    let nats = test_nats().await;
    let machine_id = machine_id("core_1");
    let runner = DockerManagedContainerRunner::local_defaults(
        ENDPOINT_NETWORK_SUBNET,
        DEFAULT_DATAPLANE_BRIDGE_IFNAME,
    )
    .expect("connect to local Docker daemon");
    let preparer =
        PloyzNativeMeshPreparer::new(PloyzNativeMeshHostConfig::with_default_key_material(
            machine_id.clone(),
            ebpf_bytecode,
            ebpf_ctl.clone(),
            DEFAULT_DATAPLANE_BRIDGE_IFNAME.to_owned(),
            DEFAULT_DATAPLANE_WG_IFNAME.to_owned(),
        ))
        .with_command_timeout(Duration::from_secs(20));
    let _service = start_machine_runtime_service(
        nats.machine_client.clone(),
        machine_id.clone(),
        runner.clone(),
        preparer,
        runner,
    )
    .await
    .expect("machine runtime service starts");

    let mut runtime = NatsMachineContainerRuntime::new(nats.client.clone())
        .with_request_timeout(Duration::from_secs(30));
    runtime
        .ensure_endpoint_network(
            &machine_id,
            MachineEnsureEndpointNetworkRpcRequest {
                operation_id: operation_id("op_123"),
            },
        )
        .await
        .expect("endpoint network is created through machine service");
    command_ok(
        "ip",
        &["link", "show", "dev", DEFAULT_DATAPLANE_BRIDGE_IFNAME],
    );

    let mut dataplane = NatsMachineDataplanePreparer::new(nats.client)
        .with_request_timeout(Duration::from_secs(30));
    let report = dataplane
        .prepare_dataplane(DataplanePrepareRequest::for_machines(
            operation_id("op_123"),
            vec![machine_id.clone()],
        ))
        .await
        .expect("real dataplane prepares through machine-scoped NATS service");
    let [ready] = report.machines.as_slice() else {
        panic!("expected one machine readiness report");
    };
    assert_eq!(ready.machine_id, machine_id);
    assert_ebpf_attached_evidence(&ready.ready.ebpf_forwarding.evidence, &ebpf_ctl);
}

#[tokio::test]
async fn local_privileged_dataplane_reports_missing_bridge_as_domain_failure() {
    if !local_proof_enabled() {
        return;
    }
    let _guard = local_dataplane_lock().lock().await;
    require_root();

    let _cleanup = DataplaneCleanup;
    cleanup_dataplane();
    let ebpf_ctl = required_path_env(EBPF_CTL_ENV);
    let ebpf_bytecode = required_path_env(EBPF_BYTECODE_ENV);
    let preparer =
        PloyzNativeMeshPreparer::new(PloyzNativeMeshHostConfig::with_default_key_material(
            machine_id("core_1"),
            ebpf_bytecode,
            ebpf_ctl,
            "missing-ployz".to_owned(),
            DEFAULT_DATAPLANE_WG_IFNAME.to_owned(),
        ))
        .with_command_timeout(Duration::from_secs(20));

    let error = preparer
        .prepare_ployz_native_mesh(&endpoint_routes(), &[])
        .await
        .expect_err("missing bridge is a typed dataplane failure");

    assert!(matches!(
        error,
        WireGuardEbpfPrepareError::Unavailable {
            machine_id,
            component: PloyzNativeMeshComponent::EbpfForwarding,
            ..
        } if machine_id == self::machine_id("core_1")
    ));
}

fn local_proof_enabled() -> bool {
    std::env::var(LOCAL_PROOF_ENV).is_ok_and(|value| value == "1")
}

fn local_dataplane_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn require_root() {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .expect("read effective uid");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "0",
        "{LOCAL_PROOF_ENV}=1 requires a privileged root container"
    );
}

fn required_path_env(name: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var(name)
            .unwrap_or_else(|_| panic!("{name} must point at the local dataplane proof artifact")),
    );
    assert!(path.exists(), "{name} does not exist: {}", path.display());
    path
}

fn endpoint_routes() -> Vec<WireGuardEbpfEndpointRoute> {
    vec![
        WireGuardEbpfEndpointRoute::default_for_machine(&machine_id("core_1")),
        WireGuardEbpfEndpointRoute::default_for_machine(&machine_id("edge_2")),
    ]
}

fn edge_peer_with_public_key(public_key: WireGuardPublicKey) -> WireGuardPeer {
    WireGuardPeer {
        machine_id: machine_id("edge_2"),
        endpoint_subnet: "10.42.2.0/24".to_owned(),
        public_endpoint: "203.0.113.2:51820".parse().expect("valid endpoint"),
        public_key,
    }
}

fn assert_wireguard_peer_evidence(evidence: &[WireGuardReadyEvidence]) {
    assert!(evidence.iter().any(|evidence| {
        matches!(
            evidence,
            WireGuardReadyEvidence::Command { program, args }
                if program == "wg" && args.iter().any(|arg| arg == "peer")
        )
    }));
}

fn assert_ebpf_attached_evidence(evidence: &[EbpfForwardingReadyEvidence], ebpf_ctl: &Path) {
    assert!(evidence.iter().any(|evidence| {
        matches!(
            evidence,
            EbpfForwardingReadyEvidence::Command { program, args }
                if program == &ebpf_ctl.display().to_string()
                    && args.first().is_some_and(|arg| arg == "ensure-attached")
        )
    }));
}

fn assert_edge_route_evidence(evidence: &[EbpfForwardingReadyEvidence], ebpf_ctl: &Path) {
    assert!(evidence.iter().any(|evidence| {
        matches!(
            evidence,
            EbpfForwardingReadyEvidence::Command { program, args }
                if program == &ebpf_ctl.display().to_string()
                    && args.as_slice() == ["route", "add-ifname", "10.42.2.0/24", DEFAULT_DATAPLANE_WG_IFNAME]
        )
    }));
}

fn generated_wireguard_public_key() -> WireGuardPublicKey {
    let output = Command::new("sh")
        .args(["-c", "wg genkey | wg pubkey"])
        .output()
        .expect("generate wireguard key");
    assert!(
        output.status.success(),
        "wireguard key generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    WireGuardPublicKey::try_new(String::from_utf8_lossy(&output.stdout).trim())
        .expect("generated wireguard public key is valid")
}

fn cleanup_dataplane() {
    command_ignore(
        "tc",
        &[
            "qdisc",
            "del",
            "dev",
            DEFAULT_DATAPLANE_BRIDGE_IFNAME,
            "clsact",
        ],
    );
    command_ignore("ip", &["link", "del", DEFAULT_DATAPLANE_WG_IFNAME]);
    command_ignore("docker", &["network", "rm", "ployz"]);
    command_ignore("rm", &["-rf", "/sys/fs/bpf/ployz"]);
}

fn command_ok(program: &str, args: &[&str]) {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|source| panic!("run {program} {}: {source}", args.join(" ")));
    assert!(
        output.status.success(),
        "{program} {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn command_ignore(program: &str, args: &[&str]) {
    let _ = Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    /// Controller principal: the deploy-worker request side.
    client: async_nats::Client,
    /// Machine principal: the machine-runtime service side.
    machine_client: async_nats::Client,
}

async fn test_nats() -> TestNats {
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("core_1")]).await;
    let client = nats.controller.clone();
    let machine_client = nats.machine_client(&machine_id("core_1")).await;

    TestNats {
        _nats: nats,
        client,
        machine_client,
    }
}

struct DataplaneCleanup;

impl Drop for DataplaneCleanup {
    fn drop(&mut self) {
        cleanup_dataplane();
    }
}

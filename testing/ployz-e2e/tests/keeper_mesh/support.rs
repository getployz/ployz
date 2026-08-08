use bollard::Docker;
use ployz_core::corrosion::derive_builtin_wireguard_member;
use ployz_core::ids::ClusterId;
use ployz_core::install::{
    AbsoluteInstallPath, ExactPloyzVersion, InstallArtifactSource, InstallArtifactSpec,
    InstallArtifactVersion, InstallSha256Digest,
};
use ployz_core::join::{JoinDoorCertFingerprint, JoinMachineSubstrate};
use ployz_core::network::WireGuardPublicKey;
use ployz_e2e::dind::{DindMachine, ExecOutcome, exec_in_container, write_file_in_container};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
pub const MACHINE_A_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB1";
pub const MACHINE_B_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB2";
pub const PROBE_NAMESPACE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB3";
pub const CORROSION_TOKEN: &str = "ployz-dind-corrosion";
pub const CORROSION_API_PORT: u16 = 8_080;
pub const CORROSION_GOSSIP_PORT: u16 = 8_787;
pub const API_PORT: u16 = 2_020;
pub const WIREGUARD_PORT: u16 = 51_820;
pub const WIREGUARD_INTERFACE: &str = "ployz0";
pub const TEST_BRIDGE_INTERFACE: &str = "ployz-test0";
pub const PLOYZ_BUILD: &str = "keeper-mesh-dind";
pub const CORROSION_VERSION: &str = "0.2.0-beta.0";

const DOOR_PRIVATE_KEY_PATH: &str = "/var/lib/ployz/door.key";
const DOOR_CERTIFICATE_PATH: &str = "/var/lib/ployz/door.crt";
const DOOR_FINGERPRINT_PATH: &str = "/var/lib/ployz/door.fingerprint";
const JOIN_SUBSTRATE_PATH: &str = "/var/lib/ployz/join-substrate.json";
const FIXED_JOIN_SUBSTRATE_ARTIFACTS: [(&str, &str); 5] = [
    ("ployzd", "/usr/local/bin/ployzd"),
    ("ployz-ebpf-tc", "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"),
    ("ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
    ("corrosion", "/usr/local/bin/corrosion"),
    (
        "corrosion-schema-v1.sql",
        "/usr/local/lib/ployz/corrosion-schema-v1.sql",
    ),
];

const WAIT_BUDGET: Duration = Duration::from_secs(45);
const WAIT_DELAY: Duration = Duration::from_millis(250);

pub async fn enable_and_assert_ipv6(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    exec_ok(
        docker,
        machine,
        &[
            "sysctl",
            "-w",
            "net.ipv6.conf.all.disable_ipv6=0",
            "net.ipv6.conf.default.disable_ipv6=0",
            "net.ipv6.conf.all.forwarding=1",
            "net.ipv6.conf.default.forwarding=1",
        ],
    )
    .await?;
    exec_ok(
        docker,
        machine,
        &[
            "sh",
            "-c",
            "test \"$(sysctl -n net.ipv6.conf.all.disable_ipv6)\" = 0 && test \"$(sysctl -n net.ipv6.conf.default.disable_ipv6)\" = 0 && test \"$(sysctl -n net.ipv6.conf.all.forwarding)\" = 1 && test \"$(sysctl -n net.ipv6.conf.default.forwarding)\" = 1",
        ],
    )
    .await?;
    Ok(())
}

pub async fn activate_default_deny_firewall(
    docker: &Docker,
    machine: &DindMachine,
) -> Result<(), String> {
    exec_ok(docker, machine, &["ufw", "--force", "reset"]).await?;
    exec_ok(docker, machine, &["ufw", "default", "deny", "incoming"]).await?;
    exec_ok(docker, machine, &["ufw", "default", "allow", "outgoing"]).await?;
    exec_ok(docker, machine, &["ufw", "--force", "enable"]).await?;
    exec_ok(
        docker,
        machine,
        &["systemctl", "enable", "--now", "ufw.service"],
    )
    .await?;
    exec_ok(
        docker,
        machine,
        &[
            "sh",
            "-c",
            "systemctl is-active --quiet ufw.service && ufw status verbose | grep -F 'Status: active' && ufw status verbose | grep -F 'Default: deny (incoming), allow (outgoing)' && ! ufw status | grep -Eq '^51820/udp[[:space:]]+ALLOW IN'",
        ],
    )
    .await?;
    Ok(())
}

pub async fn assert_product_configured_mesh_firewall(
    docker: &Docker,
    machine: &DindMachine,
    allowed_machine_subnets: [&str; 2],
) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "product-managed UFW mesh rules",
        vec!["ufw".to_owned(), "status".to_owned(), "verbose".to_owned()],
        |outcome| outcome.success() && ufw_mesh_rules_present(&outcome.stdout),
    )
    .await?;

    let expected = allowed_machine_subnets
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    wait_for_command(
        docker,
        machine,
        "machine-only Corrosion gossip firewall",
        vec!["ip6tables".to_owned(), "-S".to_owned()],
        |outcome| outcome.success() && corrosion_firewall_matches(&outcome.stdout, &expected),
    )
    .await
}

fn ufw_mesh_rules_present(status: &str) -> bool {
    let wireguard = status.lines().any(|line| {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        matches!(
            columns.as_slice(),
            ["51820/udp", "ALLOW", "IN", "Anywhere", ..]
        )
    });
    let api_http = status
        .lines()
        .filter(|line| ufw_comment(line) == Some("ployz-api-http"))
        .collect::<Vec<_>>();
    let gossip_backstop = status
        .lines()
        .filter(|line| ufw_comment(line) == Some("ployz-corrosion-gossip"))
        .collect::<Vec<_>>();
    wireguard
        && matches!(api_http.as_slice(), [line] if current_ufw_rule(line, API_PORT, "tcp", "ALLOW IN"))
        && matches!(gossip_backstop.as_slice(), [line] if current_ufw_rule(line, CORROSION_GOSSIP_PORT, "udp", "DENY IN"))
}

fn ufw_comment(line: &str) -> Option<&str> {
    line.split_once(" # ").map(|(_, comment)| comment.trim())
}

fn current_ufw_rule(line: &str, port: u16, protocol: &str, action: &str) -> bool {
    let port_protocol = format!("{port}/{protocol}");
    line.split_whitespace().any(|field| field == port_protocol)
        && line.contains(action)
        && line.contains("(v6)")
        && line
            .split_whitespace()
            .any(|column| column == WIREGUARD_INTERFACE)
}

fn corrosion_firewall_matches(rules: &str, expected_sources: &BTreeSet<String>) -> bool {
    let input_jumps = rules
        .lines()
        .filter(|line| {
            line.starts_with("-A INPUT ")
                && line
                    .split_ascii_whitespace()
                    .collect::<Vec<_>>()
                    .windows(2)
                    .any(|fields| fields == ["-j", "PLOYZ-CORROSION"])
        })
        .collect::<Vec<_>>();
    if !matches!(input_jumps.as_slice(), [line] if current_corrosion_input_jump(line)) {
        return false;
    }

    let chain_rules = rules
        .lines()
        .filter(|line| line.starts_with("-A PLOYZ-CORROSION "))
        .collect::<Vec<_>>();
    let Some((reject, accepts)) = chain_rules.split_last() else {
        return false;
    };
    if !terminal_corrosion_reject(reject) || accepts.len() != expected_sources.len() {
        return false;
    }
    let Some(sources) = accepts
        .iter()
        .map(|line| corrosion_accept_source(line))
        .collect::<Option<BTreeSet<_>>>()
    else {
        return false;
    };
    sources == *expected_sources
}

fn current_corrosion_input_jump(line: &str) -> bool {
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    matches!(
        fields.as_slice(),
        ["-A", "INPUT", "-i", WIREGUARD_INTERFACE, "-p", "udp", "-m", "udp", "--dport", port, "-j", "PLOYZ-CORROSION"]
            | ["-A", "INPUT", "-i", WIREGUARD_INTERFACE, "-p", "udp", "--dport", port, "-j", "PLOYZ-CORROSION"]
            if *port == CORROSION_GOSSIP_PORT.to_string()
    )
}

fn corrosion_accept_source(line: &str) -> Option<String> {
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    let ["-A", "PLOYZ-CORROSION", "-s", source, "-j", "ACCEPT"] = fields.as_slice() else {
        return None;
    };
    Some((*source).to_owned())
}

fn terminal_corrosion_reject(line: &str) -> bool {
    matches!(
        line.split_ascii_whitespace().collect::<Vec<_>>().as_slice(),
        ["-A", "PLOYZ-CORROSION", "-j", "REJECT"]
            | ["-A", "PLOYZ-CORROSION", "-j", "REJECT", "--reject-with", _]
    )
}

pub async fn install_keeper_unit(
    docker: &Docker,
    machine: &DindMachine,
    machine_id: &str,
    bridge_ifname: &str,
) -> Result<(), String> {
    if machine.cgroup_root == "/sys/fs/cgroup" {
        return Err(format!(
            "{} Keeper fixture refused the unified host cgroup root",
            machine.name
        ));
    }
    let unit = format!(
        "[Unit]\nDescription=Ployz Keeper mesh test\nAfter=network.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nRuntimeDirectory=ployz-control\nEnvironment=PLOYZ_CLUSTER_ID={CLUSTER_ID}\nEnvironment=PLOYZ_MACHINE_ID={machine_id}\nEnvironment=PLOYZ_CORROSION_API_ADDR=127.0.0.1:{CORROSION_API_PORT}\nEnvironment=PLOYZ_CORROSION_BEARER_TOKEN={CORROSION_TOKEN}\nEnvironment=PLOYZ_CORROSION_GOSSIP_PORT={CORROSION_GOSSIP_PORT}\nEnvironment=PLOYZ_API_LISTEN_ADDR=[::]:{API_PORT}\nEnvironment=PLOYZ_WIREGUARD_PRIVATE_KEY_PATH=/var/lib/ployz/wireguard.key\nEnvironment=PLOYZ_WIREGUARD_INTERFACE={WIREGUARD_INTERFACE}\nEnvironment=PLOYZ_WIREGUARD_LISTEN_PORT={WIREGUARD_PORT}\nEnvironment=PLOYZ_BRIDGE_INTERFACE={bridge_ifname}\nEnvironment=PLOYZ_EBPF_CTL_PATH=/opt/ployz/artifacts/ployz-ebpf-ctl\nEnvironment=PLOYZ_EBPF_BYTECODE_PATH=/opt/ployz/artifacts/ployz-ebpf-tc\nEnvironment=PLOYZ_EBPF_PIN_PATH=/sys/fs/bpf/ployz\nEnvironment=PLOYZ_ISOLATION_CGROUP_ROOT={}\nEnvironment=PLOYZ_CORROSION_VERSION={CORROSION_VERSION}\nEnvironment=PLOYZ_SUPERVISOR_BACKEND=systemd\nEnvironment=PLOYZ_KEEPER_HOST_COMMAND_TIMEOUT_MS=5000\nEnvironment=PLOYZ_KEEPER_HOST_FOLD_TIMEOUT_MS=15000\nEnvironment=PLOYZ_KEEPER_RECONCILE_INTERVAL_MS=250\nEnvironment=PLOYZ_KEEPER_RETRY_INITIAL_MS=100\nEnvironment=PLOYZ_KEEPER_RETRY_MAX_MS=1000\nEnvironment=PLOYZ_LOG=debug\nExecStart=/opt/ployz/artifacts/ployzd keeper\nRestart=on-failure\nRestartSec=100ms\n\n[Install]\nWantedBy=multi-user.target\n",
        machine.cgroup_root
    );
    write_file_in_container(
        docker,
        &machine.container_id,
        "/etc/systemd/system/ployz-keeper.service",
        &unit,
        "0644",
    )
    .await
    .map_err(|error| error.to_string())
}

pub async fn install_corrosion(
    docker: &Docker,
    machine: &DindMachine,
    own_address: Ipv6Addr,
    peer_address: Ipv6Addr,
) -> Result<(), String> {
    let config = format!(
        "[db]\npath = \"/var/lib/ployz/corrosion.db\"\nschema_paths = [\"/opt/ployz/artifacts/corrosion-schema-v1.sql\"]\nsubscriptions_path = \"/var/lib/ployz/subscriptions\"\n\n[gossip]\naddr = \"[{own_address}]:{CORROSION_GOSSIP_PORT}\"\nbootstrap = [\"[{peer_address}]:{CORROSION_GOSSIP_PORT}\"]\nplaintext = true\nmax_mtu = 1232\n\n[api]\naddr = \"127.0.0.1:{CORROSION_API_PORT}\"\nauthz.bearer-token = \"{CORROSION_TOKEN}\"\n\n[admin]\npath = \"/run/ployz/corrosion-admin.sock\"\n"
    );
    let unit = "[Unit]\nDescription=Pinned Corrosion mesh test\nAfter=network.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nExecStartPre=/usr/bin/install -d -m 0755 /var/lib/ployz/subscriptions /run/ployz\nExecStart=/opt/ployz/artifacts/corrosion --config /etc/ployz/corrosion.toml agent\nRestart=on-failure\nRestartSec=250ms\n\n[Install]\nWantedBy=multi-user.target\n";
    write_file_in_container(
        docker,
        &machine.container_id,
        "/etc/ployz/corrosion.toml",
        &config,
        "0600",
    )
    .await
    .map_err(|error| error.to_string())?;
    write_file_in_container(
        docker,
        &machine.container_id,
        "/etc/systemd/system/corrosion.service",
        unit,
        "0644",
    )
    .await
    .map_err(|error| error.to_string())
}

pub async fn install_api_unit(
    docker: &Docker,
    machine: &DindMachine,
    machine_id: &str,
    own_address: Ipv6Addr,
) -> Result<(), String> {
    let door = join_door_fixture()?;
    let substrate = render_installed_join_substrate(docker, machine).await?;
    for (path, contents, mode) in [
        (DOOR_PRIVATE_KEY_PATH, door.private_key.as_str(), "0600"),
        (DOOR_CERTIFICATE_PATH, door.certificate.as_str(), "0644"),
        (DOOR_FINGERPRINT_PATH, door.fingerprint.as_str(), "0644"),
        (JOIN_SUBSTRATE_PATH, substrate.as_str(), "0600"),
    ] {
        write_file_in_container(docker, &machine.container_id, path, contents, mode)
            .await
            .map_err(|error| error.to_string())?;
    }
    let unit = render_api_unit(machine_id, own_address);
    write_file_in_container(
        docker,
        &machine.container_id,
        "/etc/systemd/system/ployz-api.service",
        &unit,
        "0644",
    )
    .await
    .map_err(|error| error.to_string())
}

fn render_api_unit(machine_id: &str, own_address: Ipv6Addr) -> String {
    format!(
        "[Unit]\nDescription=Ployz API mesh test\nAfter=corrosion.service ployz-keeper.service\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nEnvironment=PLOYZ_CORROSION_API_ADDR=127.0.0.1:{CORROSION_API_PORT}\nEnvironment=PLOYZ_CORROSION_BEARER_TOKEN={CORROSION_TOKEN}\nEnvironment=PLOYZ_CLUSTER_ID={CLUSTER_ID}\nEnvironment=PLOYZ_MACHINE_ID={machine_id}\nEnvironment=PLOYZ_API_LISTEN_ADDR=[{own_address}]:{API_PORT}\nEnvironment=PLOYZ_API_DOOR_PRIVATE_KEY_PATH={DOOR_PRIVATE_KEY_PATH}\nEnvironment=PLOYZ_API_DOOR_CERTIFICATE_PATH={DOOR_CERTIFICATE_PATH}\nEnvironment=PLOYZ_API_DOOR_FINGERPRINT_PATH={DOOR_FINGERPRINT_PATH}\nEnvironment=PLOYZ_API_JOIN_SUBSTRATE_PATH={JOIN_SUBSTRATE_PATH}\nEnvironment=PLOYZ_BUILD={PLOYZ_BUILD}\nEnvironment=PLOYZ_LOG=debug\nExecStart=/opt/ployz/artifacts/ployzd api\nRestart=on-failure\nRestartSec=250ms\n\n[Install]\nWantedBy=multi-user.target\n"
    )
}

struct JoinDoorFixture {
    private_key: String,
    certificate: String,
    fingerprint: String,
}

static JOIN_DOOR_FIXTURE: OnceLock<Result<JoinDoorFixture, String>> = OnceLock::new();

fn join_door_fixture() -> Result<&'static JoinDoorFixture, String> {
    match JOIN_DOOR_FIXTURE.get_or_init(build_join_door_fixture) {
        Ok(fixture) => Ok(fixture),
        Err(error) => Err(error.clone()),
    }
}

fn build_join_door_fixture() -> Result<JoinDoorFixture, String> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(["door.ployz.internal".to_owned()])
            .map_err(|error| error.to_string())?;
    let fingerprint = JoinDoorCertFingerprint::for_certificate_der(cert.der().as_ref());
    Ok(JoinDoorFixture {
        private_key: signing_key.serialize_pem(),
        certificate: cert.pem(),
        fingerprint: format!("{}\n", fingerprint.as_str()),
    })
}

async fn render_installed_join_substrate(
    docker: &Docker,
    machine: &DindMachine,
) -> Result<String, String> {
    let artifacts = FIXED_JOIN_SUBSTRATE_ARTIFACTS;
    let mut digests = Vec::with_capacity(artifacts.len());
    for (artifact, _) in artifacts {
        let source = format!("/opt/ployz/artifacts/{artifact}");
        let outcome = exec_ok(docker, machine, &["sha256sum", &source]).await?;
        let Some(digest) = outcome.stdout.split_whitespace().next() else {
            return Err(format!("sha256sum returned no digest for {source}"));
        };
        digests.push(digest.to_owned());
    }
    let substrate = join_substrate_from_digests(&digests)?;
    serde_json::to_string_pretty(&substrate).map_err(|error| error.to_string())
}

fn join_substrate_from_digests(digests: &[String]) -> Result<JoinMachineSubstrate, String> {
    let artifact_contract = FIXED_JOIN_SUBSTRATE_ARTIFACTS;
    if digests.len() != artifact_contract.len() {
        return Err(format!(
            "join substrate requires {} artifact digests, got {}",
            artifact_contract.len(),
            digests.len()
        ));
    }
    let artifacts = artifact_contract
        .into_iter()
        .zip(digests)
        .map(|((artifact, install_path), digest)| {
            fixture_install_artifact(artifact, install_path, digest)
        })
        .collect::<Result<Vec<_>, _>>()?;
    JoinMachineSubstrate::try_new(
        ExactPloyzVersion::try_new(env!("CARGO_PKG_VERSION")).map_err(|error| error.to_string())?,
        CORROSION_VERSION,
        artifacts,
    )
    .map_err(|error| error.to_string())
}

fn fixture_install_artifact(
    artifact: &str,
    install_path: &str,
    digest: &str,
) -> Result<InstallArtifactSpec, String> {
    let version = match artifact {
        "corrosion" | "corrosion-schema-v1.sql" => CORROSION_VERSION,
        _ => env!("CARGO_PKG_VERSION"),
    };
    Ok(InstallArtifactSpec {
        version: InstallArtifactVersion::try_new(version).map_err(|error| error.to_string())?,
        source: InstallArtifactSource::try_new(format!("/opt/ployz/artifacts/{artifact}"))
            .map_err(|error| error.to_string())?,
        sha256: InstallSha256Digest::try_new(digest).map_err(|error| error.to_string())?,
        install_path: AbsoluteInstallPath::try_new(install_path)
            .map_err(|error| error.to_string())?,
    })
}

pub async fn start_unit(docker: &Docker, machine: &DindMachine, unit: &str) -> Result<(), String> {
    exec_ok(docker, machine, &["systemctl", "daemon-reload"]).await?;
    exec_ok(docker, machine, &["systemctl", "start", unit]).await?;
    Ok(())
}

pub async fn wait_for_public_key(
    docker: &Docker,
    machine: &DindMachine,
) -> Result<WireGuardPublicKey, String> {
    let mut last = String::from("WireGuard interface not observed");
    let deadline = Instant::now() + WAIT_BUDGET;
    while Instant::now() < deadline {
        match exec_in_container(
            docker,
            &machine.container_id,
            &["wg", "show", WIREGUARD_INTERFACE, "public-key"],
        )
        .await
        {
            Ok(outcome) if outcome.success() => {
                match WireGuardPublicKey::try_new(outcome.stdout.trim()) {
                    Ok(key) => return Ok(key),
                    Err(error) => last = error.to_string(),
                }
            }
            Ok(outcome) => last = render_failure(&outcome),
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!(
        "{} did not provision {WIREGUARD_INTERFACE}: {last}",
        machine.name
    ))
}

pub fn derived_address(public_key: &WireGuardPublicKey) -> Result<Ipv6Addr, String> {
    let cluster_id = ClusterId::try_new(CLUSTER_ID).map_err(|error| error.to_string())?;
    Ok(derive_builtin_wireguard_member(&cluster_id, public_key)
        .bind_address()
        .get())
}

pub fn derived_subnet(public_key: &WireGuardPublicKey) -> Result<String, String> {
    let cluster_id = ClusterId::try_new(CLUSTER_ID).map_err(|error| error.to_string())?;
    Ok(derive_builtin_wireguard_member(&cluster_id, public_key)
        .subnet()
        .to_string())
}

pub async fn wait_for_interface_address(
    docker: &Docker,
    machine: &DindMachine,
    address: Ipv6Addr,
) -> Result<(), String> {
    let expected = format!("{address}/112");
    wait_for_command(
        docker,
        machine,
        "derived WireGuard ULA",
        vec![
            "ip".to_owned(),
            "-o".to_owned(),
            "-6".to_owned(),
            "address".to_owned(),
            "show".to_owned(),
            "dev".to_owned(),
            WIREGUARD_INTERFACE.to_owned(),
        ],
        |outcome| outcome.success() && outcome.stdout.contains(&expected),
    )
    .await
}

pub fn endpoint(machine: &DindMachine) -> Result<String, String> {
    let IpAddr::V4(address) = machine.bridge_ip else {
        return Err(format!(
            "{} received non-IPv4 outer DinD address {}",
            machine.name, machine.bridge_ip
        ));
    };
    Ok(format!("{address}:{WIREGUARD_PORT}"))
}

pub async fn wait_for_corrosion(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "Corrosion loopback query",
        corrosion_curl_command("v1/queries", &json!("SELECT 1")),
        ExecOutcome::success,
    )
    .await
}

pub async fn corrosion_transaction(
    docker: &Docker,
    machine: &DindMachine,
    statements: &Value,
) -> Result<String, String> {
    let command = corrosion_curl_command("v1/transactions", statements);
    let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
    let outcome = exec_ok(docker, machine, &refs).await?;
    if outcome.stdout.contains("\"error\"") {
        return Err(format!(
            "{} Corrosion transaction failed: {}",
            machine.name, outcome.stdout
        ));
    }
    Ok(outcome.stdout)
}

pub fn corrosion_curl_command(path: &str, body: &Value) -> Vec<String> {
    vec![
        "curl".to_owned(),
        "--fail".to_owned(),
        "--silent".to_owned(),
        "--show-error".to_owned(),
        "--max-time".to_owned(),
        "3".to_owned(),
        "-H".to_owned(),
        format!("Authorization: Bearer {CORROSION_TOKEN}"),
        "-H".to_owned(),
        "Accept: application/json".to_owned(),
        "-H".to_owned(),
        "Content-Type: application/json".to_owned(),
        "--data-binary".to_owned(),
        body.to_string(),
        format!("http://127.0.0.1:{CORROSION_API_PORT}/{path}"),
    ]
}

pub async fn wait_for_corrosion_row(
    docker: &Docker,
    machine: &DindMachine,
    table: &str,
    id: &str,
) -> Result<(), String> {
    let statement = json!([format!("SELECT id FROM {table} WHERE id = ?"), [id]]);
    wait_for_command(
        docker,
        machine,
        "Corrosion gossip row",
        corrosion_curl_command("v1/queries", &statement),
        |outcome| outcome.success() && outcome.stdout.contains(id),
    )
    .await
}

pub async fn wait_for_corrosion_row_absent(
    docker: &Docker,
    machine: &DindMachine,
    table: &str,
    id: &str,
) -> Result<(), String> {
    let statement = json!([format!("SELECT id FROM {table} WHERE id = ?"), [id]]);
    wait_for_command(
        docker,
        machine,
        "Corrosion tombstone convergence",
        corrosion_curl_command("v1/queries", &statement),
        |outcome| outcome.success() && !outcome.stdout.contains(id),
    )
    .await
}

pub async fn wait_for_command<Predicate>(
    docker: &Docker,
    machine: &DindMachine,
    description: &str,
    command: Vec<String>,
    mut predicate: Predicate,
) -> Result<(), String>
where
    Predicate: FnMut(&ExecOutcome) -> bool,
{
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("command was not attempted");
    let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
    while Instant::now() < deadline {
        match exec_in_container(docker, &machine.container_id, &refs).await {
            Ok(outcome) if predicate(&outcome) => return Ok(()),
            Ok(outcome) => last = render_failure(&outcome),
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(WAIT_DELAY).await;
    }
    Err(format!(
        "{} did not reach {description} in {WAIT_BUDGET:?}: {last}",
        machine.name
    ))
}

pub async fn exec_ok(
    docker: &Docker,
    machine: &DindMachine,
    command: &[&str],
) -> Result<ExecOutcome, String> {
    let outcome = exec_in_container(docker, &machine.container_id, command)
        .await
        .map_err(|error| error.to_string())?;
    if outcome.success() {
        Ok(outcome)
    } else {
        Err(format!(
            "{} command {:?} failed: {}",
            machine.name,
            command,
            render_failure(&outcome)
        ))
    }
}

pub fn render_failure(outcome: &ExecOutcome) -> String {
    format!(
        "exit {} stdout={:?} stderr={:?}",
        outcome.exit_code,
        outcome.stdout.trim(),
        outcome.stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn endpoint_rejects_an_ipv6_outer_network() {
        let machine = DindMachine {
            name: "bad-outer-network".to_owned(),
            container_id: "container".to_owned(),
            bridge_ip: IpAddr::V6(Ipv6Addr::LOCALHOST),
            cgroup_root: "/sys/fs/cgroup/test-bad".to_owned(),
        };
        assert!(endpoint(&machine).is_err());
        let machine = DindMachine {
            name: "outer-network".to_owned(),
            container_id: "container".to_owned(),
            bridge_ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
            cgroup_root: "/sys/fs/cgroup/test-good".to_owned(),
        };
        assert_eq!(
            endpoint(&machine).expect("IPv4 endpoint"),
            "192.0.2.10:51820"
        );
    }

    #[test]
    fn managed_firewall_requires_exact_machine_gossip_sources() {
        let expected = ["fd00::/112", "fd01::/112"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let rules = "-A INPUT -i ployz0 -p udp -m udp --dport 8787 -j PLOYZ-CORROSION\n\
-A PLOYZ-CORROSION -s fd00::/112 -j ACCEPT\n\
-A PLOYZ-CORROSION -s fd01::/112 -j ACCEPT\n\
-A PLOYZ-CORROSION -j REJECT --reject-with icmp6-port-unreachable\n";
        assert!(corrosion_firewall_matches(rules, &expected));
        assert!(!corrosion_firewall_matches(
            &rules.replace(
                "-j REJECT",
                "-s fd02::/112 -j ACCEPT\n-A PLOYZ-CORROSION -j REJECT"
            ),
            &expected
        ));
        for unsafe_rules in [
            rules.replacen("-A INPUT ", "-A INPUT -j PLOYZ-CORROSION\n-A INPUT ", 1),
            rules.replace(
                "-A PLOYZ-CORROSION -j REJECT",
                "-A PLOYZ-CORROSION -j RETURN\n-A PLOYZ-CORROSION -j REJECT",
            ),
            rules.replace(
                "-A PLOYZ-CORROSION -j REJECT",
                "-A PLOYZ-CORROSION -j ACCEPT\n-A PLOYZ-CORROSION -j REJECT",
            ),
            format!("{rules}-A PLOYZ-CORROSION -j REJECT\n"),
        ] {
            assert!(!corrosion_firewall_matches(&unsafe_rules, &expected));
        }
    }

    #[test]
    fn managed_ufw_rules_cover_outer_wireguard_and_inner_mesh() {
        let status = "51820/udp                 ALLOW IN    Anywhere\n\
2020/tcp (v6) on ployz0    ALLOW IN    Anywhere (v6) # ployz-api-http\n\
8787/udp (v6) on ployz0    DENY IN     Anywhere (v6) # ployz-corrosion-gossip\n";
        assert!(ufw_mesh_rules_present(status));
        assert!(!ufw_mesh_rules_present(
            "51820/udp                 ALLOW IN    Anywhere\n\
2020/tcp (v6) on ployz0    ALLOW IN    Anywhere (v6) # ployz-api-http\n"
        ));
        assert!(!ufw_mesh_rules_present(&format!(
            "{status}8787/udp (v6) on ployz0 DENY IN Anywhere (v6) # ployz-corrosion-gossip\n"
        )));
        assert!(!ufw_mesh_rules_present(&format!(
            "{status}7777/udp on old0 DENY IN Anywhere # ployz-corrosion-gossip\n"
        )));
        assert!(!ufw_mesh_rules_present(
            &status.replace("2020/tcp", "12020/tcp")
        ));
        assert!(!ufw_mesh_rules_present(
            &status.replace("8787/udp", "18787/udp")
        ));
        assert!(ufw_mesh_rules_present(&format!(
            "{status}7777/udp (v6) on old0 DENY IN Anywhere (v6) # foreign-ployz-corrosion-gossip\n"
        )));
    }

    #[test]
    fn api_unit_binds_the_derived_machine_ula() {
        let address = "fd12:3456:789a::42".parse().expect("ULA");

        let unit = render_api_unit(MACHINE_A_ID, address);

        assert!(unit.contains("Environment=PLOYZ_API_LISTEN_ADDR=[fd12:3456:789a::42]:2020"));
        assert!(!unit.contains("PLOYZ_API_LISTEN_ADDR=[::]:"));
    }

    #[test]
    fn api_fixture_declares_every_complete_join_door_input() {
        let address = "fd12:3456:789a::42".parse().expect("ULA");

        let unit = render_api_unit(MACHINE_A_ID, address);

        for expected in [
            "Environment=PLOYZ_API_DOOR_PRIVATE_KEY_PATH=/var/lib/ployz/door.key",
            "Environment=PLOYZ_API_DOOR_CERTIFICATE_PATH=/var/lib/ployz/door.crt",
            "Environment=PLOYZ_API_DOOR_FINGERPRINT_PATH=/var/lib/ployz/door.fingerprint",
            "Environment=PLOYZ_API_JOIN_SUBSTRATE_PATH=/var/lib/ployz/join-substrate.json",
        ] {
            assert!(unit.contains(expected), "missing {expected:?} from {unit}");
        }

        let door = join_door_fixture().expect("complete join-door fixture");
        assert!(door.private_key.contains("BEGIN PRIVATE KEY"));
        assert!(door.certificate.contains("BEGIN CERTIFICATE"));
        assert_eq!(door.fingerprint.trim().len(), 64);
        let digests = (0..FIXED_JOIN_SUBSTRATE_ARTIFACTS.len())
            .map(|index| format!("{index:02x}").repeat(32))
            .collect::<Vec<_>>();
        join_substrate_from_digests(&digests).expect("schema-valid complete join substrate");
    }
}

use bollard::Docker;
use ployz_core::corrosion::derive_builtin_wireguard_member;
use ployz_core::ids::ClusterId;
use ployz_core::network::WireGuardPublicKey;
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, DindMachine, ExecOutcome, MachineSpec, artifact_dir,
    connect_docker, e2e_enabled, exec_in_container, keep_requested, machine_image, shell_quote,
    write_file_in_container,
};
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::time::{Duration, Instant};

const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const MACHINE_A_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB1";
const MACHINE_B_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB2";
const PROBE_NAMESPACE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB3";
const CORROSION_TOKEN: &str = "ployz-dind-corrosion";
const CORROSION_API_PORT: u16 = 8_080;
const CORROSION_GOSSIP_PORT: u16 = 8_787;
const API_PORT: u16 = 20_20;
const WIREGUARD_PORT: u16 = 51_820;
const WIREGUARD_INTERFACE: &str = "ployz0";
const TEST_BRIDGE_INTERFACE: &str = "ployz-test0";
const PLOYZ_BUILD: &str = "keeper-mesh-dind";
const CORROSION_VERSION: &str = "0.2.0-beta.0";
const WAIT_BUDGET: Duration = Duration::from_secs(45);
const WAIT_DELAY: Duration = Duration::from_millis(250);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_machine_keeper_converges_and_fences_builtin_mesh() {
    if !e2e_enabled() {
        eprintln!("skipping Keeper mesh DinD proof; set PLOYZ_DIND_E2E=1 to enable it");
        return;
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    panic!("the pinned Corrosion Keeper mesh proof supports only Linux x86_64");

    let docker = connect_docker().expect("connect to Docker for Keeper mesh proof");
    let cluster = DindCluster::provision(
        &docker,
        DindClusterSpec {
            artifact_dir: artifact_dir(),
            machines: vec![
                MachineSpec {
                    image: machine_image(),
                },
                MachineSpec {
                    image: machine_image(),
                },
            ],
        },
    )
    .await
    .expect("provision two privileged DinD machines");

    let result = exercise_keeper_mesh(&docker, &cluster).await;
    if let Err(error) = &result {
        match cluster.capture_evidence().await {
            Ok(path) => eprintln!("Keeper mesh evidence captured under {}", path.display()),
            Err(capture_error) => eprintln!("Keeper mesh evidence capture failed: {capture_error}"),
        }
        eprintln!("Keeper mesh proof failed: {error}");
    }
    if keep_requested() {
        eprintln!(
            "retaining DinD run {} because PLOYZ_DIND_KEEP=1",
            cluster.run_id()
        );
    } else {
        cluster.teardown().await.expect("tear down Keeper mesh run");
    }
    result.unwrap_or_else(|error| panic!("Keeper mesh proof failed: {error}"));
}

async fn exercise_keeper_mesh(docker: &Docker, cluster: &DindCluster) -> Result<(), String> {
    let [machine_a, machine_b] = cluster.machines() else {
        return Err("Keeper mesh proof requires exactly two machines".to_owned());
    };
    enable_and_assert_ipv6(docker, machine_a).await?;
    enable_and_assert_ipv6(docker, machine_b).await?;

    install_keeper_unit(docker, machine_a, MACHINE_A_ID).await?;
    install_keeper_unit(docker, machine_b, MACHINE_B_ID).await?;
    start_unit(docker, machine_a, "ployz-keeper.service").await?;
    start_unit(docker, machine_b, "ployz-keeper.service").await?;

    let public_key_a = wait_for_public_key(docker, machine_a).await?;
    let public_key_b = wait_for_public_key(docker, machine_b).await?;
    let address_a = derived_address(&public_key_a)?;
    let address_b = derived_address(&public_key_b)?;
    wait_for_interface_address(docker, machine_a, address_a).await?;
    wait_for_interface_address(docker, machine_b, address_b).await?;

    install_corrosion(docker, machine_a, address_a, address_b).await?;
    install_corrosion(docker, machine_b, address_b, address_a).await?;
    start_unit(docker, machine_a, "corrosion.service").await?;
    start_unit(docker, machine_b, "corrosion.service").await?;
    wait_for_corrosion(docker, machine_a).await?;
    wait_for_corrosion(docker, machine_b).await?;

    let roster = roster_transaction(
        machine_a,
        machine_b,
        &public_key_a,
        &public_key_b,
        address_a,
        address_b,
    )?;
    corrosion_transaction(docker, machine_a, &roster).await?;
    corrosion_transaction(docker, machine_b, &roster).await?;

    wait_for_live_peer(
        docker,
        machine_a,
        &public_key_b,
        address_b,
        "10.210.20.0/24",
    )
    .await?;
    wait_for_live_peer(
        docker,
        machine_b,
        &public_key_a,
        address_a,
        "10.210.10.0/24",
    )
    .await?;

    install_api_unit(docker, machine_a, MACHINE_A_ID, address_a).await?;
    install_api_unit(docker, machine_b, MACHINE_B_ID, address_b).await?;
    start_unit(docker, machine_a, "ployz-api.service").await?;
    start_unit(docker, machine_b, "ployz-api.service").await?;
    wait_for_ula_version(docker, machine_a, address_b).await?;
    wait_for_ula_version(docker, machine_b, address_a).await?;

    wait_for_mesh_status(
        docker,
        machine_a,
        MACHINE_A_ID,
        MeshStatusExpectation::BridgeMissing,
    )
    .await?;
    wait_for_mesh_status(
        docker,
        machine_b,
        MACHINE_B_ID,
        MeshStatusExpectation::BridgeMissing,
    )
    .await?;

    corrosion_transaction(docker, machine_a, &probe_namespace_transaction()?).await?;
    wait_for_corrosion_row(docker, machine_b, "namespaces", PROBE_NAMESPACE_ID).await?;

    create_test_bridge(docker, machine_a).await?;
    create_test_bridge(docker, machine_b).await?;
    wait_for_mesh_status(
        docker,
        machine_a,
        MACHINE_A_ID,
        MeshStatusExpectation::Ready,
    )
    .await?;
    wait_for_mesh_status(
        docker,
        machine_b,
        MACHINE_B_ID,
        MeshStatusExpectation::Ready,
    )
    .await?;
    assert_exact_route_map(docker, machine_a, [10, 210, 20, 0], 24).await?;
    assert_exact_route_map(docker, machine_b, [10, 210, 10, 0], 24).await?;
    assert_status_ownership(docker, machine_a).await?;
    assert_status_ownership(docker, machine_b).await?;

    let delete_b = json!([["DELETE FROM machines WHERE id = ?", [MACHINE_B_ID]]]);
    corrosion_transaction(docker, machine_a, &delete_b).await?;
    wait_for_peer_absent(docker, machine_a, &public_key_b).await?;
    wait_for_route_absent(docker, machine_a, "10.210.20.0/24").await?;
    wait_for_route_absent(docker, machine_a, &derived_subnet(&public_key_b)?).await?;
    wait_for_empty_route_map(docker, machine_a).await?;

    if corrosion_row_is_absent(docker, machine_b, "machines", MACHINE_B_ID).await? {
        wait_for_peer_absent(docker, machine_b, &public_key_a).await?;
        wait_for_route_absent(docker, machine_b, "10.210.10.0/24").await?;
        wait_for_route_absent(docker, machine_b, &derived_subnet(&public_key_a)?).await?;
        wait_for_empty_route_map(docker, machine_b).await?;
    }
    Ok(())
}

async fn enable_and_assert_ipv6(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
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
    let outcome = exec_ok(
        docker,
        machine,
        &[
            "sh",
            "-c",
            "test \"$(sysctl -n net.ipv6.conf.all.disable_ipv6)\" = 0 && test \"$(sysctl -n net.ipv6.conf.default.disable_ipv6)\" = 0 && test \"$(sysctl -n net.ipv6.conf.all.forwarding)\" = 1 && test \"$(sysctl -n net.ipv6.conf.default.forwarding)\" = 1",
        ],
    )
    .await?;
    if outcome.success() {
        Ok(())
    } else {
        Err(format!(
            "{} did not retain required IPv6 sysctls",
            machine.name
        ))
    }
}

async fn install_keeper_unit(
    docker: &Docker,
    machine: &DindMachine,
    machine_id: &str,
) -> Result<(), String> {
    let unit = format!(
        "[Unit]\nDescription=Ployz Keeper mesh test\nAfter=network.target\n\n[Service]\nType=simple\nEnvironment=PLOYZ_CLUSTER_ID={CLUSTER_ID}\nEnvironment=PLOYZ_MACHINE_ID={machine_id}\nEnvironment=PLOYZ_CORROSION_API_ADDR=127.0.0.1:{CORROSION_API_PORT}\nEnvironment=PLOYZ_CORROSION_BEARER_TOKEN={CORROSION_TOKEN}\nEnvironment=PLOYZ_WIREGUARD_PRIVATE_KEY_PATH=/var/lib/ployz/wireguard.key\nEnvironment=PLOYZ_WIREGUARD_INTERFACE={WIREGUARD_INTERFACE}\nEnvironment=PLOYZ_WIREGUARD_LISTEN_PORT={WIREGUARD_PORT}\nEnvironment=PLOYZ_BRIDGE_INTERFACE={TEST_BRIDGE_INTERFACE}\nEnvironment=PLOYZ_EBPF_CTL_PATH=/opt/ployz/artifacts/ployz-ebpf-ctl\nEnvironment=PLOYZ_EBPF_BYTECODE_PATH=/opt/ployz/artifacts/ployz-ebpf-tc\nEnvironment=PLOYZ_EBPF_PIN_PATH=/sys/fs/bpf/ployz\nEnvironment=PLOYZ_CORROSION_VERSION={CORROSION_VERSION}\nEnvironment=PLOYZ_SUPERVISOR_BACKEND=systemd\nEnvironment=PLOYZ_KEEPER_HOST_COMMAND_TIMEOUT_MS=5000\nEnvironment=PLOYZ_KEEPER_HOST_FOLD_TIMEOUT_MS=15000\nEnvironment=PLOYZ_KEEPER_RECONCILE_INTERVAL_MS=250\nEnvironment=PLOYZ_KEEPER_RETRY_INITIAL_MS=100\nEnvironment=PLOYZ_KEEPER_RETRY_MAX_MS=1000\nEnvironment=PLOYZ_LOG=debug\nExecStart=/opt/ployz/artifacts/ployzd keeper\nRestart=on-failure\nRestartSec=100ms\n\n[Install]\nWantedBy=multi-user.target\n"
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

async fn install_corrosion(
    docker: &Docker,
    machine: &DindMachine,
    own_address: Ipv6Addr,
    peer_address: Ipv6Addr,
) -> Result<(), String> {
    let config = format!(
        "[db]\npath = \"/var/lib/ployz/corrosion.db\"\nschema_paths = [\"/opt/ployz/artifacts/corrosion-schema-v1.sql\"]\nsubscriptions_path = \"/var/lib/ployz/subscriptions\"\n\n[gossip]\naddr = \"[{own_address}]:{CORROSION_GOSSIP_PORT}\"\nbootstrap = [\"[{peer_address}]:{CORROSION_GOSSIP_PORT}\"]\nplaintext = true\nmax_mtu = 1232\n\n[api]\naddr = \"127.0.0.1:{CORROSION_API_PORT}\"\nauthz.bearer-token = \"{CORROSION_TOKEN}\"\n\n[admin]\npath = \"/run/ployz/corrosion-admin.sock\"\n"
    );
    let unit = "[Unit]\nDescription=Pinned Corrosion mesh test\nAfter=network.target\n\n[Service]\nType=simple\nExecStartPre=/usr/bin/install -d -m 0755 /var/lib/ployz/subscriptions /run/ployz\nExecStart=/opt/ployz/artifacts/corrosion --config /etc/ployz/corrosion.toml agent\nRestart=on-failure\nRestartSec=250ms\n\n[Install]\nWantedBy=multi-user.target\n";
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

async fn install_api_unit(
    docker: &Docker,
    machine: &DindMachine,
    machine_id: &str,
    own_address: Ipv6Addr,
) -> Result<(), String> {
    let unit = format!(
        "[Unit]\nDescription=Ployz API mesh test\nAfter=corrosion.service ployz-keeper.service\n\n[Service]\nType=simple\nEnvironment=PLOYZ_CORROSION_API_ADDR=127.0.0.1:{CORROSION_API_PORT}\nEnvironment=PLOYZ_CORROSION_BEARER_TOKEN={CORROSION_TOKEN}\nEnvironment=PLOYZ_CLUSTER_ID={CLUSTER_ID}\nEnvironment=PLOYZ_MACHINE_ID={machine_id}\nEnvironment=PLOYZ_API_LISTEN_ADDR=[{own_address}]:{API_PORT}\nEnvironment=PLOYZ_BUILD={PLOYZ_BUILD}\nEnvironment=PLOYZ_LOG=debug\nExecStart=/opt/ployz/artifacts/ployzd api\nRestart=on-failure\nRestartSec=250ms\n\n[Install]\nWantedBy=multi-user.target\n"
    );
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

async fn start_unit(docker: &Docker, machine: &DindMachine, unit: &str) -> Result<(), String> {
    exec_ok(docker, machine, &["systemctl", "daemon-reload"]).await?;
    exec_ok(docker, machine, &["systemctl", "start", unit]).await?;
    Ok(())
}

async fn wait_for_public_key(
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

fn derived_address(public_key: &WireGuardPublicKey) -> Result<Ipv6Addr, String> {
    let cluster_id = ClusterId::try_new(CLUSTER_ID).map_err(|error| error.to_string())?;
    Ok(derive_builtin_wireguard_member(&cluster_id, public_key)
        .bind_address()
        .get())
}

fn derived_subnet(public_key: &WireGuardPublicKey) -> Result<String, String> {
    let cluster_id = ClusterId::try_new(CLUSTER_ID).map_err(|error| error.to_string())?;
    Ok(derive_builtin_wireguard_member(&cluster_id, public_key)
        .subnet()
        .to_string())
}

async fn wait_for_interface_address(
    docker: &Docker,
    machine: &DindMachine,
    address: Ipv6Addr,
) -> Result<(), String> {
    let expected = format!("{address}/112");
    wait_for_command(
        docker,
        machine,
        "derived WireGuard ULA",
        || {
            vec![
                "ip".to_owned(),
                "-o".to_owned(),
                "-6".to_owned(),
                "address".to_owned(),
                "show".to_owned(),
                "dev".to_owned(),
                WIREGUARD_INTERFACE.to_owned(),
            ]
        },
        |outcome| outcome.success() && outcome.stdout.contains(&expected),
    )
    .await
}

fn roster_transaction(
    machine_a: &DindMachine,
    machine_b: &DindMachine,
    public_key_a: &WireGuardPublicKey,
    public_key_b: &WireGuardPublicKey,
    address_a: Ipv6Addr,
    address_b: Ipv6Addr,
) -> Result<Value, String> {
    let endpoint_a = endpoint(machine_a)?;
    let endpoint_b = endpoint(machine_b)?;
    let cluster = json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "name": "keeper-mesh-dind",
        "storage_default": "plain",
        "hostname_mode": {"mode": "disabled"},
        "prefix": "10.210.0.0/16",
        "provider": "builtin_wireguard",
        "acme_directory_url": "https://acme.invalid/directory",
        "acme_contact": null,
        "written_by": {"kind": "machine", "machine_id": MACHINE_A_ID},
        "written_at": "2026-08-04T10:00:00.000000000Z"
    });
    let machine_document = |name: &str,
                            id: &str,
                            public_key: &WireGuardPublicKey,
                            address: Ipv6Addr,
                            endpoint: String,
                            subnet: &str| {
        json!({
            "v": 1,
            "cluster_id": CLUSTER_ID,
            "name": name,
            "lifecycle": "active",
            "transport": {
                "kind": "wireguard",
                "pubkey": public_key.as_str(),
                "addr_v6": address,
                "endpoint": endpoint,
                "subnet_v4": subnet
            },
            "storage": {"mode": "plain", "reason": {"kind": "default"}},
            "written_by": {"kind": "machine", "machine_id": id},
            "written_at": "2026-08-04T10:00:00.000000000Z"
        })
    };
    let machine_a_document = machine_document(
        "edge-a",
        MACHINE_A_ID,
        public_key_a,
        address_a,
        endpoint_a,
        "10.210.10.0/24",
    );
    let machine_b_document = machine_document(
        "edge-b",
        MACHINE_B_ID,
        public_key_b,
        address_b,
        endpoint_b,
        "10.210.20.0/24",
    );
    Ok(json!([
        [
            "INSERT INTO cluster (id, document) VALUES (?, ?)",
            [
                CLUSTER_ID,
                serde_json::to_string(&cluster).map_err(|error| error.to_string())?
            ]
        ],
        [
            "INSERT INTO machines (id, document) VALUES (?, ?)",
            [
                MACHINE_A_ID,
                serde_json::to_string(&machine_a_document).map_err(|error| error.to_string())?
            ]
        ],
        [
            "INSERT INTO machines (id, document) VALUES (?, ?)",
            [
                MACHINE_B_ID,
                serde_json::to_string(&machine_b_document).map_err(|error| error.to_string())?
            ]
        ]
    ]))
}

fn endpoint(machine: &DindMachine) -> Result<String, String> {
    let IpAddr::V4(address) = machine.bridge_ip else {
        return Err(format!(
            "{} received non-IPv4 outer DinD address {}",
            machine.name, machine.bridge_ip
        ));
    };
    Ok(format!("{address}:{WIREGUARD_PORT}"))
}

async fn wait_for_corrosion(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "Corrosion loopback query",
        || corrosion_curl_command("v1/queries", &json!("SELECT 1")),
        ExecOutcome::success,
    )
    .await
}

async fn corrosion_transaction(
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

fn corrosion_curl_command(path: &str, body: &Value) -> Vec<String> {
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

async fn wait_for_live_peer(
    docker: &Docker,
    machine: &DindMachine,
    remote_key: &WireGuardPublicKey,
    remote_address: Ipv6Addr,
    remote_subnet: &str,
) -> Result<(), String> {
    let ping = format!(
        "ping -6 -c 1 -W 1 {}",
        shell_quote(&remote_address.to_string())
    );
    let expected_v6 = derived_subnet(remote_key)?;
    wait_for_command(
        docker,
        machine,
        "exact live WireGuard peer",
        || {
            vec![
                "sh".to_owned(),
                "-c".to_owned(),
                format!("{ping} >/dev/null 2>&1 || true; wg show {WIREGUARD_INTERFACE} dump"),
            ]
        },
        |outcome| {
            outcome.success()
                && parse_peer_dump(&outcome.stdout).is_some_and(|peer| {
                    peer.public_key == remote_key.as_str()
                        && allowed_ips_match(peer.allowed_ips, &expected_v6, remote_subnet)
                        && peer.latest_handshake > 0
                })
        },
    )
    .await
}

fn allowed_ips_match(actual: &str, expected_v6: &str, expected_v4: &str) -> bool {
    let mut actual = actual.split(',').map(str::trim).collect::<Vec<_>>();
    actual.sort_unstable();
    let mut expected = vec![expected_v6, expected_v4];
    expected.sort_unstable();
    actual == expected
}

struct PeerDump<'a> {
    public_key: &'a str,
    allowed_ips: &'a str,
    latest_handshake: u64,
}

fn parse_peer_dump(output: &str) -> Option<PeerDump<'_>> {
    let mut lines = output.lines();
    lines.next()?;
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let fields = line.split('\t').collect::<Vec<_>>();
    Some(PeerDump {
        public_key: fields.first()?,
        allowed_ips: fields.get(3)?,
        latest_handshake: fields.get(4)?.parse().ok()?,
    })
}

async fn wait_for_ula_version(
    docker: &Docker,
    source: &DindMachine,
    destination: Ipv6Addr,
) -> Result<(), String> {
    let url = format!("http://[{destination}]:{API_PORT}/version");
    wait_for_command(
        docker,
        source,
        "API /version over WireGuard ULA",
        || {
            vec![
                "curl".to_owned(),
                "--fail".to_owned(),
                "--silent".to_owned(),
                "--show-error".to_owned(),
                "--max-time".to_owned(),
                "3".to_owned(),
                "--noproxy".to_owned(),
                "*".to_owned(),
                url.clone(),
            ]
        },
        |outcome| outcome.success() && outcome.stdout.contains(PLOYZ_BUILD),
    )
    .await
}

#[derive(Clone, Copy)]
enum MeshStatusExpectation {
    BridgeMissing,
    Ready,
}

async fn wait_for_mesh_status(
    docker: &Docker,
    machine: &DindMachine,
    expected_machine_id: &str,
    expectation: MeshStatusExpectation,
) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "machine_status mesh testimony",
        || corrosion_curl_command("v1/queries", &machine_status_query()),
        |outcome| {
            if !outcome.success() {
                return false;
            }
            let documents = query_documents(&outcome.stdout);
            documents.iter().any(|document| {
                document.get("machine_id").and_then(Value::as_str) == Some(expected_machine_id)
                    && mesh_status_matches(document, expectation)
            })
        },
    )
    .await
}

fn machine_status_query() -> Value {
    json!("SELECT machine_id AS id, document FROM machine_status ORDER BY machine_id")
}

fn query_documents(output: &str) -> Vec<Value> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|frame| frame.get("row").and_then(Value::as_array).cloned())
        .filter_map(|row| row.get(1).and_then(Value::as_array).cloned())
        .filter_map(|values| values.get(1).and_then(Value::as_str).map(str::to_owned))
        .filter_map(|document| serde_json::from_str(&document).ok())
        .collect()
}

fn mesh_status_matches(document: &Value, expectation: MeshStatusExpectation) -> bool {
    let Some(mesh) = document.get("mesh") else {
        return false;
    };
    match expectation {
        MeshStatusExpectation::BridgeMissing => {
            mesh.get("state").and_then(Value::as_str) == Some("degraded")
                && mesh
                    .pointer("/degradation/components")
                    .and_then(Value::as_str)
                    == Some("ebpf")
                && mesh
                    .pointer("/degradation/wireguard/converged_at")
                    .is_some()
                && mesh
                    .pointer("/degradation/ebpf/reason/kind")
                    .and_then(Value::as_str)
                    == Some("missing_bridge")
                && mesh
                    .pointer("/degradation/ebpf/reason/ifname")
                    .and_then(Value::as_str)
                    == Some(TEST_BRIDGE_INTERFACE)
        }
        MeshStatusExpectation::Ready => {
            mesh.get("state").and_then(Value::as_str) == Some("converged")
                && mesh.pointer("/wireguard/converged_at").is_some()
                && mesh.pointer("/ebpf/converged_at").is_some()
                && mesh.get("last_successful_converge").is_some()
        }
    }
}

fn probe_namespace_transaction() -> Result<Value, String> {
    let document = json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "name": "gossip-proof",
        "written_by": {"kind": "machine", "machine_id": MACHINE_A_ID},
        "written_at": "2026-08-04T10:01:00.000000000Z"
    });
    Ok(json!([[
        "INSERT INTO namespaces (id, document) VALUES (?, ?)",
        [
            PROBE_NAMESPACE_ID,
            serde_json::to_string(&document).map_err(|error| error.to_string())?
        ]
    ]]))
}

async fn wait_for_corrosion_row(
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
        || corrosion_curl_command("v1/queries", &statement),
        |outcome| outcome.success() && outcome.stdout.contains(id),
    )
    .await
}

async fn create_test_bridge(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    exec_ok(
        docker,
        machine,
        &[
            "sh",
            "-c",
            &format!(
                "ip link show {} >/dev/null 2>&1 || ip link add {} type bridge; ip link set {} up",
                shell_quote(TEST_BRIDGE_INTERFACE),
                shell_quote(TEST_BRIDGE_INTERFACE),
                shell_quote(TEST_BRIDGE_INTERFACE)
            ),
        ],
    )
    .await?;
    Ok(())
}

async fn assert_exact_route_map(
    docker: &Docker,
    machine: &DindMachine,
    network: [u8; 4],
    prefix_len: u8,
) -> Result<(), String> {
    let outcome = exec_ok(
        docker,
        machine,
        &[
            "/opt/ployz/artifacts/bpftool",
            "-j",
            "map",
            "dump",
            "pinned",
            "/sys/fs/bpf/ployz/routes",
        ],
    )
    .await?;
    let entries = serde_json::from_str::<Value>(&outcome.stdout)
        .map_err(|error| format!("decode bpftool JSON on {}: {error}", machine.name))?;
    let Some([entry]) = entries.as_array().map(Vec::as_slice) else {
        return Err(format!(
            "{} eBPF route map was not a singleton: {}",
            machine.name, outcome.stdout
        ));
    };
    let key = decode_route_key(entry.get("key"))?;
    let expected = (network, u32::from(prefix_len));
    if key != expected {
        return Err(format!(
            "{} eBPF route key was {key:?}, expected {expected:?}",
            machine.name
        ));
    }
    let value = decode_route_value(entry.get("value"))?;
    let ifindex = exec_ok(
        docker,
        machine,
        &[
            "cat",
            &format!("/sys/class/net/{WIREGUARD_INTERFACE}/ifindex"),
        ],
    )
    .await?
    .stdout
    .trim()
    .parse::<u32>()
    .map_err(|error| format!("parse WireGuard ifindex on {}: {error}", machine.name))?;
    if value != ifindex {
        return Err(format!(
            "{} eBPF route value was {value}, expected ifindex {ifindex}",
            machine.name
        ));
    }
    Ok(())
}

fn decode_route_key(value: Option<&Value>) -> Result<([u8; 4], u32), String> {
    if value.is_some_and(Value::is_array) {
        let bytes = decode_bpftool_bytes(value)?;
        let Some((network, prefix)) = bytes.split_at_checked(4) else {
            return Err(format!("bpftool route key had {} bytes", bytes.len()));
        };
        let network = <[u8; 4]>::try_from(network)
            .map_err(|_| format!("bpftool route key network had {} bytes", network.len()))?;
        let prefix = <[u8; 4]>::try_from(prefix)
            .map_err(|_| format!("bpftool route key prefix had {} bytes", prefix.len()))?;
        return Ok((network, u32::from_ne_bytes(prefix)));
    }
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| format!("bpftool route key was neither raw nor typed: {value:?}"))?;
    let stored_network = json_u32(object.get("network"))?;
    let prefix = json_u32(object.get("prefix_len"))?;
    Ok((stored_network.to_ne_bytes(), prefix))
}

fn decode_route_value(value: Option<&Value>) -> Result<u32, String> {
    if value.is_some_and(Value::is_array) {
        let bytes = decode_bpftool_bytes(value)?;
        let bytes = <[u8; 4]>::try_from(bytes.as_slice())
            .map_err(|_| format!("bpftool route value had {} bytes", bytes.len()))?;
        return Ok(u32::from_ne_bytes(bytes));
    }
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| format!("bpftool route value was neither raw nor typed: {value:?}"))?;
    json_u32(object.get("ifindex"))
}

fn json_u32(value: Option<&Value>) -> Result<u32, String> {
    let number = value
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("bpftool typed field was not an integer: {value:?}"))?;
    u32::try_from(number).map_err(|error| error.to_string())
}

fn decode_bpftool_bytes(value: Option<&Value>) -> Result<Vec<u8>, String> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("bpftool did not return a byte array: {value:?}"))?;
    values
        .iter()
        .map(|value| {
            if let Some(number) = value.as_u64() {
                return u8::try_from(number).map_err(|error| error.to_string());
            }
            let text = value
                .as_str()
                .ok_or_else(|| format!("bpftool byte was neither integer nor text: {value}"))?;
            u8::from_str_radix(text.trim_start_matches("0x"), 16)
                .map_err(|error| format!("parse bpftool byte {text:?}: {error}"))
        })
        .collect()
}

async fn assert_status_ownership(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "two correctly-owned machine_status rows",
        || corrosion_curl_command("v1/queries", &machine_status_query()),
        |outcome| {
            outcome.success()
                && query_status_rows(&outcome.stdout).is_ok_and(|rows| status_rows_are_owned(&rows))
        },
    )
    .await
}

fn status_rows_are_owned(rows: &[(String, Value)]) -> bool {
    if rows.len() != 2 {
        return false;
    }
    rows.iter().all(|(id, document)| {
        if document.get("machine_id").and_then(Value::as_str) != Some(id.as_str()) {
            return false;
        }
        document
            .get("ployz_version")
            .and_then(Value::as_str)
            .is_some()
            && document.get("corrosion_version").and_then(Value::as_str) == Some(CORROSION_VERSION)
            && document.get("mesh").is_some()
    })
}

fn query_status_rows(output: &str) -> Result<Vec<(String, Value)>, String> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|frame| frame.get("row").and_then(Value::as_array).cloned())
        .map(|row| {
            let values = row
                .get(1)
                .and_then(Value::as_array)
                .ok_or_else(|| format!("machine_status row had invalid values: {row:?}"))?;
            let id = values
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| format!("machine_status row omitted key: {values:?}"))?;
            let document = values
                .get(1)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("machine_status row omitted document: {values:?}"))?;
            let document = serde_json::from_str(document)
                .map_err(|error| format!("decode machine_status {id}: {error}"))?;
            Ok((id.to_owned(), document))
        })
        .collect()
}

async fn wait_for_peer_absent(
    docker: &Docker,
    machine: &DindMachine,
    removed_key: &WireGuardPublicKey,
) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "removed WireGuard peer",
        || {
            vec![
                "wg".to_owned(),
                "show".to_owned(),
                WIREGUARD_INTERFACE.to_owned(),
                "dump".to_owned(),
            ]
        },
        |outcome| outcome.success() && !outcome.stdout.contains(removed_key.as_str()),
    )
    .await
}

async fn wait_for_route_absent(
    docker: &Docker,
    machine: &DindMachine,
    removed_subnet: &str,
) -> Result<(), String> {
    let family = if removed_subnet.contains(':') {
        "-6"
    } else {
        "-4"
    };
    wait_for_command(
        docker,
        machine,
        "removed kernel route",
        || {
            vec![
                "ip".to_owned(),
                family.to_owned(),
                "route".to_owned(),
                "show".to_owned(),
                removed_subnet.to_owned(),
            ]
        },
        |outcome| outcome.success() && outcome.stdout.trim().is_empty(),
    )
    .await
}

async fn wait_for_empty_route_map(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "empty eBPF route map",
        || {
            vec![
                "/opt/ployz/artifacts/bpftool".to_owned(),
                "-j".to_owned(),
                "map".to_owned(),
                "dump".to_owned(),
                "pinned".to_owned(),
                "/sys/fs/bpf/ployz/routes".to_owned(),
            ]
        },
        |outcome| {
            outcome.success()
                && serde_json::from_str::<Value>(&outcome.stdout)
                    .is_ok_and(|value| value.as_array().is_some_and(Vec::is_empty))
        },
    )
    .await
}

async fn corrosion_row_is_absent(
    docker: &Docker,
    machine: &DindMachine,
    table: &str,
    id: &str,
) -> Result<bool, String> {
    let statement = json!([format!("SELECT id FROM {table} WHERE id = ?"), [id]]);
    let command = corrosion_curl_command("v1/queries", &statement);
    let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
    let outcome = exec_ok(docker, machine, &refs).await?;
    Ok(!outcome.stdout.contains(id))
}

async fn wait_for_command<Command, Predicate>(
    docker: &Docker,
    machine: &DindMachine,
    description: &str,
    mut command: Command,
    mut predicate: Predicate,
) -> Result<(), String>
where
    Command: FnMut() -> Vec<String>,
    Predicate: FnMut(&ExecOutcome) -> bool,
{
    let deadline = Instant::now() + WAIT_BUDGET;
    let mut last = String::from("command was not attempted");
    while Instant::now() < deadline {
        let command = command();
        let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
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

async fn exec_ok(
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

fn render_failure(outcome: &ExecOutcome) -> String {
    format!(
        "exit {} stdout={:?} stderr={:?}",
        outcome.exit_code,
        outcome.stdout.trim(),
        outcome.stderr.trim()
    )
}

#[test]
fn bpftool_byte_decoder_accepts_raw_json_forms() {
    assert_eq!(
        decode_bpftool_bytes(Some(&json!([
            "0a", "d2", "14", "00", "18", "00", "00", "00"
        ])))
        .expect("hex bytes"),
        [10, 210, 20, 0, 24, 0, 0, 0]
    );
    assert_eq!(
        decode_bpftool_bytes(Some(&json!([10, 210, 20, 0]))).expect("integer bytes"),
        [10, 210, 20, 0]
    );
    assert_eq!(
        decode_route_key(Some(&json!({"network": 1_364_490, "prefix_len": 24})))
            .expect("typed key"),
        ([10, 210, 20, 0], 24)
    );
    assert_eq!(
        decode_route_value(Some(&json!({"ifindex": 7}))).expect("typed value"),
        7
    );
}

#[test]
fn peer_dump_requires_exactly_one_peer() {
    let dump = "private\tpublic\t51820\toff\nremote\tpsk\t192.0.2.1:51820\tfd00::1/112,10.210.20.0/24\t42\t1\t2\toff\n";
    let peer = parse_peer_dump(dump).expect("one peer");
    assert_eq!(peer.public_key, "remote");
    assert_eq!(peer.allowed_ips, "fd00::1/112,10.210.20.0/24");
    assert_eq!(peer.latest_handshake, 42);
}

#[test]
fn deterministic_cluster_vector_stays_fixed() {
    let cluster_id = ClusterId::try_new(CLUSTER_ID).expect("cluster id");
    let public_key = WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        .expect("public key");
    let identity = derive_builtin_wireguard_member(&cluster_id, &public_key);
    assert_eq!(
        identity.bind_address().get(),
        Ipv6Addr::from_str("fd8e:ac53:b3f1:6668:7aad:f862:bd77:1").expect("IPv6")
    );
}

#[test]
fn derived_member_route_uses_the_canonical_subnet() {
    let public_key = WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        .expect("public key");
    assert_eq!(
        derived_subnet(&public_key).expect("derived subnet"),
        "fd8e:ac53:b3f1:6668:7aad:f862:bd77:0/112"
    );
}

#[test]
fn mesh_status_matching_is_structural() {
    let degraded = json!({
        "mesh": {
            "state": "degraded",
            "bind_address": "fd00::1",
            "attempted_at": "2026-08-04T10:00:00Z",
            "last_successful_converge": null,
            "degradation": {
                "components": "ebpf",
                "wireguard": {"converged_at": "2026-08-04T10:00:00Z"},
                "ebpf": {"reason": {"kind": "missing_bridge", "ifname": TEST_BRIDGE_INTERFACE}}
            }
        }
    });
    assert!(mesh_status_matches(
        &degraded,
        MeshStatusExpectation::BridgeMissing
    ));
    assert!(!mesh_status_matches(
        &degraded,
        MeshStatusExpectation::Ready
    ));

    let converged = json!({
        "mesh": {
            "state": "converged",
            "bind_address": "fd00::1",
            "attempted_at": "2026-08-04T10:01:00Z",
            "last_successful_converge": "2026-08-04T10:01:00Z",
            "wireguard": {"converged_at": "2026-08-04T10:01:00Z"},
            "ebpf": {"converged_at": "2026-08-04T10:01:00Z"}
        }
    });
    assert!(mesh_status_matches(
        &converged,
        MeshStatusExpectation::Ready
    ));
}

#[test]
fn allowed_ips_comparison_is_order_independent_and_exact() {
    assert!(allowed_ips_match(
        "10.210.20.0/24,fd00::/112",
        "fd00::/112",
        "10.210.20.0/24"
    ));
    assert!(!allowed_ips_match(
        "fd00::/112,10.210.20.0/24,10.210.30.0/24",
        "fd00::/112",
        "10.210.20.0/24"
    ));
}

#[test]
fn corrosion_query_uses_the_simple_statement_wire_shape() {
    let command = corrosion_curl_command("v1/queries", &machine_status_query());
    let body_index = command
        .iter()
        .position(|part| part == "--data-binary")
        .and_then(|index| index.checked_add(1))
        .expect("curl body argument");
    assert_eq!(
        command.get(body_index).map(String::as_str),
        Some("\"SELECT machine_id AS id, document FROM machine_status ORDER BY machine_id\"")
    );
}

#[test]
fn endpoint_rejects_an_ipv6_outer_network() {
    let machine = DindMachine {
        name: "bad-outer-network".to_owned(),
        container_id: "container".to_owned(),
        bridge_ip: IpAddr::V6(Ipv6Addr::LOCALHOST),
    };
    assert!(endpoint(&machine).is_err());
    let machine = DindMachine {
        name: "outer-network".to_owned(),
        container_id: "container".to_owned(),
        bridge_ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
    };
    assert_eq!(
        endpoint(&machine).expect("IPv4 endpoint"),
        "192.0.2.10:51820"
    );
}

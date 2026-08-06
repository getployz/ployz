use bollard::Docker;
use ployz_core::network::WireGuardPublicKey;
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, DindMachine, MachineSpec, artifact_dir,
    assert_keeper_isolation_root, connect_docker, e2e_enabled, keep_requested, machine_image,
    shell_quote,
};
use serde_json::{Value, json};
use std::net::Ipv6Addr;

#[path = "keeper_mesh/support.rs"]
mod support;
use support::*;

#[path = "keeper_mesh/diagnostics.rs"]
mod diagnostics;

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
    activate_default_deny_firewall(docker, machine_a).await?;
    activate_default_deny_firewall(docker, machine_b).await?;

    install_keeper_unit(docker, machine_a, MACHINE_A_ID, TEST_BRIDGE_INTERFACE).await?;
    install_keeper_unit(docker, machine_b, MACHINE_B_ID, TEST_BRIDGE_INTERFACE).await?;
    start_unit(docker, machine_a, "ployz-keeper.service").await?;
    start_unit(docker, machine_b, "ployz-keeper.service").await?;
    assert_keeper_isolation_root(docker, machine_a, "ployz-keeper.service").await?;
    assert_keeper_isolation_root(docker, machine_b, "ployz-keeper.service").await?;

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
    let subnet_a = derived_subnet(&public_key_a)?;
    let subnet_b = derived_subnet(&public_key_b)?;
    assert_product_configured_mesh_firewall(docker, machine_a, [&subnet_a, &subnet_b]).await?;
    assert_product_configured_mesh_firewall(docker, machine_b, [&subnet_a, &subnet_b]).await?;

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

    create_test_bridge(docker, machine_a, "10.210.10.1/24").await?;
    create_test_bridge(docker, machine_b, "10.210.20.1/24").await?;
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
    diagnostics::wait_for_absolute_handshake(docker, machine_b, MACHINE_B_ID, MACHINE_A_ID).await?;
    diagnostics::wait_for_public_status_handshake(docker, machine_a, address_b, MACHINE_A_ID)
        .await?;

    let delete_b = json!([["DELETE FROM machines WHERE id = ?", [MACHINE_B_ID]]]);
    corrosion_transaction(docker, machine_a, &delete_b).await?;
    wait_for_corrosion_row_absent(docker, machine_b, "machines", MACHINE_B_ID).await?;
    wait_for_peer_absent(docker, machine_a, &public_key_b).await?;
    wait_for_route_absent(docker, machine_a, "10.210.20.0/24").await?;
    wait_for_route_absent(docker, machine_a, &subnet_b).await?;
    wait_for_empty_route_map(docker, machine_a).await?;

    wait_for_peer_absent(docker, machine_b, &public_key_a).await?;
    wait_for_route_absent(docker, machine_b, "10.210.10.0/24").await?;
    wait_for_route_absent(docker, machine_b, &subnet_a).await?;
    wait_for_empty_route_map(docker, machine_b).await?;

    let mismatched_address = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
    corrosion_transaction(
        docker,
        machine_a,
        &mismatched_machine_a_transaction(machine_a, &public_key_a, mismatched_address)?,
    )
    .await?;
    wait_for_mesh_status(
        docker,
        machine_a,
        MACHINE_A_ID,
        MeshStatusExpectation::KeyMismatch,
    )
    .await?;
    Ok(())
}

fn mismatched_machine_a_transaction(
    machine_a: &DindMachine,
    public_key_a: &WireGuardPublicKey,
    mismatched_address: Ipv6Addr,
) -> Result<Value, String> {
    let document = machine_document(
        "edge-a",
        MACHINE_A_ID,
        public_key_a,
        mismatched_address,
        endpoint(machine_a)?,
        "10.210.10.0/24",
        "2026-08-04T10:02:00.000000000Z",
    );
    Ok(json!([[
        "UPDATE machines SET document = ? WHERE id = ?",
        [
            serde_json::to_string(&document).map_err(|error| error.to_string())?,
            MACHINE_A_ID
        ]
    ]]))
}

fn machine_document(
    name: &str,
    id: &str,
    public_key: &WireGuardPublicKey,
    address: Ipv6Addr,
    endpoint: String,
    subnet: &str,
    written_at: &str,
) -> Value {
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
        "written_at": written_at
    })
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
    let machine_a_document = machine_document(
        "edge-a",
        MACHINE_A_ID,
        public_key_a,
        address_a,
        endpoint_a,
        "10.210.10.0/24",
        "2026-08-04T10:00:00.000000000Z",
    );
    let machine_b_document = machine_document(
        "edge-b",
        MACHINE_B_ID,
        public_key_b,
        address_b,
        endpoint_b,
        "10.210.20.0/24",
        "2026-08-04T10:00:00.000000000Z",
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
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("{ping} >/dev/null 2>&1 || true; wg show {WIREGUARD_INTERFACE} dump"),
        ],
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
        vec![
            "curl".to_owned(),
            "--fail".to_owned(),
            "--silent".to_owned(),
            "--show-error".to_owned(),
            "--max-time".to_owned(),
            "3".to_owned(),
            "--noproxy".to_owned(),
            "*".to_owned(),
            url,
        ],
        |outcome| outcome.success() && outcome.stdout.contains(PLOYZ_BUILD),
    )
    .await
}

#[derive(Clone, Copy)]
enum MeshStatusExpectation {
    BridgeMissing,
    KeyMismatch,
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
        corrosion_curl_command("v1/queries", &machine_status_query()),
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
        MeshStatusExpectation::KeyMismatch => {
            mesh.get("state").and_then(Value::as_str) == Some("key_mismatch")
                && mesh.get("attempted_at").and_then(Value::as_str).is_some()
                && mesh
                    .get("mismatches")
                    .and_then(Value::as_array)
                    .is_some_and(|mismatches| !mismatches.is_empty())
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

async fn create_test_bridge(
    docker: &Docker,
    machine: &DindMachine,
    gateway_cidr: &str,
) -> Result<(), String> {
    let command = test_bridge_command(TEST_BRIDGE_INTERFACE, gateway_cidr);
    exec_ok(docker, machine, &["sh", "-c", &command]).await?;
    Ok(())
}

fn test_bridge_command(interface: &str, gateway_cidr: &str) -> String {
    format!(
        "ip link show {} >/dev/null 2>&1 || ip link add {} type bridge; ip address replace {} dev {}; ip link set {} up",
        shell_quote(interface),
        shell_quote(interface),
        shell_quote(gateway_cidr),
        shell_quote(interface),
        shell_quote(interface)
    )
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
            "bpftool",
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
        corrosion_curl_command("v1/queries", &machine_status_query()),
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
        document.get("ployz_version").and_then(Value::as_str) == Some(env!("CARGO_PKG_VERSION"))
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
        vec![
            "wg".to_owned(),
            "show".to_owned(),
            WIREGUARD_INTERFACE.to_owned(),
            "dump".to_owned(),
        ],
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
        vec![
            "ip".to_owned(),
            family.to_owned(),
            "route".to_owned(),
            "show".to_owned(),
            removed_subnet.to_owned(),
        ],
        |outcome| outcome.success() && outcome.stdout.trim().is_empty(),
    )
    .await
}

async fn wait_for_empty_route_map(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "empty eBPF route map",
        vec![
            "bpftool".to_owned(),
            "-j".to_owned(),
            "map".to_owned(),
            "dump".to_owned(),
            "pinned".to_owned(),
            "/sys/fs/bpf/ployz/routes".to_owned(),
        ],
        |outcome| {
            outcome.success()
                && serde_json::from_str::<Value>(&outcome.stdout)
                    .is_ok_and(|value| value.as_array().is_some_and(Vec::is_empty))
        },
    )
    .await
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
fn synthetic_bridge_carries_the_machine_endpoint_gateway() {
    let command = test_bridge_command(TEST_BRIDGE_INTERFACE, "10.210.10.1/24");
    assert!(
        command.contains("ip address replace '10.210.10.1/24' dev 'ployz-test0'"),
        "{command}"
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

    let key_mismatch = json!({
        "mesh": {
            "state": "key_mismatch",
            "attempted_at": "2026-08-04T10:00:30Z",
            "mismatches": [{
                "kind": "stored_ipv6_address",
                "member_id": {"kind": "machine", "machine_id": MACHINE_A_ID},
                "stored": "fd00::1",
                "derived": "fd00::2"
            }]
        }
    });
    assert!(mesh_status_matches(
        &key_mismatch,
        MeshStatusExpectation::KeyMismatch
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

use bollard::Docker;
use ployz_core::network::WireGuardPublicKey;
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, DindMachine, MachineSpec, artifact_dir, connect_docker,
    e2e_enabled, exec_in_container, keep_requested, machine_image, write_file_in_container,
};
use serde_json::{Value, json};
use std::net::Ipv6Addr;
use std::time::Duration;

#[allow(dead_code)]
#[path = "keeper_mesh/support.rs"]
mod mesh_support;
use mesh_support::*;

const NAMESPACE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB4";
const SERVICE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB5";
const ACTIVE_DEPLOY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB6";
const INACTIVE_DEPLOY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB7";
const STALE_PROBE_NAMESPACE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB8";
const ACTIVE_IP_A: &str = "10.210.10.10";
const ACTIVE_IP_B: &str = "10.210.20.10";
const INACTIVE_IP: &str = "10.210.10.11";
const ORIGINAL_SUBNET_A: &str = "10.210.10.0/24";
const ORIGINAL_SUBNET_B: &str = "10.210.20.0/24";
const REPLACEMENT_SUBNET_A: &str = "10.210.30.0/24";
const INTERNAL_NAME: &str = "web.production.internal";
const FORWARDED_INTERNAL_NAME: &str = "metadata.compute.internal";
const FORWARDED_PUBLIC_NAME: &str = "outside.example";
const FORWARDED_IP: &str = "203.0.113.53";
const DNS_QUERY_PATH: &str = "/usr/local/bin/ployz-test-dns-query";
const UPSTREAM_PATH: &str = "/usr/local/bin/ployz-test-dns-upstream";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_machine_container_plane_converges_network_and_service_dns() {
    if !e2e_enabled() {
        eprintln!("skipping container-plane DinD proof; set PLOYZ_DIND_E2E=1 to enable it");
        return;
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    panic!("the pinned Corrosion container-plane proof supports only Linux x86_64");

    let docker = connect_docker().expect("connect to Docker for container-plane proof");
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

    let result = exercise_container_plane(&docker, &cluster).await;
    if let Err(error) = &result {
        match cluster.capture_evidence().await {
            Ok(path) => eprintln!("container-plane evidence captured under {}", path.display()),
            Err(capture_error) => {
                eprintln!("container-plane evidence capture failed: {capture_error}");
            }
        }
        eprintln!("container-plane proof failed: {error}");
    }
    if keep_requested() {
        eprintln!(
            "retaining DinD run {} because PLOYZ_DIND_KEEP=1",
            cluster.run_id()
        );
    } else {
        cluster
            .teardown()
            .await
            .expect("tear down container-plane run");
    }
    result.unwrap_or_else(|error| panic!("container-plane proof failed: {error}"));
}

async fn exercise_container_plane(docker: &Docker, cluster: &DindCluster) -> Result<(), String> {
    let [machine_a, machine_b] = cluster.machines() else {
        return Err("container-plane proof requires exactly two machines".to_owned());
    };
    let mesh = start_two_machine_mesh(docker, machine_a, machine_b).await?;

    install_api_unit(docker, machine_a, MACHINE_A_ID, mesh.address_a).await?;
    install_api_unit(docker, machine_b, MACHINE_B_ID, mesh.address_b).await?;
    start_unit(docker, machine_a, "ployz-api.service").await?;
    start_unit(docker, machine_b, "ployz-api.service").await?;
    wait_for_api(docker, machine_a, mesh.address_a).await?;
    wait_for_api(docker, machine_b, mesh.address_b).await?;
    wait_for_exact_endpoint_network(docker, machine_a, ORIGINAL_SUBNET_A).await?;
    wait_for_exact_endpoint_network(docker, machine_b, ORIGINAL_SUBNET_B).await?;

    install_dns_test_tools(docker, machine_a).await?;
    install_dns_test_tools(docker, machine_b).await?;
    install_dns_unit(docker, machine_a, MACHINE_A_ID).await?;
    install_dns_unit(docker, machine_b, MACHINE_B_ID).await?;
    start_unit(docker, machine_a, "ployzd-dns.service").await?;
    start_unit(docker, machine_b, "ployzd-dns.service").await?;
    assert_dns_hardening(docker, machine_a).await?;
    assert_dns_hardening(docker, machine_b).await?;

    corrosion_transaction(docker, machine_a, &service_rows_transaction()?).await?;
    wait_for_corrosion_row(docker, machine_b, "containers", "active-container-b").await?;
    let expected = [ACTIVE_IP_A, ACTIVE_IP_B];
    wait_for_dns_answers(docker, machine_a, "10.210.10.1", INTERNAL_NAME, &expected).await?;
    wait_for_dns_answers(docker, machine_b, "10.210.20.1", INTERNAL_NAME, &expected).await?;
    assert_dns_answers(
        docker,
        machine_a,
        "10.210.10.1",
        FORWARDED_PUBLIC_NAME,
        &[FORWARDED_IP],
    )
    .await?;
    assert_dns_answers(
        docker,
        machine_b,
        "10.210.20.1",
        FORWARDED_PUBLIC_NAME,
        &[FORWARDED_IP],
    )
    .await?;
    assert_dns_answers(
        docker,
        machine_a,
        "10.210.10.1",
        FORWARDED_INTERNAL_NAME,
        &[FORWARDED_IP],
    )
    .await?;
    assert_dns_answers(
        docker,
        machine_b,
        "10.210.20.1",
        FORWARDED_INTERNAL_NAME,
        &[FORWARDED_IP],
    )
    .await?;

    make_machine_dark(docker, machine_a, machine_b, mesh.address_b).await?;
    corrosion_transaction(docker, machine_a, &stale_probe_transaction()?).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_corrosion_row_absent_now(docker, machine_b, "namespaces", STALE_PROBE_NAMESPACE_ID)
        .await?;
    wait_for_corrosion_row(docker, machine_a, "machines", MACHINE_B_ID).await?;
    wait_for_corrosion_row(docker, machine_a, "containers", "active-container-b").await?;
    assert_dns_answers(docker, machine_a, "10.210.10.1", INTERNAL_NAME, &expected).await?;

    let started_before = start_managed_nginx(docker, machine_a).await?;
    corrosion_transaction(
        docker,
        machine_a,
        &replace_machine_subnet_transaction(machine_a, &mesh.public_key_a, mesh.address_a)?,
    )
    .await?;
    wait_for_exact_endpoint_network(docker, machine_a, REPLACEMENT_SUBNET_A).await?;
    wait_for_container_restart(docker, machine_a, &started_before).await?;
    wait_for_dns_answers(docker, machine_a, "10.210.30.1", INTERNAL_NAME, &expected).await?;
    Ok(())
}

struct MeshFixture {
    public_key_a: WireGuardPublicKey,
    address_a: Ipv6Addr,
    address_b: Ipv6Addr,
}

async fn start_two_machine_mesh(
    docker: &Docker,
    machine_a: &DindMachine,
    machine_b: &DindMachine,
) -> Result<MeshFixture, String> {
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
    wait_for_peer_ping(docker, machine_a, address_b).await?;
    wait_for_peer_ping(docker, machine_b, address_a).await?;

    Ok(MeshFixture {
        public_key_a,
        address_a,
        address_b,
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
    let cluster = json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "name": "container-plane-dind",
        "storage_default": "plain",
        "hostname_mode": {"mode": "disabled"},
        "prefix": "10.210.0.0/16",
        "provider": "builtin_wireguard",
        "acme_directory_url": "https://acme.invalid/directory",
        "acme_contact": null,
        "written_by": {"kind": "machine", "machine_id": MACHINE_A_ID},
        "written_at": "2026-08-05T10:00:00.000000000Z"
    });
    let machine_a_document = machine_document(
        "edge-a",
        MACHINE_A_ID,
        public_key_a,
        address_a,
        endpoint(machine_a)?,
        ORIGINAL_SUBNET_A,
        "2026-08-05T10:00:00.000000000Z",
    );
    let machine_b_document = machine_document(
        "edge-b",
        MACHINE_B_ID,
        public_key_b,
        address_b,
        endpoint(machine_b)?,
        ORIGINAL_SUBNET_B,
        "2026-08-05T10:00:00.000000000Z",
    );
    Ok(json!([
        [
            "INSERT INTO cluster (id, document) VALUES (?, ?)",
            [CLUSTER_ID, encode_document(&cluster)?]
        ],
        [
            "INSERT INTO machines (id, document) VALUES (?, ?)",
            [MACHINE_A_ID, encode_document(&machine_a_document)?]
        ],
        [
            "INSERT INTO machines (id, document) VALUES (?, ?)",
            [MACHINE_B_ID, encode_document(&machine_b_document)?]
        ]
    ]))
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

fn service_rows_transaction() -> Result<Value, String> {
    let namespace = json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "name": "production",
        "written_by": {"kind": "machine", "machine_id": MACHINE_A_ID},
        "written_at": "2026-08-05T10:01:00.000000000Z"
    });
    let service = json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "namespace_id": NAMESPACE_ID,
        "name": "web",
        "image": "nginx:1.27-alpine",
        "env_fingerprints": {},
        "mode": "replicated",
        "replicas": 2,
        "pinned_machines": [],
        "active_deploy": ACTIVE_DEPLOY_ID,
        "previous_image": null,
        "deployed_at": "2026-08-05T10:01:00.000000000Z",
        "operation_id": ACTIVE_DEPLOY_ID,
        "written_by": {"kind": "machine", "machine_id": MACHINE_A_ID},
        "written_at": "2026-08-05T10:01:00.000000000Z"
    });
    let active_a = container_document(MACHINE_A_ID, ACTIVE_IP_A, ACTIVE_DEPLOY_ID);
    let active_b = container_document(MACHINE_B_ID, ACTIVE_IP_B, ACTIVE_DEPLOY_ID);
    let inactive = container_document(MACHINE_A_ID, INACTIVE_IP, INACTIVE_DEPLOY_ID);
    Ok(json!([
        [
            "INSERT INTO namespaces (id, document) VALUES (?, ?)",
            [NAMESPACE_ID, encode_document(&namespace)?]
        ],
        [
            "INSERT INTO services (id, document) VALUES (?, ?)",
            [SERVICE_ID, encode_document(&service)?]
        ],
        [
            "INSERT INTO containers (id, document) VALUES (?, ?)",
            ["active-container-a", encode_document(&active_a)?]
        ],
        [
            "INSERT INTO containers (id, document) VALUES (?, ?)",
            ["active-container-b", encode_document(&active_b)?]
        ],
        [
            "INSERT INTO containers (id, document) VALUES (?, ?)",
            ["inactive-container", encode_document(&inactive)?]
        ]
    ]))
}

fn container_document(machine_id: &str, ip: &str, deploy: &str) -> Value {
    json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "machine_id": machine_id,
        "service_id": SERVICE_ID,
        "namespace_id": NAMESPACE_ID,
        "ip": ip,
        "deploy": deploy
    })
}

fn stale_probe_transaction() -> Result<Value, String> {
    let document = json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "name": "darkness-probe",
        "written_by": {"kind": "machine", "machine_id": MACHINE_A_ID},
        "written_at": "2026-08-05T10:02:00.000000000Z"
    });
    Ok(json!([[
        "INSERT INTO namespaces (id, document) VALUES (?, ?)",
        [STALE_PROBE_NAMESPACE_ID, encode_document(&document)?]
    ]]))
}

fn replace_machine_subnet_transaction(
    machine: &DindMachine,
    public_key: &WireGuardPublicKey,
    address: Ipv6Addr,
) -> Result<Value, String> {
    let document = machine_document(
        "edge-a",
        MACHINE_A_ID,
        public_key,
        address,
        endpoint(machine)?,
        REPLACEMENT_SUBNET_A,
        "2026-08-05T10:03:00.000000000Z",
    );
    Ok(json!([[
        "UPDATE machines SET document = ? WHERE id = ?",
        [encode_document(&document)?, MACHINE_A_ID]
    ]]))
}

fn encode_document(document: &Value) -> Result<String, String> {
    serde_json::to_string(document).map_err(|error| error.to_string())
}

async fn wait_for_peer_ping(
    docker: &Docker,
    machine: &DindMachine,
    address: Ipv6Addr,
) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "WireGuard peer connectivity",
        vec![
            "ping".to_owned(),
            "-6".to_owned(),
            "-c".to_owned(),
            "1".to_owned(),
            "-W".to_owned(),
            "1".to_owned(),
            address.to_string(),
        ],
        |outcome| outcome.success(),
    )
    .await
}

async fn wait_for_api(
    docker: &Docker,
    machine: &DindMachine,
    address: Ipv6Addr,
) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "shipped API role",
        vec![
            "curl".to_owned(),
            "--fail".to_owned(),
            "--silent".to_owned(),
            "--show-error".to_owned(),
            "--max-time".to_owned(),
            "3".to_owned(),
            "--noproxy".to_owned(),
            "*".to_owned(),
            format!("http://[{address}]:{API_PORT}/version"),
        ],
        |outcome| outcome.success() && outcome.stdout.contains(PLOYZ_BUILD),
    )
    .await
}

async fn wait_for_exact_endpoint_network(
    docker: &Docker,
    machine: &DindMachine,
    subnet: &str,
) -> Result<(), String> {
    let expected = subnet.to_owned();
    wait_for_command(
        docker,
        machine,
        "exact API-owned ployz network",
        vec![
            "docker".to_owned(),
            "network".to_owned(),
            "inspect".to_owned(),
            "ployz".to_owned(),
        ],
        move |outcome| {
            outcome.success()
                && serde_json::from_str::<Value>(&outcome.stdout)
                    .ok()
                    .is_some_and(|value| exact_endpoint_network(&value, &expected))
        },
    )
    .await
}

fn exact_endpoint_network(value: &Value, expected_subnet: &str) -> bool {
    let Some([network]) = value.as_array().map(Vec::as_slice) else {
        return false;
    };
    network.get("Name").and_then(Value::as_str) == Some("ployz")
        && network.get("Driver").and_then(Value::as_str) == Some("bridge")
        && network.pointer("/Labels/plz.managed").and_then(Value::as_str) == Some("true")
        && network
            .pointer("/Options/com.docker.network.bridge.name")
            .and_then(Value::as_str)
            == Some("br-ployz")
        && network
            .pointer("/Options/com.docker.network.driver.mtu")
            .and_then(Value::as_str)
            == Some("1420")
        && network
            .pointer("/IPAM/Config")
            .and_then(Value::as_array)
            .is_some_and(|configs| {
                matches!(configs.as_slice(), [config] if config.get("Subnet").and_then(Value::as_str) == Some(expected_subnet))
            })
}

async fn install_dns_test_tools(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    write_file_in_container(
        docker,
        &machine.container_id,
        DNS_QUERY_PATH,
        DNS_QUERY_SCRIPT,
        "0755",
    )
    .await
    .map_err(|error| error.to_string())?;
    write_file_in_container(
        docker,
        &machine.container_id,
        UPSTREAM_PATH,
        DNS_UPSTREAM_SCRIPT,
        "0755",
    )
    .await
    .map_err(|error| error.to_string())?;
    let upstream_unit = format!(
        "[Unit]\nDescription=Controlled DNS upstream\n\n[Service]\nType=simple\nExecStart={UPSTREAM_PATH}\nRestart=on-failure\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    write_file_in_container(
        docker,
        &machine.container_id,
        "/etc/systemd/system/ployz-test-dns-upstream.service",
        &upstream_unit,
        "0644",
    )
    .await
    .map_err(|error| error.to_string())?;
    exec_ok(
        docker,
        machine,
        &[
            "sh",
            "-c",
            "printf 'nameserver 127.0.0.1\\n' > /etc/resolv.conf",
        ],
    )
    .await?;
    start_unit(docker, machine, "ployz-test-dns-upstream.service").await
}

async fn install_dns_unit(
    docker: &Docker,
    machine: &DindMachine,
    machine_id: &str,
) -> Result<(), String> {
    let unit = format!(
        "[Unit]\nDescription=Ployz DNS container-plane test\nAfter=corrosion.service ployz-api.service ployz-test-dns-upstream.service\n\n[Service]\nType=simple\nDynamicUser=yes\nUser=ployz-dns\nAmbientCapabilities=CAP_NET_BIND_SERVICE\nCapabilityBoundingSet=CAP_NET_BIND_SERVICE\nNoNewPrivileges=yes\nEnvironment=PLOYZ_CORROSION_API_ADDR=127.0.0.1:{CORROSION_API_PORT}\nEnvironment=PLOYZ_CORROSION_BEARER_TOKEN={CORROSION_TOKEN}\nEnvironment=PLOYZ_CLUSTER_ID={CLUSTER_ID}\nEnvironment=PLOYZ_MACHINE_ID={machine_id}\nEnvironment=PLOYZ_LOG=debug\nExecStart=/opt/ployz/artifacts/ployzd dns\nRestart=on-failure\nRestartSec=250ms\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    write_file_in_container(
        docker,
        &machine.container_id,
        "/etc/systemd/system/ployzd-dns.service",
        &unit,
        "0644",
    )
    .await
    .map_err(|error| error.to_string())
}

async fn assert_dns_hardening(docker: &Docker, machine: &DindMachine) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "unprivileged DNS role with only port-53 capability",
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "systemctl is-active --quiet ployzd-dns.service && properties=$(systemctl show ployzd-dns.service -p DynamicUser -p User -p AmbientCapabilities -p CapabilityBoundingSet -p NoNewPrivileges); printf '%s\\n' \"$properties\" | grep -Fx 'DynamicUser=yes' && printf '%s\\n' \"$properties\" | grep -Fx 'User=ployz-dns' && printf '%s\\n' \"$properties\" | grep -Fx 'NoNewPrivileges=yes' && printf '%s\\n' \"$properties\" | grep -Eix 'AmbientCapabilities=cap_net_bind_service' && printf '%s\\n' \"$properties\" | grep -Eix 'CapabilityBoundingSet=cap_net_bind_service'".to_owned(),
        ],
        |outcome| outcome.success(),
    )
    .await
}

async fn wait_for_dns_answers(
    docker: &Docker,
    machine: &DindMachine,
    resolver: &str,
    name: &str,
    expected: &[&str],
) -> Result<(), String> {
    let expected = expected.join("\n");
    wait_for_command(
        docker,
        machine,
        "cluster-wide active-only DNS answers",
        vec![
            DNS_QUERY_PATH.to_owned(),
            resolver.to_owned(),
            name.to_owned(),
        ],
        move |outcome| outcome.success() && outcome.stdout.trim() == expected,
    )
    .await
}

async fn assert_dns_answers(
    docker: &Docker,
    machine: &DindMachine,
    resolver: &str,
    name: &str,
    expected: &[&str],
) -> Result<(), String> {
    let outcome = exec_ok(docker, machine, &[DNS_QUERY_PATH, resolver, name]).await?;
    let actual = outcome.stdout.trim();
    let expected = expected.join("\n");
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{} DNS query {name:?} via {resolver} returned {actual:?}, expected {expected:?}",
            machine.name
        ))
    }
}

async fn make_machine_dark(
    docker: &Docker,
    survivor: &DindMachine,
    dark_machine: &DindMachine,
    address: Ipv6Addr,
) -> Result<(), String> {
    exec_ok(
        docker,
        dark_machine,
        &["systemctl", "stop", "ployz-keeper.service"],
    )
    .await?;
    exec_ok(
        docker,
        dark_machine,
        &["ip", "link", "set", WIREGUARD_INTERFACE, "down"],
    )
    .await?;
    let ping = exec_in_container(
        docker,
        &survivor.container_id,
        &["ping", "-6", "-c", "1", "-W", "1", &address.to_string()],
    )
    .await
    .map_err(|error| error.to_string())?;
    if ping.success() {
        return Err(format!(
            "{} remained reachable after its WireGuard interface went dark",
            dark_machine.name
        ));
    }
    Ok(())
}

async fn assert_corrosion_row_absent_now(
    docker: &Docker,
    machine: &DindMachine,
    table: &str,
    id: &str,
) -> Result<(), String> {
    let query = json!([format!("SELECT id FROM {table} WHERE id = ?"), [id]]);
    let command = corrosion_curl_command("v1/queries", &query);
    let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
    let outcome = exec_ok(docker, machine, &refs).await?;
    if outcome.stdout.contains(id) {
        Err(format!(
            "{} received post-darkness Corrosion row {table}/{id}",
            machine.name
        ))
    } else {
        Ok(())
    }
}

async fn start_managed_nginx(docker: &Docker, machine: &DindMachine) -> Result<String, String> {
    wait_for_command(
        docker,
        machine,
        "preloaded nginx workload image",
        vec![
            "docker".to_owned(),
            "image".to_owned(),
            "inspect".to_owned(),
            "nginx:1.27-alpine".to_owned(),
        ],
        |outcome| outcome.success(),
    )
    .await?;
    exec_ok(
        docker,
        machine,
        &[
            "docker",
            "run",
            "--detach",
            "--name",
            "ployz-restart-proof",
            "--network",
            "ployz",
            "--label",
            "plz.managed=true",
            "nginx:1.27-alpine",
        ],
    )
    .await?;
    container_started_at(docker, machine).await
}

async fn container_started_at(docker: &Docker, machine: &DindMachine) -> Result<String, String> {
    Ok(exec_ok(
        docker,
        machine,
        &[
            "docker",
            "inspect",
            "--format",
            "{{.State.StartedAt}}",
            "ployz-restart-proof",
        ],
    )
    .await?
    .stdout
    .trim()
    .to_owned())
}

async fn wait_for_container_restart(
    docker: &Docker,
    machine: &DindMachine,
    started_before: &str,
) -> Result<(), String> {
    let started_before = started_before.to_owned();
    wait_for_command(
        docker,
        machine,
        "managed container restart after subnet replacement",
        vec![
            "docker".to_owned(),
            "inspect".to_owned(),
            "--format".to_owned(),
            "{{.State.StartedAt}}".to_owned(),
            "ployz-restart-proof".to_owned(),
        ],
        move |outcome| outcome.success() && outcome.stdout.trim() != started_before,
    )
    .await
}

const DNS_QUERY_SCRIPT: &str = r#"#!/usr/bin/python3
import ipaddress
import socket
import struct
import sys

resolver, name = sys.argv[1:]
labels = name.rstrip(".").split(".")
question = b"".join(bytes([len(label)]) + label.encode("ascii") for label in labels) + b"\0"
query = struct.pack("!HHHHHH", 0x8150, 0x0100, 1, 0, 0, 0) + question + struct.pack("!HH", 1, 1)
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.settimeout(3)
sock.sendto(query, (resolver, 53))
response = sock.recv(4096)
transaction_id, flags, questions, answers, _, _ = struct.unpack("!HHHHHH", response[:12])
if transaction_id != 0x8150 or not flags & 0x8000 or flags & 0x000f or questions != 1:
    raise SystemExit("unsuccessful DNS response")
offset = 12
while response[offset] != 0:
    offset += response[offset] + 1
offset += 5
addresses = []
for _ in range(answers):
    if response[offset] & 0xc0 == 0xc0:
        offset += 2
    else:
        while response[offset] != 0:
            offset += response[offset] + 1
        offset += 1
    kind, dns_class, _, length = struct.unpack("!HHIH", response[offset:offset + 10])
    offset += 10
    data = response[offset:offset + length]
    offset += length
    if kind == 1 and dns_class == 1 and length == 4:
        addresses.append(str(ipaddress.ip_address(data)))
for address in sorted(set(addresses), key=ipaddress.ip_address):
    print(address)
"#;

const DNS_UPSTREAM_SCRIPT: &str = r#"#!/usr/bin/python3
import ipaddress
import socket
import struct

answer_ip = ipaddress.ip_address("203.0.113.53").packed
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind(("127.0.0.1", 53))
while True:
    query, peer = sock.recvfrom(4096)
    if len(query) < 12:
        continue
    transaction_id = query[:2]
    response = transaction_id + struct.pack("!HHHHH", 0x8180, 1, 1, 0, 0)
    response += query[12:]
    response += b"\xc0\x0c" + struct.pack("!HHIH", 1, 1, 5, 4) + answer_ip
    sock.sendto(response, peer)
"#;

use std::process::Command;
use std::time::Instant;

use serde::Serialize;

use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::three_server_harness::{
    HttpProbe, HttpsProbe, PebbleAcme, ProductCommandRecord, ProductHarness, ServingRoleProbe,
};

const SCENARIO: &str = "three-node-parity-smoke";
const NODES: &[&str] = &["node-a", "node-b", "node-c"];
const DOCKER_IMAGE: &str = "busybox:latest";
const DOCKER_SERVICE_PORT: u16 = 8080;
const DOCKER_COMMAND: &str =
    "mkdir -p /www && echo ok >/www/index.html && httpd -f -p 8080 -h /www";

#[derive(Debug, Serialize)]
struct ThreeNodeParityReport {
    scenario: &'static str,
    stage: &'static str,
    installed_node_bin: String,
    nodes: Vec<&'static str>,
    privileged_preflight: PrivilegedPreflight,
    installed_help: ProductCommandRecord,
    bootstrap_commands: Vec<ProductCommandRecord>,
    join_commands: Vec<ProductCommandRecord>,
    admit_commands: Vec<ProductCommandRecord>,
    daemon_statuses: Vec<NodeCommandStatus>,
    runtime_placements: Vec<RuntimePlacementEvidence>,
    gateway_http_checks: Vec<GatewayHttpEvidence>,
    acme_https_checks: Vec<GatewayHttpsEvidence>,
    gateway_statuses: Vec<NodeRoleStatus>,
    dns_statuses: Vec<NodeRoleStatus>,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct PrivilegedPreflight {
    enabled: bool,
    missing_prerequisites: Vec<String>,
    skipped_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct NodeCommandStatus {
    node: &'static str,
    status: ProductCommandRecord,
}

#[derive(Debug, Serialize)]
struct NodeRoleStatus {
    node: &'static str,
    status: ServingRoleProbe,
}

#[derive(Debug, Serialize)]
struct RuntimePlacementEvidence {
    service: &'static str,
    target_node: &'static str,
    deploy: ProductCommandRecord,
    deploy_status: ProductCommandRecord,
    active_backends: Vec<String>,
}

#[derive(Debug, Serialize)]
struct GatewayHttpEvidence {
    service: &'static str,
    gateway_node: &'static str,
    probe: HttpProbe,
}

#[derive(Debug, Serialize)]
struct GatewayHttpsEvidence {
    service: &'static str,
    gateway_node: &'static str,
    acme_issue: ProductCommandRecord,
    probe: HttpsProbe,
}

pub(crate) fn run() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir(SCENARIO);
    reset_dir(&root)?;
    let harness = ProductHarness::install(&root)?;
    let installed_node_bin = harness.node_bin().display().to_string();
    let installed_help = harness.help()?.record;
    let missing_prerequisites = missing_privileged_prerequisites();
    let privileged_preflight = PrivilegedPreflight {
        enabled: missing_prerequisites.is_empty(),
        skipped_reason: if missing_prerequisites.is_empty() {
            None
        } else {
            Some(
                "privileged workload phase skipped because host prerequisites are missing"
                    .to_string(),
            )
        },
        missing_prerequisites,
    };

    let mut bootstrap_commands = Vec::new();
    bootstrap_commands.push(harness.bootstrap_node("node-a")?.record);
    let mut founder_daemon = spawn_parity_daemon(&harness, "node-a", privileged_preflight.enabled)?;
    let founder_status = NodeCommandStatus {
        node: "node-a",
        status: harness.wait_daemon_status("node-a")?,
    };

    let mut join_commands = Vec::new();
    let mut admit_commands = Vec::new();
    for node in ["node-b", "node-c"] {
        let token = harness.invite("node-a")?;
        join_commands.push(harness.join(node, &token)?.record);
        bootstrap_commands.push(harness.bootstrap_node(node)?.record);
        let admission = harness.admission(node)?;
        admit_commands.push(harness.admit("node-a", &admission)?.record);
    }

    let mut peer_daemons = Vec::new();
    let mut daemon_statuses = vec![founder_status];
    for node in ["node-b", "node-c"] {
        peer_daemons.push(spawn_parity_daemon(
            &harness,
            node,
            privileged_preflight.enabled,
        )?);
        daemon_statuses.push(NodeCommandStatus {
            node,
            status: harness.wait_daemon_status(node)?,
        });
    }

    let runtime_placements = if privileged_preflight.enabled {
        deploy_runtime_workloads(&harness)?
    } else {
        Vec::new()
    };

    let _ = founder_daemon.kill()?;
    for mut daemon in peer_daemons {
        let _ = daemon.kill()?;
    }
    for node in NODES {
        bootstrap_commands.push(harness.bootstrap_node(node)?.record);
    }

    let mut gateways = Vec::new();
    let mut dns_roles = Vec::new();
    for node in NODES {
        if privileged_preflight.enabled {
            gateways.push(harness.spawn_node_gateway_tls(node)?);
        } else {
            gateways.push(harness.spawn_node_gateway(node)?);
        }
        dns_roles.push(harness.spawn_node_dns(node)?);
    }

    let mut gateway_statuses = Vec::new();
    let mut dns_statuses = Vec::new();
    for node in NODES {
        gateway_statuses.push(NodeRoleStatus {
            node,
            status: harness.wait_role(harness.role_control_socket(node, "gateway").as_path())?,
        });
        dns_statuses.push(NodeRoleStatus {
            node,
            status: harness.wait_role(harness.role_control_socket(node, "dns").as_path())?,
        });
    }

    let gateway_http_checks = if privileged_preflight.enabled {
        verify_equal_node_http(&harness, &gateway_statuses)?
    } else {
        Vec::new()
    };
    let acme_https_checks = if privileged_preflight.enabled {
        verify_pebble_https(&harness, &root, &gateway_statuses)?
    } else {
        Vec::new()
    };

    for node in NODES {
        harness.shutdown_role(harness.role_control_socket(node, "gateway").as_path())?;
        harness.shutdown_role(harness.role_control_socket(node, "dns").as_path())?;
    }
    for mut gateway in gateways {
        gateway.wait()?;
    }
    for mut dns in dns_roles {
        dns.wait()?;
    }
    if privileged_preflight.enabled {
        harness.cleanup_docker_runtime(NODES)?;
    }
    harness.cleanup_runtime_processes(NODES)?;

    assert_installed_command_records(
        &installed_node_bin,
        [&installed_help]
            .into_iter()
            .chain(bootstrap_commands.iter())
            .chain(join_commands.iter())
            .chain(admit_commands.iter())
            .chain(daemon_statuses.iter().map(|status| &status.status)),
    )?;
    assert_bootstrap_stdout(&bootstrap_commands)?;

    let report = ThreeNodeParityReport {
        scenario: SCENARIO,
        stage: if runtime_placements.is_empty() {
            "installed-bootstrap-readiness"
        } else {
            "docker-runtime-placement"
        },
        installed_node_bin,
        nodes: NODES.to_vec(),
        privileged_preflight,
        installed_help,
        bootstrap_commands,
        join_commands,
        admit_commands,
        daemon_statuses,
        runtime_placements,
        gateway_http_checks,
        acme_https_checks,
        gateway_statuses,
        dns_statuses,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(&root.join("three-node-parity-smoke-report.json"), &report)?;
    println!("{json}");
    if report.privileged_preflight.enabled && report.runtime_placements.len() != 3 {
        return Err("three-node-parity-smoke did not place all runtime workloads".to_string());
    }
    if report.privileged_preflight.enabled && report.gateway_http_checks.len() != 4 {
        return Err(
            "three-node-parity-smoke did not verify all equal-node HTTP checks".to_string(),
        );
    }
    if report.privileged_preflight.enabled && report.acme_https_checks.len() != 2 {
        return Err("three-node-parity-smoke did not verify Pebble HTTPS checks".to_string());
    }
    if report.privileged_preflight.enabled {
        eprintln!(
            "PASS {SCENARIO} runtime placement and gateway HTTPS; later parity phases still pending"
        );
    } else {
        eprintln!(
            "SKIP {SCENARIO} privileged workload phase skipped; report={}",
            root.join("three-node-parity-smoke-report.json").display()
        );
    }
    Ok(())
}

fn spawn_parity_daemon(
    harness: &ProductHarness,
    node: &str,
    privileged_enabled: bool,
) -> Result<crate::three_server_harness::ProductChild, String> {
    if privileged_enabled {
        return harness.spawn_docker_daemon(
            node,
            60_000,
            DOCKER_IMAGE,
            DOCKER_SERVICE_PORT,
            DOCKER_COMMAND,
        );
    }
    harness.spawn_daemon(node, 60_000)
}

fn deploy_runtime_workloads(
    harness: &ProductHarness,
) -> Result<Vec<RuntimePlacementEvidence>, String> {
    [
        ("web", "node-a", "deploy-web", "rev-1", "web.example.test"),
        ("api", "node-b", "deploy-api", "rev-1", "api.example.test"),
        (
            "echo",
            "node-c",
            "deploy-echo",
            "rev-1",
            "echo.example.test",
        ),
    ]
    .into_iter()
    .map(|(service, target_node, deploy_id, revision, hostname)| {
        let deploy = harness
            .deploy_service_via_daemon(
                "node-a",
                deploy_id,
                target_node,
                service,
                revision,
                hostname,
            )?
            .record;
        let active_backends = parse_active_backends(&deploy.stdout)?;
        if active_backends
            .iter()
            .any(|backend| backend.starts_with(&format!("{target_node}@127.")))
        {
            return Err(format!(
                "{service} deploy used loopback backend in Docker phase: {active_backends:?}"
            ));
        }
        if active_backends
            .iter()
            .all(|backend| !backend.starts_with(&format!("{target_node}@10.210.")))
        {
            return Err(format!(
                "{service} deploy did not report a container subnet backend: {active_backends:?}"
            ));
        }
        let deploy_status = harness.deploy_status("node-a", deploy_id)?.record;
        Ok(RuntimePlacementEvidence {
            service,
            target_node,
            deploy,
            deploy_status,
            active_backends,
        })
    })
    .collect()
}

fn verify_equal_node_http(
    harness: &ProductHarness,
    gateway_statuses: &[NodeRoleStatus],
) -> Result<Vec<GatewayHttpEvidence>, String> {
    [
        ("web", "node-b", "web.example.test"),
        ("web", "node-c", "web.example.test"),
        ("api", "node-a", "api.example.test"),
        ("api", "node-c", "api.example.test"),
    ]
    .into_iter()
    .map(|(service, gateway_node, hostname)| {
        let gateway = role_status(gateway_statuses, gateway_node)?;
        Ok(GatewayHttpEvidence {
            service,
            gateway_node,
            probe: harness.wait_http(gateway.status.listen_addr, hostname, "ok")?,
        })
    })
    .collect()
}

fn verify_pebble_https(
    harness: &ProductHarness,
    root: &std::path::Path,
    gateway_statuses: &[NodeRoleStatus],
) -> Result<Vec<GatewayHttpsEvidence>, String> {
    let pebble = PebbleAcme::start(root)?;
    [
        ("web", "node-b", "web.example.test"),
        ("api", "node-c", "api.example.test"),
    ]
    .into_iter()
    .map(|(service, gateway_node, hostname)| {
        let gateway = role_status(gateway_statuses, gateway_node)?;
        let tls_addr = gateway
            .status
            .tls_listen_addr
            .ok_or_else(|| format!("gateway {gateway_node} did not expose TLS listener"))?;
        let control = harness.role_control_socket(gateway_node, "gateway");
        let acme_issue = harness
            .acme_issue_with_pebble(
                gateway_node,
                hostname,
                gateway.status.listen_addr,
                control.as_path(),
                &pebble,
            )?
            .record;
        Ok(GatewayHttpsEvidence {
            service,
            gateway_node,
            acme_issue,
            probe: harness.wait_https(tls_addr, hostname, &pebble.issued_root_ca_path, "ok")?,
        })
    })
    .collect()
}

fn role_status<'a>(
    statuses: &'a [NodeRoleStatus],
    node: &str,
) -> Result<&'a NodeRoleStatus, String> {
    statuses
        .iter()
        .find(|status| status.node == node)
        .ok_or_else(|| format!("missing role status for {node}"))
}

fn parse_active_backends(stdout: &str) -> Result<Vec<String>, String> {
    let value = serde_json::from_str::<serde_json::Value>(stdout)
        .map_err(|error| format!("decode deploy response: {error}; stdout={stdout}"))?;
    let backends = value
        .get("active_backends")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("deploy response missing active_backends: {value}"))?;
    Ok(backends
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(ToString::to_string)
        .collect())
}

fn assert_installed_command_records<'a>(
    installed_node_bin: &str,
    records: impl IntoIterator<Item = &'a ProductCommandRecord>,
) -> Result<(), String> {
    for record in records {
        if record.program != installed_node_bin {
            return Err(format!(
                "command did not run installed binary: program={} installed={} args={:?}",
                record.program, installed_node_bin, record.args
            ));
        }
    }
    Ok(())
}

fn assert_bootstrap_stdout(bootstrap_commands: &[ProductCommandRecord]) -> Result<(), String> {
    for command in bootstrap_commands {
        let value =
            serde_json::from_str::<serde_json::Value>(&command.stdout).map_err(|error| {
                format!(
                    "decode bootstrap stdout: {error}; stdout={}",
                    command.stdout
                )
            })?;
        if value.get("status").and_then(serde_json::Value::as_str) != Some("bootstrapped") {
            return Err(format!("bootstrap did not report bootstrapped: {value}"));
        }
    }
    Ok(())
}

fn missing_privileged_prerequisites() -> Vec<String> {
    let mut missing = Vec::new();
    if !cfg!(target_os = "linux") {
        missing.push("Linux host".to_string());
    }
    if !is_root() {
        missing.push("root privileges".to_string());
    }
    for command in ["docker", "ip", "iptables", "ployz-bpfctl"] {
        if !command_available(command) {
            missing.push(format!("command `{command}`"));
        }
    }
    if command_available("docker") && !docker_daemon_available() {
        missing.push("running Docker daemon".to_string());
    }
    missing
}

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|uid| uid.trim() == "0")
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--help")
        .output()
        .map(|output| {
            output.status.success() || !output.stderr.is_empty() || !output.stdout.is_empty()
        })
        .unwrap_or(false)
}

fn docker_daemon_available() -> bool {
    Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    let root = scenario_dir(SCENARIO);
    if !root.exists() {
        return Ok(());
    }
    let harness = ProductHarness::install(&root)?;
    let _ = harness.cleanup_docker_runtime(NODES);
    harness.cleanup_runtime_processes(NODES)?;
    Ok(())
}

use std::process::Command;
use std::time::Instant;

use serde::Serialize;

use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::three_server_harness::{ProductCommandRecord, ProductHarness, ServingRoleProbe};

const SCENARIO: &str = "three-node-parity-smoke";
const NODES: &[&str] = &["node-a", "node-b", "node-c"];

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

pub(crate) fn run() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir(SCENARIO);
    reset_dir(&root)?;
    let harness = ProductHarness::install(&root)?;
    let installed_node_bin = harness.node_bin().display().to_string();
    let installed_help = harness.help()?.record;

    let mut bootstrap_commands = Vec::new();
    bootstrap_commands.push(harness.bootstrap_node("node-a")?.record);
    let mut founder_daemon = harness.spawn_daemon("node-a", 60_000)?;
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
        peer_daemons.push(harness.spawn_daemon(node, 60_000)?);
        daemon_statuses.push(NodeCommandStatus {
            node,
            status: harness.wait_daemon_status(node)?,
        });
    }

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
        gateways.push(harness.spawn_node_gateway(node)?);
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

    let missing_prerequisites = missing_privileged_prerequisites();
    let privileged_preflight = PrivilegedPreflight {
        enabled: missing_prerequisites.is_empty(),
        skipped_reason: if missing_prerequisites.is_empty() {
            Some("privileged workload phase is not wired yet".to_string())
        } else {
            Some(
                "privileged workload phase skipped because host prerequisites are missing"
                    .to_string(),
            )
        },
        missing_prerequisites,
    };

    let report = ThreeNodeParityReport {
        scenario: SCENARIO,
        stage: "installed-bootstrap-readiness",
        installed_node_bin,
        nodes: NODES.to_vec(),
        privileged_preflight,
        installed_help,
        bootstrap_commands,
        join_commands,
        admit_commands,
        daemon_statuses,
        gateway_statuses,
        dns_statuses,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(&root.join("three-node-parity-smoke-report.json"), &report)?;
    println!("{json}");
    if report.privileged_preflight.enabled {
        return Err(
            "three-node-parity-smoke reached installed bootstrap readiness, but privileged workload parity is not wired yet"
                .to_string(),
        );
    }
    eprintln!(
        "SKIP {SCENARIO} privileged workload phase skipped; report={}",
        root.join("three-node-parity-smoke-report.json").display()
    );
    Ok(())
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

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    let root = scenario_dir(SCENARIO);
    if !root.exists() {
        return Ok(());
    }
    ProductHarness::install(&root)?.cleanup_runtime_processes(NODES)?;
    Ok(())
}

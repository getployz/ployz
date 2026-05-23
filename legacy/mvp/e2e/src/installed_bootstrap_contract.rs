use std::time::Instant;

use serde::Serialize;

use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::three_server_harness::{ProductCommandRecord, ProductHarness, ServingRoleProbe};

const SCENARIO: &str = "installed-bootstrap-contract";
const NODES: &[&str] = &["node-a", "node-b", "node-c"];

#[derive(Debug, Serialize)]
struct InstalledBootstrapReport {
    scenario: &'static str,
    installed_node_bin: String,
    nodes: Vec<&'static str>,
    installed_help: ProductCommandRecord,
    bootstrap_commands: Vec<ProductCommandRecord>,
    join_commands: Vec<ProductCommandRecord>,
    admit_commands: Vec<ProductCommandRecord>,
    daemon_statuses: Vec<ProductCommandRecord>,
    gateway_statuses: Vec<NodeRoleStatus>,
    dns_statuses: Vec<NodeRoleStatus>,
    restarted_daemon_status: ProductCommandRecord,
    gateway_after_daemon_restart: NodeRoleStatus,
    dns_after_daemon_restart: NodeRoleStatus,
    elapsed_ms: u128,
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
    let founder_status = harness.wait_daemon_status("node-a")?;

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
        let daemon = harness.spawn_daemon(node, 60_000)?;
        daemon_statuses.push(harness.wait_daemon_status(node)?);
        peer_daemons.push(daemon);
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
    let mut gateway_statuses = Vec::new();
    let mut dns_statuses = Vec::new();
    for node in NODES {
        gateways.push(harness.spawn_node_gateway(node)?);
        dns_roles.push(harness.spawn_node_dns(node)?);
    }
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

    let mut restarted_daemon = harness.spawn_daemon("node-b", 30_000)?;
    let restarted_daemon_status = harness.wait_daemon_status("node-b")?;
    let gateway_after_daemon_restart = NodeRoleStatus {
        node: "node-b",
        status: harness.role_request(
            harness.role_control_socket("node-b", "gateway").as_path(),
            "status",
        )?,
    };
    let dns_after_daemon_restart = NodeRoleStatus {
        node: "node-b",
        status: harness.role_request(
            harness.role_control_socket("node-b", "dns").as_path(),
            "status",
        )?,
    };

    let _ = restarted_daemon.kill()?;
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

    assert_installed_commands(&installed_node_bin, &installed_help, &bootstrap_commands)?;

    let report = InstalledBootstrapReport {
        scenario: SCENARIO,
        installed_node_bin,
        nodes: NODES.to_vec(),
        installed_help,
        bootstrap_commands,
        join_commands,
        admit_commands,
        daemon_statuses,
        gateway_statuses,
        dns_statuses,
        restarted_daemon_status,
        gateway_after_daemon_restart,
        dns_after_daemon_restart,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(
        &root.join("installed-bootstrap-contract-report.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS {SCENARIO}");
    Ok(())
}

fn assert_installed_commands(
    installed_node_bin: &str,
    installed_help: &ProductCommandRecord,
    bootstrap_commands: &[ProductCommandRecord],
) -> Result<(), String> {
    if installed_help.program != installed_node_bin {
        return Err(format!(
            "help did not run installed binary: program={} installed={installed_node_bin}",
            installed_help.program
        ));
    }
    for command in bootstrap_commands {
        if command.program != installed_node_bin {
            return Err(format!(
                "bootstrap did not run installed binary: program={} installed={installed_node_bin}",
                command.program
            ));
        }
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

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    let root = scenario_dir(SCENARIO);
    if !root.exists() {
        return Ok(());
    }
    ProductHarness::install(&root)?.cleanup_runtime_processes(NODES)?;
    Ok(())
}

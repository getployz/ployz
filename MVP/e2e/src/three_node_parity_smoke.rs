use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::three_server_harness::ProductHarness;

mod preflight;
mod report;
mod verification;

use self::preflight::require_privileged_prerequisites;
use self::report::{NODES, NodeCommandStatus, NodeRoleStatus, SCENARIO, ThreeNodeParityReport};
use self::verification::{
    assert_bootstrap_stdout, assert_installed_command_records, container_gateway_ip,
    deploy_runtime_workloads, spawn_parity_daemon, verify_container_dns,
    verify_daemon_restart_survival, verify_equal_node_http, verify_pebble_https,
    verify_update_drain, wait_gateway_route_projection,
};

const DOCKER_IMAGE: &str = "busybox:latest";
const DOCKER_SERVICE_PORT: u16 = 8080;
const DOCKER_COMMAND: &str = "mkdir -p /www && echo ok-$PLOYZ_SERVICE-$PLOYZ_REVISION >/www/index.html && httpd -f -p 8080 -h /www";
const PROJECTION_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const PROJECTION_POLL: Duration = Duration::from_millis(100);

pub(crate) fn run() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir(SCENARIO);
    reset_dir(&root)?;
    let harness = ProductHarness::install(&root)?;
    let installed_node_bin = harness.node_bin().display().to_string();
    let installed_help = harness.help()?.record;
    let privileged_preflight = require_privileged_prerequisites()?;

    let mut bootstrap_commands = Vec::new();
    bootstrap_commands.push(harness.bootstrap_node("node-a")?.record);
    let mut founder_daemon = spawn_parity_daemon(&harness, "node-a")?;
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

    let mut daemon_statuses = vec![founder_status];
    let mut node_b_daemon = spawn_parity_daemon(&harness, "node-b")?;
    daemon_statuses.push(NodeCommandStatus {
        node: "node-b",
        status: harness.wait_daemon_status("node-b")?,
    });
    let mut node_c_daemon = spawn_parity_daemon(&harness, "node-c")?;
    daemon_statuses.push(NodeCommandStatus {
        node: "node-c",
        status: harness.wait_daemon_status("node-c")?,
    });

    let runtime_placements = deploy_runtime_workloads(&harness)?;
    wait_gateway_route_projection(&harness, runtime_placements.len())?;

    let mut gateways = Vec::new();
    let mut dns_roles = Vec::new();
    for node in NODES {
        gateways.push(harness.spawn_node_gateway_tls(node)?);
        let listen = SocketAddr::new(container_gateway_ip(&bootstrap_commands, node)?.into(), 53);
        dns_roles.push(harness.spawn_node_dns_at(node, listen)?);
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

    let gateway_http_checks = verify_equal_node_http(&harness, &gateway_statuses)?;
    let acme_https_checks = verify_pebble_https(&harness, &root, &gateway_statuses)?;
    let container_dns_checks = verify_container_dns(&harness, &bootstrap_commands)?;
    let update_drain_checks = vec![verify_update_drain(&harness, &gateway_statuses)?];
    let daemon_restart_checks = vec![verify_daemon_restart_survival(
        &harness,
        &gateway_statuses,
        &bootstrap_commands,
        &mut node_b_daemon,
    )?];

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
    let _ = founder_daemon.kill()?;
    let _ = node_b_daemon.kill()?;
    let _ = node_c_daemon.kill()?;
    harness.cleanup_docker_runtime(NODES)?;
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
        stage: "docker-runtime-placement",
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
        container_dns_checks,
        update_drain_checks,
        daemon_restart_checks,
        gateway_statuses,
        dns_statuses,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(&root.join("three-node-parity-smoke-report.json"), &report)?;
    println!("{json}");
    if report.runtime_placements.len() != 3 {
        return Err("three-node-parity-smoke did not place all runtime workloads".to_string());
    }
    if report.gateway_http_checks.len() != 4 {
        return Err(
            "three-node-parity-smoke did not verify all equal-node HTTP checks".to_string(),
        );
    }
    if report.acme_https_checks.len() != 2 {
        return Err("three-node-parity-smoke did not verify Pebble HTTPS checks".to_string());
    }
    if report.container_dns_checks.len() != 1 {
        return Err("three-node-parity-smoke did not verify container service DNS".to_string());
    }
    if report.update_drain_checks.len() != 1 {
        return Err("three-node-parity-smoke did not verify update/drain".to_string());
    }
    if report.daemon_restart_checks.len() != 1 {
        return Err("three-node-parity-smoke did not verify daemon restart survival".to_string());
    }
    eprintln!(
        "PASS {SCENARIO} lower-level single-host runtime placement, gateway HTTPS, container DNS, update/drain, and daemon restart survival; final multi-container parity gate is crates/ployz-e2e mvp_three_node_parity_smoke"
    );
    Ok(())
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

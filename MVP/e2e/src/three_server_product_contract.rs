use std::thread;
use std::time::Instant;

use serde::Serialize;

use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::three_server_harness::{
    DnsProbe, HttpProbe, ProductCommandRecord, ProductHarness, ServingRoleProbe,
};

const SCENARIO: &str = "three-server-product";
const NODES: &[&str] = &["founder", "peer-a", "peer-b"];
const HOSTNAME: &str = "web.example.test";
const EXPECTED_BODY: &str = "instance=deploy-smoke-web-rev-1";

#[derive(Debug, Serialize)]
struct ThreeServerProductReport {
    scenario: &'static str,
    nodes: Vec<&'static str>,
    init_commands: Vec<ProductCommandRecord>,
    join_commands: Vec<ProductCommandRecord>,
    admit_commands: Vec<ProductCommandRecord>,
    daemon_commands: Vec<ProductCommandRecord>,
    daemon_imported_operations: Vec<u64>,
    daemon_node_agent_handlers: Vec<u64>,
    deploy_target_node: &'static str,
    deploy_target_daemon_status: ProductCommandRecord,
    deploy: ProductCommandRecord,
    gateway_before_daemon_kill: HttpProbe,
    dns_before_daemon_kill: DnsProbe,
    gateway_after_daemon_kill: HttpProbe,
    dns_after_daemon_kill: DnsProbe,
    gateway_status: ServingRoleProbe,
    dns_status: ServingRoleProbe,
    killed_runtime_processes: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir(SCENARIO);
    reset_dir(&root)?;
    let harness = ProductHarness::new(&root)?;

    let init_commands = vec![harness.init_node("founder")?.record];

    let mut join_commands = Vec::new();
    let mut admit_commands = Vec::new();
    for peer in ["peer-a", "peer-b"] {
        let token = harness.invite("founder")?;
        join_commands.push(harness.join(peer, &token)?.record);
        let admission = harness.admission(peer)?;
        admit_commands.push(harness.admit("founder", &admission)?.record);
    }

    let mut daemon_commands = Vec::new();
    let daemon_handles = NODES
        .iter()
        .map(|node| {
            let node = *node;
            let harness = harness.clone();
            thread::spawn(move || harness.run_daemon_once(node, 3_000))
        })
        .collect::<Vec<_>>();
    for handle in daemon_handles {
        daemon_commands.push(
            handle
                .join()
                .map_err(|_| "daemon worker panicked".to_string())??
                .record,
        );
    }
    let daemon_imported_operations = daemon_commands
        .iter()
        .map(|record| parse_daemon_field(&record.stdout, "imported_operations"))
        .collect::<Result<Vec<_>, _>>()?;
    let daemon_node_agent_handlers = daemon_commands
        .iter()
        .map(|record| parse_daemon_field(&record.stdout, "node_agent_handlers"))
        .collect::<Result<Vec<_>, _>>()?;
    if daemon_imported_operations.contains(&0) {
        return Err(format!(
            "expected every daemon to import membership operations, got {daemon_imported_operations:?}"
        ));
    }
    if daemon_node_agent_handlers.iter().any(|count| *count < 6) {
        return Err(format!(
            "expected every daemon to register node-agent handlers, got {daemon_node_agent_handlers:?}"
        ));
    }

    let mut deploy_target_daemon = harness.spawn_daemon("peer-a", 30_000)?;
    let deploy_target_daemon_status = harness.wait_daemon_status("peer-a")?;
    let deploy = harness
        .deploy("founder", "peer-a", "rev-1", HOSTNAME)?
        .record;
    let mut gateway = harness.spawn_gateway("founder")?;
    let mut dns = harness.spawn_dns("founder")?;
    let gateway_status = harness.wait_role(harness.control_socket("gateway").as_path())?;
    let dns_status = harness.wait_role(harness.control_socket("dns").as_path())?;
    let gateway_before_daemon_kill =
        harness.wait_http(gateway_status.listen_addr, HOSTNAME, EXPECTED_BODY)?;
    let dns_before_daemon_kill = harness.wait_dns(dns_status.listen_addr, HOSTNAME, "127.0.0.1")?;

    let _daemon_exit = deploy_target_daemon.kill()?;
    let gateway_after_daemon_kill =
        harness.wait_http(gateway_status.listen_addr, HOSTNAME, EXPECTED_BODY)?;
    let dns_after_daemon_kill = harness.wait_dns(dns_status.listen_addr, HOSTNAME, "127.0.0.1")?;

    harness.shutdown_role(harness.control_socket("gateway").as_path())?;
    harness.shutdown_role(harness.control_socket("dns").as_path())?;
    gateway.wait()?;
    dns.wait()?;
    let killed_runtime_processes = harness.cleanup_runtime_processes(NODES)?;

    let report = ThreeServerProductReport {
        scenario: SCENARIO,
        nodes: NODES.to_vec(),
        init_commands,
        join_commands,
        admit_commands,
        daemon_commands,
        daemon_imported_operations,
        daemon_node_agent_handlers,
        deploy_target_node: "peer-a",
        deploy_target_daemon_status,
        deploy,
        gateway_before_daemon_kill,
        dns_before_daemon_kill,
        gateway_after_daemon_kill,
        dns_after_daemon_kill,
        gateway_status,
        dns_status,
        killed_runtime_processes,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(&root.join("three-server-product-report.json"), &report)?;
    println!("{json}");
    eprintln!("PASS {SCENARIO}");
    Ok(())
}

fn parse_daemon_field(stdout: &str, field: &str) -> Result<u64, String> {
    let prefix = format!("{field}=");
    stdout
        .split_ascii_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
        .ok_or_else(|| format!("daemon output missing {field}: {stdout}"))?
        .parse::<u64>()
        .map_err(|error| format!("parse daemon field {field}: {error}; stdout={stdout}"))
}

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    let root = scenario_dir(SCENARIO);
    if !root.exists() {
        return Ok(());
    }
    ProductHarness::new(&root)?.cleanup_runtime_processes(NODES)?;
    Ok(())
}

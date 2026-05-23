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
const EXPECTED_BODY_REV_1: &str = "instance=deploy-smoke-web-rev-1";
const EXPECTED_BODY_REV_2: &str = "instance=deploy-smoke-2-web-rev-2";

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
    deploy_coordinator_daemon_status: ProductCommandRecord,
    deploy_target_daemon_status: ProductCommandRecord,
    restarted_target_daemon_status: ProductCommandRecord,
    deploy: ProductCommandRecord,
    deploy_status: ProductCommandRecord,
    live_update_deploy: ProductCommandRecord,
    live_update_deploy_status: ProductCommandRecord,
    gateway_before_live_reload: HttpProbe,
    dns_before_live_reload: DnsProbe,
    gateway_reloaded_status: ServingRoleProbe,
    dns_reloaded_status: ServingRoleProbe,
    gateway_after_live_reload: HttpProbe,
    dns_after_live_reload: DnsProbe,
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
    let mut deploy_coordinator_daemon = harness.spawn_daemon("founder", 60_000)?;
    let deploy_coordinator_daemon_status = harness.wait_daemon_status("founder")?;

    let mut join_commands = Vec::new();
    let mut admit_commands = Vec::new();
    for peer in ["peer-a", "peer-b"] {
        let token = harness.invite("founder")?;
        join_commands.push(harness.join(peer, &token)?.record);
        let admission = harness.admission(peer)?;
        admit_commands.push(harness.admit("founder", &admission)?.record);
    }

    let mut daemon_commands = Vec::new();
    let daemon_handles = ["peer-a", "peer-b"]
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
        .deploy_via_daemon("founder", "peer-a", "rev-1", HOSTNAME)?
        .record;
    let deploy_status = harness.deploy_status("founder", "deploy-smoke")?.record;
    assert_deploy_status_complete(&deploy_status.stdout)?;
    let mut gateway = harness.spawn_gateway("founder")?;
    let mut dns = harness.spawn_dns("founder")?;
    let gateway_status = harness.wait_role(harness.control_socket("gateway").as_path())?;
    let dns_status = harness.wait_role(harness.control_socket("dns").as_path())?;
    let gateway_before_live_reload =
        harness.wait_http(gateway_status.listen_addr, HOSTNAME, EXPECTED_BODY_REV_1)?;
    let dns_before_live_reload = harness.wait_dns(dns_status.listen_addr, HOSTNAME, "127.0.0.1")?;

    let live_update_deploy = harness
        .deploy_via_daemon_with_id("founder", "deploy-smoke-2", "peer-a", "rev-2", HOSTNAME)?
        .record;
    let live_update_deploy_status = harness.deploy_status("founder", "deploy-smoke-2")?.record;
    assert_deploy_status_complete(&live_update_deploy_status.stdout)?;
    let gateway_reloaded_status =
        harness.reload_role(harness.control_socket("gateway").as_path())?;
    let dns_reloaded_status = harness.reload_role(harness.control_socket("dns").as_path())?;
    if gateway_reloaded_status.loaded_generation == gateway_status.loaded_generation {
        return Err("gateway reload did not advance serving generation".to_string());
    }
    if dns_reloaded_status.loaded_generation == dns_status.loaded_generation {
        return Err("dns reload did not advance serving generation".to_string());
    }
    let gateway_after_live_reload = harness.wait_http(
        gateway_reloaded_status.listen_addr,
        HOSTNAME,
        EXPECTED_BODY_REV_2,
    )?;
    let dns_after_live_reload =
        harness.wait_dns(dns_reloaded_status.listen_addr, HOSTNAME, "127.0.0.1")?;

    let _coordinator_daemon_exit = deploy_coordinator_daemon.kill()?;
    let _daemon_exit = deploy_target_daemon.kill()?;
    let gateway_after_daemon_kill = harness.wait_http(
        gateway_reloaded_status.listen_addr,
        HOSTNAME,
        EXPECTED_BODY_REV_2,
    )?;
    let dns_after_daemon_kill =
        harness.wait_dns(dns_reloaded_status.listen_addr, HOSTNAME, "127.0.0.1")?;
    let mut restarted_target_daemon = harness.spawn_daemon("peer-a", 10_000)?;
    let restarted_target_daemon_status = harness.wait_daemon_status("peer-a")?;
    let _restarted_daemon_exit = restarted_target_daemon.kill()?;

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
        deploy_coordinator_daemon_status,
        deploy_target_daemon_status,
        restarted_target_daemon_status,
        deploy,
        deploy_status,
        live_update_deploy,
        live_update_deploy_status,
        gateway_before_live_reload,
        dns_before_live_reload,
        gateway_reloaded_status,
        dns_reloaded_status,
        gateway_after_live_reload,
        dns_after_live_reload,
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

fn assert_deploy_status_complete(stdout: &str) -> Result<(), String> {
    let value = serde_json::from_str::<serde_json::Value>(stdout)
        .map_err(|error| format!("decode deploy status: {error}; stdout={stdout}"))?;
    let phases = value
        .pointer("/statuses")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("deploy status missing statuses: {stdout}"))?
        .iter()
        .filter_map(|status| status.get("phase").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    for expected in ["planned", "cleanup_pending", "cleanup_done"] {
        if !phases.contains(&expected) {
            return Err(format!(
                "deploy status missing phase {expected}; phases={phases:?}; stdout={stdout}"
            ));
        }
    }
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

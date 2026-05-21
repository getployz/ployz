use std::fs;
use std::net::Ipv4Addr;
use std::thread;
use std::time::Instant;

use crate::three_server_harness::{PebbleAcme, ProductCommandRecord, ProductHarness};

use super::report::{
    ContainerDnsEvidence, DaemonRestartEvidence, DeployResponse, GatewayHttpEvidence,
    GatewayHttpsEvidence, NODES, NodeRoleStatus, RuntimePlacementEvidence, UpdateDrainEvidence,
};
use super::{
    DOCKER_COMMAND, DOCKER_IMAGE, DOCKER_SERVICE_PORT, PROJECTION_POLL, PROJECTION_WAIT_TIMEOUT,
};

pub(super) fn spawn_parity_daemon(
    harness: &ProductHarness,
    node: &str,
) -> Result<crate::three_server_harness::ProductChild, String> {
    harness.spawn_docker_daemon(
        node,
        60_000,
        DOCKER_IMAGE,
        DOCKER_SERVICE_PORT,
        DOCKER_COMMAND,
    )
}

pub(super) fn deploy_runtime_workloads(
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

pub(super) fn verify_equal_node_http(
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

pub(super) fn verify_pebble_https(
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

pub(super) fn verify_container_dns(
    harness: &ProductHarness,
    bootstrap_commands: &[ProductCommandRecord],
) -> Result<Vec<ContainerDnsEvidence>, String> {
    let dns_server = container_gateway_ip(bootstrap_commands, "node-a")?.to_string();
    let probe = harness.run_container_dns_client(
        "ployz-mvp-node-a",
        &dns_server,
        "echo.service.example.test",
        "http://echo.service.example.test:8080/",
    )?;
    if probe.body.trim() != "ok-echo-rev-1" {
        return Err(format!(
            "container DNS client reached echo but body was unexpected: {}",
            probe.body
        ));
    }
    let dns_answer = container_dns_answer(&probe.nslookup_stdout)?;
    Ok(vec![ContainerDnsEvidence {
        client_node: "node-a",
        service: "echo",
        dns_answer,
        probe,
    }])
}

pub(super) fn verify_update_drain(
    harness: &ProductHarness,
    gateway_statuses: &[NodeRoleStatus],
) -> Result<UpdateDrainEvidence, String> {
    let pre_update_checks = verify_api_http_body(harness, gateway_statuses, "ok-api-rev-1")?;
    let update_deploy = harness
        .deploy_service_via_daemon(
            "node-a",
            "deploy-api-v2",
            "node-b",
            "api",
            "rev-2",
            "api.example.test",
        )?
        .record;
    let response = parse_deploy_response(&update_deploy.stdout)?;
    if response.old_backends == 0 {
        return Err(format!(
            "api rev-2 deploy did not report old backends to drain: {}",
            update_deploy.stdout
        ));
    }
    if response
        .active_backends
        .iter()
        .all(|backend| !backend.starts_with("node-b@10.210."))
    {
        return Err(format!(
            "api rev-2 deploy did not report a node-b container subnet backend: {:?}",
            response.active_backends
        ));
    }
    let update_deploy_status = harness.deploy_status("node-a", "deploy-api-v2")?.record;
    assert_deploy_status_complete(&update_deploy_status.stdout)?;
    wait_gateway_api_update_projection(&harness)?;
    let gateway_reloads = reload_gateways(harness)?;
    let post_update_checks = verify_api_http_body(harness, gateway_statuses, "ok-api-rev-2")?;

    Ok(UpdateDrainEvidence {
        service: "api",
        target_node: "node-b",
        pre_update_checks,
        update_deploy,
        update_deploy_status,
        updated_active_backends: response.active_backends,
        old_backends_to_drain: response.old_backends,
        gateway_reloads,
        post_update_checks,
    })
}

pub(super) fn verify_daemon_restart_survival(
    harness: &ProductHarness,
    gateway_statuses: &[NodeRoleStatus],
    bootstrap_commands: &[ProductCommandRecord],
    node_b_daemon: &mut crate::three_server_harness::ProductChild,
) -> Result<DaemonRestartEvidence, String> {
    let _ = node_b_daemon.kill()?;
    *node_b_daemon = spawn_parity_daemon(harness, "node-b")?;
    let post_restart_status = harness.wait_daemon_status("node-b")?;
    let post_restart_checks = verify_api_http_body(harness, gateway_statuses, "ok-api-rev-2")?;
    let post_restart_container_dns = verify_container_dns(harness, bootstrap_commands)?;

    Ok(DaemonRestartEvidence {
        restarted_node: "node-b",
        post_restart_status,
        post_restart_checks,
        post_restart_container_dns,
    })
}

fn verify_api_http_body(
    harness: &ProductHarness,
    gateway_statuses: &[NodeRoleStatus],
    expected_body: &str,
) -> Result<Vec<GatewayHttpEvidence>, String> {
    ["node-a", "node-b", "node-c"]
        .into_iter()
        .map(|gateway_node| {
            let gateway = role_status(gateway_statuses, gateway_node)?;
            Ok(GatewayHttpEvidence {
                service: "api",
                gateway_node,
                probe: harness.wait_http(
                    gateway.status.listen_addr,
                    "api.example.test",
                    expected_body,
                )?,
            })
        })
        .collect()
}

fn reload_gateways(harness: &ProductHarness) -> Result<Vec<NodeRoleStatus>, String> {
    NODES
        .iter()
        .map(|node| {
            Ok(NodeRoleStatus {
                node: *node,
                status: harness
                    .reload_role(harness.role_control_socket(node, "gateway").as_path())?,
            })
        })
        .collect()
}

pub(super) fn wait_gateway_route_projection(
    harness: &ProductHarness,
    expected_routes: usize,
) -> Result<(), String> {
    let deadline = Instant::now() + PROJECTION_WAIT_TIMEOUT;
    let mut last = Vec::new();
    loop {
        last.clear();
        let mut ready = true;
        for node in NODES {
            let path = harness.node_dir(node).join("gateway.snapshot");
            match gateway_route_count(&path) {
                Ok(count) if count >= expected_routes => {
                    last.push(format!("{node}={count}"));
                }
                Ok(count) => {
                    ready = false;
                    last.push(format!("{node}={count}"));
                }
                Err(error) => {
                    ready = false;
                    last.push(format!("{node}={error}"));
                }
            }
        }
        if ready {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "gateway projections did not reach {expected_routes} routes: {}",
                last.join(", ")
            ));
        }
        thread::sleep(PROJECTION_POLL);
    }
}

fn wait_gateway_api_update_projection(harness: &ProductHarness) -> Result<(), String> {
    let deadline = Instant::now() + PROJECTION_WAIT_TIMEOUT;
    let mut last = Vec::new();
    loop {
        last.clear();
        let mut ready = true;
        for node in NODES {
            let path = harness.node_dir(node).join("gateway.snapshot");
            match api_update_snapshot_status(&path) {
                Ok(()) => {
                    last.push(format!("{node}=ready"));
                }
                Err(error) => {
                    ready = false;
                    last.push(format!("{node}={error}"));
                }
            }
        }
        if ready {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "gateway projections did not show api update/drain state: {}",
                last.join(", ")
            ));
        }
        thread::sleep(PROJECTION_POLL);
    }
}

fn api_update_snapshot_status(path: &std::path::Path) -> Result<(), String> {
    let snapshot =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&snapshot)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    let gateway_commit_id = value
        .get("gateway_commit_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let route_commit_id = value
        .get("route_commit_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if gateway_commit_id != "deploy-api-v2-gateway" || route_commit_id != "deploy-api-v2-route" {
        return Err(format!("revision={gateway_commit_id}/{route_commit_id}"));
    }
    let routes = value
        .get("routes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "missing routes".to_string())?;
    let api_route = routes
        .iter()
        .find(|route| route.get("route_id").and_then(serde_json::Value::as_str) == Some("api"))
        .ok_or_else(|| "missing api route".to_string())?;
    let has_active = endpoint_list_contains_node_subnet(api_route.get("backends"), "node-b");
    let has_old =
        endpoint_list_contains_node_subnet(api_route.get("old_backends_to_drain"), "node-b");
    if !has_active || !has_old {
        return Err(format!("api route active={has_active} old={has_old}"));
    }
    Ok(())
}

fn endpoint_list_contains_node_subnet(value: Option<&serde_json::Value>, node_id: &str) -> bool {
    value
        .and_then(serde_json::Value::as_array)
        .is_some_and(|endpoints| {
            endpoints.iter().any(|endpoint| {
                endpoint.get("node_id").and_then(serde_json::Value::as_str) == Some(node_id)
                    && endpoint
                        .get("address")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|address| address.starts_with("10.210."))
            })
        })
}

fn gateway_route_count(path: &std::path::Path) -> Result<usize, String> {
    let snapshot =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let value = serde_json::from_str::<serde_json::Value>(&snapshot)
        .map_err(|error| format!("decode {}: {error}", path.display()))?;
    value
        .get("routes")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| format!("{} missing routes array", path.display()))
}

fn parse_deploy_response(stdout: &str) -> Result<DeployResponse, String> {
    serde_json::from_str(stdout)
        .map_err(|error| format!("decode deploy response: {error}; stdout={stdout}"))
}

fn assert_deploy_status_complete(stdout: &str) -> Result<(), String> {
    let value = serde_json::from_str::<serde_json::Value>(stdout)
        .map_err(|error| format!("decode deploy status: {error}; stdout={stdout}"))?;
    let phases = value
        .get("statuses")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("deploy status missing statuses: {value}"))?;
    if phases
        .iter()
        .any(|entry| entry.get("phase").and_then(serde_json::Value::as_str) == Some("failed"))
    {
        return Err(format!("deploy status contains failed phase: {value}"));
    }
    if phases
        .iter()
        .all(|entry| entry.get("phase").and_then(serde_json::Value::as_str) != Some("cleanup_done"))
    {
        return Err(format!("deploy status never reached cleanup_done: {value}"));
    }
    Ok(())
}

fn container_dns_answer(nslookup_stdout: &str) -> Result<String, String> {
    nslookup_stdout
        .lines()
        .filter_map(|line| line.split_once(':').map(|(_key, value)| value.trim()))
        .find(|value| value.starts_with("10.210."))
        .map(ToString::to_string)
        .ok_or_else(|| {
            format!("nslookup did not include a container subnet answer: {nslookup_stdout}")
        })
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

pub(super) fn container_gateway_ip(
    bootstrap_commands: &[ProductCommandRecord],
    node: &str,
) -> Result<Ipv4Addr, String> {
    for command in bootstrap_commands.iter().rev() {
        let value =
            serde_json::from_str::<serde_json::Value>(&command.stdout).map_err(|error| {
                format!(
                    "decode bootstrap stdout for container subnet: {error}; stdout={}",
                    command.stdout
                )
            })?;
        if value.get("node_id").and_then(serde_json::Value::as_str) != Some(node) {
            continue;
        }
        let subnet = value
            .pointer("/identity/container_subnet")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("bootstrap output missing container_subnet: {value}"))?;
        return docker_gateway_ip(subnet);
    }
    Err(format!("missing bootstrap output for {node}"))
}

fn docker_gateway_ip(subnet: &str) -> Result<Ipv4Addr, String> {
    let cidr = subnet
        .split_once('/')
        .map(|(addr, _prefix)| addr)
        .ok_or_else(|| format!("container subnet missing prefix: {subnet}"))?;
    let octets = cidr
        .split('.')
        .map(|octet| {
            octet
                .parse::<u8>()
                .map_err(|error| format!("parse container subnet octet '{octet}': {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let [a, b, c, _d] = octets.as_slice() else {
        return Err(format!("container subnet is not IPv4: {subnet}"));
    };
    Ok(Ipv4Addr::new(*a, *b, *c, 1))
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

pub(super) fn assert_installed_command_records<'a>(
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

pub(super) fn assert_bootstrap_stdout(
    bootstrap_commands: &[ProductCommandRecord],
) -> Result<(), String> {
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

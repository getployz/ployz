use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use crate::support::wait_until;
use serde::Serialize;

use super::cluster::{MvpBootstrapEvidence, MvpDaemonEvidence, restart_peer_daemon};
use super::serving::{
    API_HOSTNAME, MvpContainerDnsEvidence, MvpGatewayHttpEvidence, MvpGatewayHttpsEvidence,
    probe_container_dns, probe_web_https, wait_gateway_body,
};
use super::{DATA_PLANE_WAIT_TIMEOUT, shell_quote};

const DAEMON_PID: &str = "/tmp/mvp-node-daemon.pid";
const GATEWAY_PID: &str = "/tmp/mvp-node-gateway.pid";
const DNS_PID: &str = "/tmp/mvp-node-dns.pid";
const API_V2_CONTAINER: &str = "ployz-mvp-node-b-deploy-api-v2-api-rev-2";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpDaemonRestartEvidence {
    pub(crate) restarted_node: &'static str,
    pub(crate) stopped_daemon_pid: String,
    pub(crate) restarted_daemon: MvpDaemonEvidence,
    pub(crate) survivor_processes: MvpRestartSurvivorEvidence,
    pub(crate) traffic_while_daemon_down: MvpRestartTrafficEvidence,
    pub(crate) traffic_after_restart: MvpRestartTrafficEvidence,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpRestartSurvivorEvidence {
    pub(crate) gateway_pid: String,
    pub(crate) dns_pid: String,
    pub(crate) runtime_container: MvpRuntimeContainerEvidence,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpRuntimeContainerEvidence {
    pub(crate) harness_node: &'static str,
    pub(crate) service: &'static str,
    pub(crate) revision: &'static str,
    pub(crate) container: &'static str,
    pub(crate) container_id: String,
    pub(crate) running: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpRestartTrafficEvidence {
    pub(crate) gateway_http: Vec<MvpGatewayHttpEvidence>,
    pub(crate) gateway_https: Vec<MvpGatewayHttpsEvidence>,
    pub(crate) container_dns: MvpContainerDnsEvidence,
}

pub(crate) fn verify_daemon_restart_survival(
    run: &ScenarioRun,
    bootstrap: &[MvpBootstrapEvidence],
) -> Result<MvpDaemonRestartEvidence> {
    let stopped_daemon_pid = stop_peer_daemon(run)?;
    let survivor_processes = verify_peer_data_plane_survived(run)?;
    let traffic_while_daemon_down = verify_traffic_while_peer_daemon_down(run, bootstrap)?;
    let restarted_daemon = restart_peer_daemon(run)?;
    if restarted_daemon.pid == stopped_daemon_pid {
        return Err(Error::Message(format!(
            "peer daemon restart reused stopped pid {stopped_daemon_pid}"
        )));
    }
    verify_peer_data_plane_still_running(run, &survivor_processes)?;
    let traffic_after_restart = verify_traffic_after_peer_daemon_restart(run, bootstrap)?;

    Ok(MvpDaemonRestartEvidence {
        restarted_node: "node-b",
        stopped_daemon_pid,
        restarted_daemon,
        survivor_processes,
        traffic_while_daemon_down,
        traffic_after_restart,
    })
}

fn stop_peer_daemon(run: &ScenarioRun) -> Result<String> {
    let pid = read_pid(run, "peer", DAEMON_PID)?;
    run.ssh_expect_ok_name("peer", &format!("kill -TERM {}", shell_quote(&pid)))?;
    wait_until(DATA_PLANE_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(
            "peer",
            &format!("kill -0 {} 2>/dev/null", shell_quote(&pid)),
        )?;
        Ok(!output.status.success())
    })
    .map_err(|error| {
        Error::Message(format!(
            "MVP daemon pid {pid} on peer did not stop after SIGTERM: {error}"
        ))
    })?;
    Ok(pid)
}

fn verify_peer_data_plane_survived(run: &ScenarioRun) -> Result<MvpRestartSurvivorEvidence> {
    let gateway_pid = read_pid(run, "peer", GATEWAY_PID)?;
    require_process_alive(run, "peer", &gateway_pid, "gateway")?;
    let dns_pid = read_pid(run, "peer", DNS_PID)?;
    require_process_alive(run, "peer", &dns_pid, "dns")?;
    let runtime_container = require_api_v2_container_running(run)?;
    Ok(MvpRestartSurvivorEvidence {
        gateway_pid,
        dns_pid,
        runtime_container,
    })
}

fn verify_peer_data_plane_still_running(
    run: &ScenarioRun,
    survivors: &MvpRestartSurvivorEvidence,
) -> Result<()> {
    require_process_alive(run, "peer", &survivors.gateway_pid, "gateway")?;
    require_process_alive(run, "peer", &survivors.dns_pid, "dns")?;
    let runtime = require_api_v2_container_running(run)?;
    if runtime.container_id != survivors.runtime_container.container_id {
        return Err(Error::Message(format!(
            "peer api runtime container changed across daemon restart: before={} after={}",
            survivors.runtime_container.container_id, runtime.container_id
        )));
    }
    Ok(())
}

fn verify_traffic_while_peer_daemon_down(
    run: &ScenarioRun,
    bootstrap: &[MvpBootstrapEvidence],
) -> Result<MvpRestartTrafficEvidence> {
    Ok(MvpRestartTrafficEvidence {
        gateway_http: vec![probe_gateway_http(
            run,
            "api",
            "edge",
            API_HOSTNAME,
            "ok-api-rev-2",
        )?],
        gateway_https: vec![probe_web_https(run, "edge")?],
        container_dns: probe_container_dns(run, bootstrap)?,
    })
}

fn verify_traffic_after_peer_daemon_restart(
    run: &ScenarioRun,
    bootstrap: &[MvpBootstrapEvidence],
) -> Result<MvpRestartTrafficEvidence> {
    Ok(MvpRestartTrafficEvidence {
        gateway_http: vec![
            probe_gateway_http(run, "api", "founder", API_HOSTNAME, "ok-api-rev-2")?,
            probe_gateway_http(run, "web", "peer", "web.example.test", "ok-web-rev-1")?,
        ],
        gateway_https: vec![probe_web_https(run, "peer")?, probe_web_https(run, "edge")?],
        container_dns: probe_container_dns(run, bootstrap)?,
    })
}

fn probe_gateway_http(
    run: &ScenarioRun,
    service: &'static str,
    gateway_node: &'static str,
    hostname: &'static str,
    expected_body: &'static str,
) -> Result<MvpGatewayHttpEvidence> {
    let body = wait_gateway_body(run, gateway_node, hostname, expected_body)?;
    Ok(MvpGatewayHttpEvidence {
        service,
        gateway_node,
        hostname,
        expected_body,
        body,
    })
}

fn read_pid(run: &ScenarioRun, node: &'static str, path: &str) -> Result<String> {
    let output = run.ssh_expect_ok_name(node, &format!("cat {path}"))?;
    let pid = output.stdout.trim().to_string();
    if pid.is_empty() {
        return Err(Error::Message(format!(
            "{path} on {node} did not contain a pid"
        )));
    }
    Ok(pid)
}

fn require_process_alive(
    run: &ScenarioRun,
    node: &'static str,
    pid: &str,
    label: &str,
) -> Result<()> {
    run.ssh_expect_ok_name(node, &format!("kill -0 {} 2>/dev/null", shell_quote(pid)))
        .map(|_| ())
        .map_err(|error| {
            Error::Message(format!(
                "{label} process pid {pid} on {node} was not alive: {error}"
            ))
        })
}

fn require_api_v2_container_running(run: &ScenarioRun) -> Result<MvpRuntimeContainerEvidence> {
    let output = run.ssh_expect_ok_name(
        "peer",
        &format!("docker inspect -f '{{{{.Id}}}} {{{{.State.Running}}}}' {API_V2_CONTAINER}"),
    )?;
    let fields = output.stdout.split_whitespace().collect::<Vec<_>>();
    let [container_id, running] = fields.as_slice() else {
        return Err(Error::Message(format!(
            "unexpected docker inspect output for {API_V2_CONTAINER}: {}",
            output.stdout
        )));
    };
    let running = *running == "true";
    if !running {
        return Err(Error::Message(format!(
            "{API_V2_CONTAINER} on peer was not running: {}",
            output.stdout
        )));
    }
    Ok(MvpRuntimeContainerEvidence {
        harness_node: "peer",
        service: "api",
        revision: "rev-2",
        container: API_V2_CONTAINER,
        container_id: (*container_id).to_string(),
        running,
    })
}

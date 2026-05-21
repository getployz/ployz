use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use crate::support::wait_until;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::{DAEMON_CONTROL, ISLAND, P2PANDA_PORT, STATE_DIR, shell_quote};

const DAEMON_LOG: &str = "/tmp/mvp-node-daemon.log";
const DAEMON_RUN_FOR_MS: u64 = 600_000;
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(45);
const RUNTIME_COMMAND: &str = "mkdir -p /www && echo ok-$PLOYZ_SERVICE-$PLOYZ_REVISION >/www/index.html && httpd -f -p 8080 -h /www";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpBootstrapEvidence {
    pub(crate) harness_node: &'static str,
    pub(crate) mvp_node_id: &'static str,
    pub(crate) state_dir: &'static str,
    pub(crate) p2panda_bind: String,
    pub(crate) p2panda_advertise: String,
    pub(crate) wireguard_overlay_ip: String,
    pub(crate) container_subnet: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpDaemonEvidence {
    pub(crate) harness_node: &'static str,
    pub(crate) mvp_node_id: &'static str,
    pub(crate) pid: String,
    pub(crate) imported_batches: u64,
    pub(crate) imported_operations: u64,
    pub(crate) node_agent_handlers: usize,
}

#[derive(Debug, Deserialize)]
struct BootstrapResponse {
    identity: BootstrapIdentity,
}

#[derive(Debug, Deserialize)]
struct BootstrapIdentity {
    p2panda_bind: String,
    p2panda_advertise: String,
    wireguard_overlay_ip: String,
    container_subnet: String,
}

#[derive(Debug, Deserialize)]
struct DaemonStatusResponse {
    status: String,
    node: String,
    imported_batches: u64,
    imported_operations: u64,
    node_agent_handlers: usize,
}

pub(crate) fn bootstrap_cluster(run: &ScenarioRun) -> Result<Vec<MvpBootstrapEvidence>> {
    let founder = MvpE2eNode::new(run, "founder", "node-a")?;
    let peer = MvpE2eNode::new(run, "peer", "node-b")?;
    let edge = MvpE2eNode::new(run, "edge", "node-c")?;

    let mut evidence = Vec::new();
    evidence.push(founder.bootstrap(run)?);
    let invite = founder.invite(run)?;

    for joining in [&peer, &edge] {
        joining.join(run, &invite)?;
        evidence.push(joining.bootstrap(run)?);
        let admission = joining.admission(run)?;
        founder.admit(run, &admission)?;
    }

    for node in [&founder, &peer, &edge] {
        node.status(run)?;
    }

    Ok(evidence)
}

pub(crate) fn start_cluster_daemons(run: &ScenarioRun) -> Result<Vec<MvpDaemonEvidence>> {
    let nodes = [
        MvpE2eNode::new(run, "founder", "node-a")?,
        MvpE2eNode::new(run, "peer", "node-b")?,
        MvpE2eNode::new(run, "edge", "node-c")?,
    ];
    for node in &nodes {
        node.start_daemon(run)?;
    }
    nodes
        .iter()
        .map(|node| node.wait_daemon_ready(run))
        .collect()
}

pub(super) fn restart_peer_daemon(run: &ScenarioRun) -> Result<MvpDaemonEvidence> {
    let peer = MvpE2eNode::new(run, "peer", "node-b")?;
    peer.start_daemon(run)?;
    peer.wait_restarted_daemon_ready(run)
}

struct MvpE2eNode {
    harness_node: &'static str,
    mvp_node_id: &'static str,
    advertise: String,
}

impl MvpE2eNode {
    fn new(
        run: &ScenarioRun,
        harness_node: &'static str,
        mvp_node_id: &'static str,
    ) -> Result<Self> {
        Ok(Self {
            harness_node,
            mvp_node_id,
            advertise: format!("{}:{P2PANDA_PORT}", run.node(harness_node)?.outer_ip),
        })
    }

    fn bootstrap(&self, run: &ScenarioRun) -> Result<MvpBootstrapEvidence> {
        let output = run.ssh_expect_ok_name(
            self.harness_node,
            &format!(
                "mvp-node bootstrap --state {STATE_DIR} --island {ISLAND} --node-id {} \
                 --p2panda-bind 0.0.0.0:{P2PANDA_PORT} --p2panda-advertise {}",
                self.mvp_node_id, self.advertise
            ),
        )?;
        let response: BootstrapResponse =
            serde_json::from_str(&output.stdout).map_err(|error| {
                Error::Message(format!(
                    "decode MVP bootstrap response from {}: {error}: {}",
                    self.harness_node, output.stdout
                ))
            })?;
        Ok(MvpBootstrapEvidence {
            harness_node: self.harness_node,
            mvp_node_id: self.mvp_node_id,
            state_dir: STATE_DIR,
            p2panda_bind: response.identity.p2panda_bind,
            p2panda_advertise: response.identity.p2panda_advertise,
            wireguard_overlay_ip: response.identity.wireguard_overlay_ip,
            container_subnet: response.identity.container_subnet,
        })
    }

    fn invite(&self, run: &ScenarioRun) -> Result<String> {
        let output = run.ssh_expect_ok_name(
            self.harness_node,
            &format!("mvp-node invite --state {STATE_DIR}"),
        )?;
        Ok(output.stdout.trim().to_string())
    }

    fn join(&self, run: &ScenarioRun, invite: &str) -> Result<()> {
        run.ssh_expect_ok_name(
            self.harness_node,
            &format!(
                "mvp-node join --state {STATE_DIR} --token {} --node-id {} \
                 --p2panda-bind 0.0.0.0:{P2PANDA_PORT} --p2panda-advertise {}",
                shell_quote(invite),
                self.mvp_node_id,
                self.advertise
            ),
        )?;
        Ok(())
    }

    fn admission(&self, run: &ScenarioRun) -> Result<String> {
        let output = run.ssh_expect_ok_name(
            self.harness_node,
            &format!("mvp-node admission --state {STATE_DIR}"),
        )?;
        Ok(output.stdout.trim().to_string())
    }

    fn admit(&self, run: &ScenarioRun, admission: &str) -> Result<()> {
        run.ssh_expect_ok_name(
            self.harness_node,
            &format!(
                "mvp-node admit --state {STATE_DIR} --request {}",
                shell_quote(admission)
            ),
        )?;
        Ok(())
    }

    fn status(&self, run: &ScenarioRun) -> Result<()> {
        run.ssh_expect_ok_name(
            self.harness_node,
            &format!("mvp-node status --state {STATE_DIR}"),
        )?;
        Ok(())
    }

    fn start_daemon(&self, run: &ScenarioRun) -> Result<()> {
        run.ssh_expect_ok_name(
            self.harness_node,
            &format!(
                "rm -f {control} {log}; \
                 nohup mvp-node daemon --state {state} --run-for-ms {run_for_ms} \
                 --control {control} --linux-wireguard-ifname ployz-mvp \
                 --linux-wireguard-listen-port 51820 --runtime docker \
                 --image ployz-e2e-preload/http-smoke:latest --service-port 8080 \
                 --container-command {runtime_command} \
                 > {log} 2>&1 < /dev/null & echo $! > /tmp/mvp-node-daemon.pid",
                control = DAEMON_CONTROL,
                log = DAEMON_LOG,
                state = STATE_DIR,
                run_for_ms = DAEMON_RUN_FOR_MS,
                runtime_command = shell_quote(RUNTIME_COMMAND)
            ),
        )?;
        Ok(())
    }

    fn wait_daemon_ready(&self, run: &ScenarioRun) -> Result<MvpDaemonEvidence> {
        self.wait_daemon_status(run, DaemonReadiness::InitialConvergence)
    }

    fn wait_restarted_daemon_ready(&self, run: &ScenarioRun) -> Result<MvpDaemonEvidence> {
        self.wait_daemon_status(run, DaemonReadiness::Restarted)
    }

    fn wait_daemon_status(
        &self,
        run: &ScenarioRun,
        readiness: DaemonReadiness,
    ) -> Result<MvpDaemonEvidence> {
        let mut last_output = String::new();
        let mut last_status = None;
        wait_until(DAEMON_READY_TIMEOUT, || {
            let output = run.ssh_run_name(
                self.harness_node,
                &format!("mvp-node daemon-status --control {DAEMON_CONTROL}"),
            )?;
            last_output = output.combined();
            if !output.status.success() {
                return Ok(false);
            }
            let status: DaemonStatusResponse =
                serde_json::from_str(output.stdout.trim()).map_err(|error| {
                    Error::Message(format!(
                        "decode daemon status on {}: {error}: {}",
                        self.harness_node, output.stdout
                    ))
                })?;
            let ready = status.status == "ready"
                && status.node == self.mvp_node_id
                && status.node_agent_handlers > 0
                && readiness.accepts(&status);
            last_status = Some(status);
            Ok(ready)
        })
        .map_err(|error| {
            Error::Message(format!(
                "MVP daemon on {} did not become ready: {error}\nlast output:\n{}",
                self.harness_node, last_output
            ))
        })?;
        let status = last_status.ok_or_else(|| {
            Error::Message(format!(
                "MVP daemon on {} did not report status",
                self.harness_node
            ))
        })?;
        let pid = run
            .ssh_expect_ok_name(self.harness_node, "cat /tmp/mvp-node-daemon.pid")?
            .stdout
            .trim()
            .to_string();
        Ok(MvpDaemonEvidence {
            harness_node: self.harness_node,
            mvp_node_id: self.mvp_node_id,
            pid,
            imported_batches: status.imported_batches,
            imported_operations: status.imported_operations,
            node_agent_handlers: status.node_agent_handlers,
        })
    }
}

#[derive(Clone, Copy)]
enum DaemonReadiness {
    InitialConvergence,
    Restarted,
}

impl DaemonReadiness {
    fn accepts(self, status: &DaemonStatusResponse) -> bool {
        match self {
            Self::InitialConvergence => status.imported_operations > 0,
            Self::Restarted => status.imported_batches > 0,
        }
    }
}

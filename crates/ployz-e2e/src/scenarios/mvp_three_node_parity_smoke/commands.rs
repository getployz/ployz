use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use crate::support::wait_until;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const STATE_DIR: &str = "/var/lib/ployz-mvp/node";
const DAEMON_CONTROL: &str = "/var/lib/ployz-mvp/node/control/daemon.sock";
const DAEMON_LOG: &str = "/tmp/mvp-node-daemon.log";
const ISLAND: &str = "prod";
const P2PANDA_PORT: u16 = 41_001;
const DAEMON_RUN_FOR_MS: u64 = 60_000;
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(45);
const DATA_PLANE_WAIT_TIMEOUT: Duration = Duration::from_secs(90);
const GATEWAY_PORT: u16 = 8_088;
const GATEWAY_CONTROL: &str = "/var/lib/ployz-mvp/node/control/gateway.sock";
const GATEWAY_LOG: &str = "/tmp/mvp-node-gateway.log";
const GATEWAY_SNAPSHOT: &str = "/var/lib/ployz-mvp/node/gateway.snapshot";
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

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpDeployEvidence {
    pub(crate) service: &'static str,
    pub(crate) target_node: &'static str,
    pub(crate) deploy_id: &'static str,
    pub(crate) hostname: &'static str,
    pub(crate) active_backends: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpGatewayHttpEvidence {
    pub(crate) service: &'static str,
    pub(crate) gateway_node: &'static str,
    pub(crate) hostname: &'static str,
    pub(crate) expected_body: &'static str,
    pub(crate) body: String,
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

#[derive(Debug, Deserialize)]
struct DeployResponse {
    active_backends: Vec<String>,
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

pub(crate) fn deploy_web_and_api(run: &ScenarioRun) -> Result<Vec<MvpDeployEvidence>> {
    [
        ("web", "node-a", "deploy-web", "web.example.test"),
        ("api", "node-b", "deploy-api", "api.example.test"),
    ]
    .into_iter()
    .map(|(service, target_node, deploy_id, hostname)| {
        let output = run.ssh_expect_ok_name(
            "founder",
            &format!(
                "mvp-node deploy --control {DAEMON_CONTROL} --target-node {target_node} \
                 --deploy-id {deploy_id} --service {service} --revision rev-1 --hostname {hostname}"
            ),
        )?;
        let response: DeployResponse =
            serde_json::from_str(output.stdout.trim()).map_err(|error| {
                Error::Message(format!(
                    "decode MVP deploy response for {service}: {error}: {}",
                    output.stdout
                ))
            })?;
        if response
            .active_backends
            .iter()
            .any(|backend| backend.starts_with(&format!("{target_node}@127.")))
        {
            return Err(Error::Message(format!(
                "{service} deploy reported loopback backend: {:?}",
                response.active_backends
            )));
        }
        if response
            .active_backends
            .iter()
            .all(|backend| !backend.starts_with(&format!("{target_node}@10.210.")))
        {
            return Err(Error::Message(format!(
                "{service} deploy did not report a container-subnet backend: {:?}",
                response.active_backends
            )));
        }
        Ok(MvpDeployEvidence {
            service,
            target_node,
            deploy_id,
            hostname,
            active_backends: response.active_backends,
        })
    })
    .collect()
}

pub(crate) fn verify_gateway_http(run: &ScenarioRun) -> Result<Vec<MvpGatewayHttpEvidence>> {
    for node in ["founder", "peer", "edge"] {
        wait_gateway_snapshot_hosts(run, node, &["web.example.test", "api.example.test"])?;
        start_gateway(run, node)?;
    }
    [
        ("web", "peer", "web.example.test", "ok-web-rev-1"),
        ("web", "edge", "web.example.test", "ok-web-rev-1"),
        ("api", "founder", "api.example.test", "ok-api-rev-1"),
        ("api", "edge", "api.example.test", "ok-api-rev-1"),
    ]
    .into_iter()
    .map(|(service, gateway_node, hostname, expected_body)| {
        let body = wait_gateway_body(run, gateway_node, hostname, expected_body)?;
        Ok(MvpGatewayHttpEvidence {
            service,
            gateway_node,
            hostname,
            expected_body,
            body,
        })
    })
    .collect()
}

fn wait_gateway_snapshot_hosts(
    run: &ScenarioRun,
    node: &'static str,
    expected_hosts: &[&str],
) -> Result<()> {
    let mut last_output = String::new();
    wait_until(DATA_PLANE_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node, &format!("cat {GATEWAY_SNAPSHOT}"))?;
        last_output = output.combined();
        if !output.status.success() {
            return Ok(false);
        }
        Ok(snapshot_contains_hosts(&output.stdout, expected_hosts))
    })
    .map_err(|error| {
        Error::Message(format!(
            "gateway snapshot on {node} did not contain hosts {:?}: {error}\nlast output:\n{last_output}",
            expected_hosts
        ))
    })
}

fn snapshot_contains_hosts(snapshot: &str, expected_hosts: &[&str]) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(snapshot) else {
        return false;
    };
    expected_hosts.iter().all(|expected| {
        value
            .get("routes")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|route| {
                route
                    .get("hostnames")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .any(|host| host.as_str() == Some(*expected))
    })
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
                && status.imported_operations > 0;
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

fn start_gateway(run: &ScenarioRun, node: &'static str) -> Result<()> {
    run.ssh_expect_ok_name(
        node,
        &format!(
            "rm -f {control} {log}; \
             nohup mvp-node gateway --state {state} --listen 0.0.0.0:{port} \
             --control {control} > {log} 2>&1 < /dev/null & echo $! > /tmp/mvp-node-gateway.pid",
            control = GATEWAY_CONTROL,
            log = GATEWAY_LOG,
            state = STATE_DIR,
            port = GATEWAY_PORT
        ),
    )?;
    Ok(())
}

fn wait_gateway_body(
    run: &ScenarioRun,
    gateway_node: &'static str,
    hostname: &'static str,
    expected_body: &'static str,
) -> Result<String> {
    let command = format!(
        "curl -fsS -H {} http://127.0.0.1:{GATEWAY_PORT}/",
        shell_quote(&format!("Host: {hostname}"))
    );
    let mut last_output = String::new();
    let mut body = String::new();
    wait_until(DATA_PLANE_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(gateway_node, &command)?;
        last_output = output.combined();
        body = output.stdout.trim().to_string();
        Ok(output.status.success() && body == expected_body)
    })
    .map_err(|error| {
        Error::Message(format!(
            "gateway {gateway_node} did not serve {hostname} body {expected_body}: {error}\nlast output:\n{last_output}"
        ))
    })?;
    Ok(body)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::{shell_quote, snapshot_contains_hosts};

    #[test]
    fn shell_quote_preserves_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    #[test]
    fn snapshot_host_probe_requires_all_hosts() {
        let snapshot = r#"{
            "routes": [
                { "hostnames": ["web.example.test"] },
                { "hostnames": ["api.example.test"] }
            ]
        }"#;

        assert!(snapshot_contains_hosts(
            snapshot,
            &["web.example.test", "api.example.test"]
        ));
        assert!(!snapshot_contains_hosts(
            snapshot,
            &["web.example.test", "echo.example.test"]
        ));
    }
}

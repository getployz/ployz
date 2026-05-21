use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use serde::{Deserialize, Serialize};

const STATE_DIR: &str = "/var/lib/ployz-mvp/node";
const ISLAND: &str = "prod";
const P2PANDA_PORT: u16 = 41_001;

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
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn shell_quote_preserves_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }
}

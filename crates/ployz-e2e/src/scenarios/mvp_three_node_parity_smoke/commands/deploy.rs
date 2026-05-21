use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use serde::{Deserialize, Serialize};

use super::DAEMON_CONTROL;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpDeployEvidence {
    pub(crate) service: &'static str,
    pub(crate) target_node: &'static str,
    pub(crate) deploy_id: &'static str,
    pub(crate) hostname: &'static str,
    pub(crate) active_backends: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DeployResponse {
    active_backends: Vec<String>,
}

pub(crate) fn deploy_web_api_and_echo(run: &ScenarioRun) -> Result<Vec<MvpDeployEvidence>> {
    [
        ("web", "node-a", "deploy-web", "web.example.test"),
        ("api", "node-b", "deploy-api", "api.example.test"),
        ("echo", "node-c", "deploy-echo", "echo.example.test"),
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

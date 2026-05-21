use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use crate::support::wait_until;
use serde::{Deserialize, Serialize};

use super::serving::{API_HOSTNAME, reload_gateway, wait_gateway_body};
use super::{DAEMON_CONTROL, DATA_PLANE_WAIT_TIMEOUT, STATE_DIR};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpDeployEvidence {
    pub(crate) service: &'static str,
    pub(crate) target_node: &'static str,
    pub(crate) deploy_id: &'static str,
    pub(crate) hostname: &'static str,
    pub(crate) active_backends: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpUpdateDrainEvidence {
    pub(crate) service: &'static str,
    pub(crate) deploy_id: &'static str,
    pub(crate) previous_revision: &'static str,
    pub(crate) updated_revision: &'static str,
    pub(crate) active_backends: Vec<String>,
    pub(crate) old_backends_to_drain: Vec<String>,
    pub(crate) status_phases: Vec<String>,
    pub(crate) gateway_http: Vec<MvpUpdateGatewayEvidence>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MvpUpdateGatewayEvidence {
    pub(crate) gateway_node: &'static str,
    pub(crate) hostname: &'static str,
    pub(crate) expected_body: &'static str,
    pub(crate) body: String,
}

#[derive(Debug, Deserialize)]
struct DeployResponse {
    active_backends: Vec<String>,
    #[serde(default)]
    old_backends_to_drain: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DeployStatusResponse {
    statuses: Vec<DeployStatusEntry>,
}

#[derive(Debug, Deserialize)]
struct DeployStatusEntry {
    phase: String,
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

pub(crate) fn update_api_and_verify_drain(run: &ScenarioRun) -> Result<MvpUpdateDrainEvidence> {
    let output = run.ssh_expect_ok_name(
        "founder",
        &format!(
            "mvp-node deploy --control {DAEMON_CONTROL} --target-node node-b \
             --deploy-id deploy-api-v2 --service api --revision rev-2 --hostname {API_HOSTNAME}"
        ),
    )?;
    let response: DeployResponse = serde_json::from_str(output.stdout.trim()).map_err(|error| {
        Error::Message(format!(
            "decode MVP api update response: {error}: {}",
            output.stdout
        ))
    })?;
    require_container_backend("api update", "node-b", &response.active_backends)?;
    if response.old_backends_to_drain.len() != 1 {
        return Err(Error::Message(format!(
            "api update did not report exactly one old backend to drain: {:?}",
            response.old_backends_to_drain
        )));
    }
    if !response.old_backends_to_drain[0].starts_with("node-b@10.210.") {
        return Err(Error::Message(format!(
            "api update old backend was not a node-b container backend: {:?}",
            response.old_backends_to_drain
        )));
    }
    if response.old_backends_to_drain[0] == response.active_backends[0] {
        return Err(Error::Message(format!(
            "api update kept the old backend active: active={:?} old={:?}",
            response.active_backends, response.old_backends_to_drain
        )));
    }
    let status_phases = wait_deploy_cleanup_done(run, "deploy-api-v2")?;
    let gateway_http = [
        ("founder", API_HOSTNAME, "ok-api-rev-2"),
        ("edge", API_HOSTNAME, "ok-api-rev-2"),
    ]
    .into_iter()
    .map(|(gateway_node, hostname, expected_body)| {
        let body = wait_gateway_body_after_reload(run, gateway_node, hostname, expected_body)?;
        Ok(MvpUpdateGatewayEvidence {
            gateway_node,
            hostname,
            expected_body,
            body,
        })
    })
    .collect::<Result<Vec<_>>>()?;

    Ok(MvpUpdateDrainEvidence {
        service: "api",
        deploy_id: "deploy-api-v2",
        previous_revision: "rev-1",
        updated_revision: "rev-2",
        active_backends: response.active_backends,
        old_backends_to_drain: response.old_backends_to_drain,
        status_phases,
        gateway_http,
    })
}

fn wait_gateway_body_after_reload(
    run: &ScenarioRun,
    gateway_node: &'static str,
    hostname: &'static str,
    expected_body: &'static str,
) -> Result<String> {
    let mut body = String::new();
    let mut last_error = None;
    wait_until(DATA_PLANE_WAIT_TIMEOUT, || {
        if let Err(error) = reload_gateway(run, gateway_node) {
            last_error = Some(error.to_string());
            return Ok(false);
        }
        match wait_gateway_body(run, gateway_node, hostname, expected_body) {
            Ok(value) => {
                body = value;
                Ok(true)
            }
            Err(error) => {
                last_error = Some(error.to_string());
                Ok(false)
            }
        }
    })
    .map_err(|error| {
        Error::Message(format!(
            "gateway {gateway_node} did not serve updated {hostname} body {expected_body}: {error}; last_error={}",
            last_error.unwrap_or_else(|| "none".to_string())
        ))
    })?;
    Ok(body)
}

fn wait_deploy_cleanup_done(run: &ScenarioRun, deploy_id: &str) -> Result<Vec<String>> {
    let mut phases = Vec::new();
    let mut last_output = String::new();
    wait_until(DATA_PLANE_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(
            "founder",
            &format!("mvp-node deploy-status --state {STATE_DIR} --deploy-id {deploy_id}"),
        )?;
        last_output = output.combined();
        if !output.status.success() {
            return Ok(false);
        }
        let response: DeployStatusResponse = serde_json::from_str(output.stdout.trim())
            .map_err(|error| Error::Message(format!("decode deploy status: {error}")))?;
        phases = response
            .statuses
            .into_iter()
            .map(|status| status.phase)
            .collect::<Vec<_>>();
        Ok(phases.iter().any(|phase| phase == "cleanup_done"))
    })
    .map_err(|error| {
        Error::Message(format!(
            "deploy {deploy_id} did not reach CleanupDone: {error}; last output:\n{last_output}"
        ))
    })?;
    Ok(phases)
}

fn require_container_backend(
    label: &str,
    target_node: &str,
    active_backends: &[String],
) -> Result<()> {
    if active_backends
        .iter()
        .any(|backend| backend.starts_with(&format!("{target_node}@127.")))
    {
        return Err(Error::Message(format!(
            "{label} reported loopback backend: {active_backends:?}"
        )));
    }
    if active_backends
        .iter()
        .all(|backend| !backend.starts_with(&format!("{target_node}@10.210.")))
    {
        return Err(Error::Message(format!(
            "{label} did not report a container-subnet backend: {active_backends:?}"
        )));
    }
    Ok(())
}

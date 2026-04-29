use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::{DaemonJsonPayload, parse_daemon_json_response, wait_until};
use std::time::Duration;

const DOCTOR_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.machine_add("founder", "peer")?;

    let active = [
        MachineExpectation {
            id: "founder",
            lifecycle: "active",
            subnet: SubnetExpectation::Present,
        },
        MachineExpectation {
            id: "peer",
            lifecycle: "active",
            subnet: SubnetExpectation::Present,
        },
    ];
    run.wait_machine_rows("founder", &active)?;
    run.wait_machine_rows("peer", &active)?;
    run.wait_mesh_ready_name("peer")?;
    wait_for_healthy_peer(run, "founder", "peer")?;
    wait_for_healthy_peer(run, "peer", "founder")?;

    run.mesh_stop("peer", true)?;
    run.mesh_start("peer", "alpha")?;
    run.wait_mesh_ready_name("peer")?;
    wait_for_healthy_peer(run, "founder", "peer")?;
    wait_for_healthy_peer(run, "peer", "founder")?;

    run.hard_reboot_node("peer")?;
    run.wait_mesh_ready_name("peer")?;
    wait_for_healthy_peer(run, "founder", "peer")?;
    wait_for_healthy_peer(run, "peer", "founder")?;

    run.hard_reboot_node("founder")?;
    run.wait_mesh_ready_name("founder")?;
    wait_for_healthy_peer(run, "founder", "peer")?;
    wait_for_healthy_peer(run, "peer", "founder")?;
    Ok(())
}

fn wait_for_healthy_peer(run: &ScenarioRun, node_name: &str, peer_name: &str) -> Result<()> {
    let mut last_report = String::new();
    wait_until(DOCTOR_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, "ployzd --json doctor")?;
        if !output.status.success() {
            last_report = output.combined();
            return Ok(false);
        }
        last_report = output.stdout;
        doctor_has_healthy_peer(&last_report, peer_name)
    })
    .map_err(|error| {
        Error::Message(format!(
            "doctor on {node_name} did not report peer '{peer_name}' healthy: {error}\nlast report:\n{last_report}"
        ))
    })
}

fn doctor_has_healthy_peer(report: &str, peer_name: &str) -> Result<bool> {
    let response = parse_daemon_json_response(report)?;
    if !response.ok {
        return Ok(false);
    }
    let Some(DaemonJsonPayload::Doctor(payload)) = response.payload else {
        return Err(Error::Message(String::from(
            "doctor response missing doctor payload",
        )));
    };
    Ok(payload.overall.lifecycle == "healthy"
        && payload.peers.iter().any(|peer| {
            peer.machine_id == peer_name
                && !peer.blocking
                && peer.store_lifecycle == "active"
                && peer.wg_state != "absent"
                && (peer.corrosion_state == "alive" || peer.wg_state == "fresh")
        }))
}

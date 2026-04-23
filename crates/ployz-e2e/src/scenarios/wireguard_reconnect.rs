use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::{DaemonJsonPayload, parse_daemon_json_response, wait_until};
use std::time::Duration;

const DOCTOR_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    let founder_side = ["founder"];
    let peer_side = ["peer"];

    run.log_progress("mesh init founder");
    run.mesh_init("founder", "alpha")?;
    run.log_progress("wait founder mesh ready");
    run.wait_mesh_ready_name("founder")?;

    run.log_progress("add peer from founder");
    run.machine_add("founder", "peer")?;
    run.log_progress("wait founder+peer enabled");
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;
    run.log_progress("wait peer mesh ready");
    run.wait_mesh_ready_name("peer")?;

    run.log_progress("wait initial peer connectivity");
    wait_for_doctor_peer_status(run, "founder", "peer", "healthy", "reachable")?;
    wait_for_doctor_peer_status(run, "peer", "founder", "healthy", "reachable")?;

    run.log_progress("install partition");
    run.partition_groups(&founder_side, &peer_side)?;
    run.log_progress("wait peer connectivity to drop");
    wait_for_doctor_peer_status(run, "founder", "peer", "blocked", "unreachable")?;
    wait_for_doctor_peer_status(run, "peer", "founder", "blocked", "unreachable")?;

    run.log_progress("clear partition");
    run.clear_partition_rules()?;
    run.log_progress("wait founder+peer enabled again");
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;
    run.log_progress("wait peer connectivity to reconnect");
    wait_for_doctor_peer_status(run, "founder", "peer", "healthy", "reachable")?;
    wait_for_doctor_peer_status(run, "peer", "founder", "healthy", "reachable")?;
    run.log_progress("scenario complete");
    Ok(())
}

fn wait_for_doctor_peer_status(
    run: &ScenarioRun,
    node_name: &str,
    peer_name: &str,
    participation: &str,
    probe_status: &str,
) -> Result<()> {
    let mut last_report = String::new();

    wait_until(DOCTOR_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, "ployzd --json doctor")?;
        if !output.status.success() {
            last_report = output.combined();
            return Ok(false);
        }

        last_report = output.stdout;
        Ok(doctor_report_matches(&last_report, peer_name, participation, probe_status)?)
    })
    .map_err(|error| {
        Error::Message(format!(
            "doctor on {node_name} did not report peer '{peer_name}' as participation={participation} probe={probe_status}: {error}\nlast report:\n{last_report}"
        ))
    })
}

fn doctor_report_matches(
    report: &str,
    peer_name: &str,
    participation: &str,
    probe_status: &str,
) -> Result<bool> {
    let response = parse_daemon_json_response(report)?;
    if !response.ok {
        return Ok(false);
    }
    let Some(DaemonJsonPayload::Doctor(payload)) = response.payload else {
        return Err(Error::Message(String::from(
            "doctor response missing doctor payload",
        )));
    };
    if payload.overall.participation != participation {
        return Ok(false);
    }

    Ok(payload.peers.iter().any(|peer| {
        let blocking_matches = match participation {
            "blocked" => peer.blocking,
            "healthy" => !peer.blocking,
            _ => false,
        };
        peer.machine_id == peer_name
            && blocking_matches
            && peer.store_participation == "enabled"
            && peer.store_status == "up"
            && peer.probe_state == probe_status
    }))
}

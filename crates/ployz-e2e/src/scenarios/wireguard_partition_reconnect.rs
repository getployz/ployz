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
    run.log_progress("wait founder+peer active");
    run.wait_machine_rows(
        "founder",
        &[
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
        ],
    )?;
    run.log_progress("wait peer mesh ready");
    run.wait_mesh_ready_name("peer")?;

    run.log_progress("wait initial peer connectivity");
    wait_for_doctor_peer_status(run, "founder", "peer", "healthy", "active", "reachable")?;
    wait_for_doctor_peer_status(run, "peer", "founder", "healthy", "active", "reachable")?;

    run.log_progress("install partition");
    run.partition_groups(&founder_side, &peer_side)?;
    run.log_progress("wait peer connectivity to drop");
    wait_for_doctor_peer_status(run, "founder", "peer", "blocked", "active", "unreachable")?;

    run.log_progress("clear partition");
    run.clear_partition_rules()?;
    run.log_progress("wait founder+peer active again");
    run.wait_machine_rows(
        "founder",
        &[
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
        ],
    )?;
    run.log_progress("wait peer connectivity to reconnect");
    wait_for_doctor_peer_status(run, "founder", "peer", "healthy", "active", "reachable")?;
    wait_for_doctor_peer_status(run, "peer", "founder", "healthy", "active", "reachable")?;
    run.log_progress("scenario complete");
    Ok(())
}

fn wait_for_doctor_peer_status(
    run: &ScenarioRun,
    node_name: &str,
    peer_name: &str,
    overall_lifecycle: &str,
    peer_lifecycle: &str,
    _legacy_probe_status: &str,
) -> Result<()> {
    let mut last_report = String::new();

    wait_until(DOCTOR_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, "ployzd --json doctor")?;
        if !output.status.success() {
            last_report = output.combined();
            return Ok(false);
        }

        last_report = output.stdout;
        doctor_report_matches(
            &last_report,
            peer_name,
            overall_lifecycle,
            peer_lifecycle,
        )
    })
    .map_err(|error| {
        Error::Message(format!(
            "doctor on {node_name} did not report peer '{peer_name}' as overall={overall_lifecycle} lifecycle={peer_lifecycle}: {error}\nlast report:\n{last_report}"
        ))
    })
}

fn doctor_report_matches(
    report: &str,
    peer_name: &str,
    overall_lifecycle: &str,
    peer_lifecycle: &str,
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
    if payload.overall.lifecycle != overall_lifecycle {
        return Ok(false);
    }

    Ok(payload.peers.iter().any(|peer| {
        let blocking_matches = match overall_lifecycle {
            "blocked" => peer.blocking,
            "healthy" => !peer.blocking,
            _ => false,
        };
        peer.machine_id == peer_name
            && blocking_matches
            && peer.store_lifecycle == peer_lifecycle
            && peer.wg_state != "absent"
            && peer.probe_state == "not-used"
            && match overall_lifecycle {
                "healthy" => peer.wg_state == "fresh",
                "blocked" => peer.wg_state != "fresh",
                _ => false,
            }
    }))
}

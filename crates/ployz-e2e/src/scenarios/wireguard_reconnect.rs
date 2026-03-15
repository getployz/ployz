use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use crate::support::wait_until;
use std::time::Duration;

const DOCTOR_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    let founder_side = ["founder"];
    let peer_side = ["peer"];
    let nodes = ["founder", "peer"];

    run.log_progress("mesh init founder");
    run.mesh_init("founder", "alpha")?;
    run.log_progress("wait founder mesh ready");
    run.wait_mesh_ready_name("founder")?;

    run.log_progress("add peer from founder");
    run.machine_add("founder", "peer")?;
    run.log_progress("wait founder+peer enabled");
    run.wait_for_settled_machine_states("founder", &[("founder", "enabled"), ("peer", "enabled")])?;
    run.log_progress("wait peer mesh ready");
    run.wait_mesh_ready_name("peer")?;

    run.log_progress("wait initial peer connectivity");
    wait_for_doctor_peer_status(run, "founder", "peer", "healthy", "reachable")?;
    wait_for_doctor_peer_status(run, "peer", "founder", "healthy", "reachable")?;

    run.log_progress("install partition");
    run.partition_groups(&founder_side, &peer_side)?;
    run.log_progress("tick partitioned nodes");
    run.tick_nodes(&nodes, 3)?;
    run.log_progress("wait peer connectivity to drop");
    wait_for_doctor_peer_status(run, "founder", "peer", "blocked", "unreachable")?;
    wait_for_doctor_peer_status(run, "peer", "founder", "blocked", "unreachable")?;

    run.log_progress("clear partition");
    run.clear_partition_rules()?;
    run.log_progress("tick healed nodes");
    run.tick_nodes(&nodes, 3)?;
    run.log_progress("wait founder+peer enabled again");
    run.wait_for_settled_machine_states_with_ticks(
        "founder",
        &[("founder", "enabled"), ("peer", "enabled")],
        &nodes,
        3,
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
        let output = run.ssh_run_name(node_name, "ployzd doctor")?;
        if !output.status.success() {
            last_report = output.combined();
            return Ok(false);
        }

        last_report = output.stdout;
        Ok(doctor_report_matches(
            &last_report,
            peer_name,
            participation,
            probe_status,
        ))
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
) -> bool {
    if !report.contains(&format!("participation: {participation}")) {
        return false;
    }

    report.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with(peer_name)
            && trimmed.contains("store=enabled/fresh")
            && trimmed.contains(&format!("probe={probe_status}"))
    })
}

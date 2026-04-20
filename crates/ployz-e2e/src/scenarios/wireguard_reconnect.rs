use crate::error::Result;
use crate::runner::ScenarioRun;
use ployz_api::PeerProbeStatus;
use std::time::Duration;

const PEER_HEALTH_WAIT_TIMEOUT: Duration = Duration::from_secs(45);

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    let founder_side = ["founder"];
    let peer_side = ["peer"];
    let nodes = ["founder", "peer"];

    run.log_progress("mesh init founder");
    run.mesh_init("founder", "alpha")?;
    run.log_progress("wait founder node ready");
    run.wait_node_ready_name("founder")?;

    run.log_progress("add peer from founder");
    run.machine_add("founder", "peer")?;
    run.log_progress("wait founder+peer active");
    run.wait_for_settled_machine_states("founder", &[("founder", "active"), ("peer", "active")])?;
    run.log_progress("wait peer node ready");
    run.wait_node_ready_name("peer")?;

    run.log_progress("wait initial peer connectivity");
    run.wait_peer_health_name(
        "founder",
        "peer",
        true,
        PeerProbeStatus::Reachable,
        PEER_HEALTH_WAIT_TIMEOUT,
    )?;

    run.log_progress("install partition");
    run.partition_groups(&founder_side, &peer_side)?;
    run.log_progress("tick partitioned nodes");
    run.tick_nodes(&nodes, 3)?;
    run.log_progress("wait peer connectivity to drop");
    run.wait_peer_health_name(
        "founder",
        "peer",
        false,
        PeerProbeStatus::Unreachable,
        PEER_HEALTH_WAIT_TIMEOUT,
    )?;

    run.log_progress("clear partition");
    run.clear_partition_rules()?;
    run.log_progress("tick healed nodes");
    run.tick_nodes(&nodes, 3)?;
    run.log_progress("wait founder+peer active again");
    run.wait_for_settled_machine_states_with_ticks(
        "founder",
        &[("founder", "active"), ("peer", "active")],
        &nodes,
        3,
    )?;
    run.log_progress("wait peer connectivity to reconnect");
    run.wait_peer_health_name(
        "founder",
        "peer",
        true,
        PeerProbeStatus::Reachable,
        PEER_HEALTH_WAIT_TIMEOUT,
    )?;
    run.log_progress("scenario complete");
    Ok(())
}

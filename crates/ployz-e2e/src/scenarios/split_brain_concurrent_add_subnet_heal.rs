use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    let founder_side = ["founder", "joiner1", "joiner2", "joiner3"];
    let peer_side = ["peer", "joiner4", "joiner5", "joiner6"];
    let all_nodes = [
        "founder", "peer", "joiner1", "joiner2", "joiner3", "joiner4", "joiner5", "joiner6",
    ];

    run.log_progress("mesh init founder");
    run.mesh_init("founder", "alpha")?;
    run.log_progress("wait founder mesh ready");
    run.wait_mesh_ready_name("founder")?;

    run.log_progress("add peer from founder");
    run.machine_add("founder", "peer")?;
    run.log_progress("wait founder+peer enabled");
    run.wait_for_settled_machine_states("founder", &[("founder", "enabled"), ("peer", "enabled")])?;

    run.log_progress("install partition");
    run.partition_groups(&founder_side, &peer_side)?;

    run.log_progress("concurrent add founder-side and peer-side joiners");
    let founder_add = run.machine_add_command(&["joiner1", "joiner2", "joiner3"])?;
    let peer_add = run.machine_add_command(&["joiner4", "joiner5", "joiner6"])?;
    run.ssh_expect_ok_concurrent(&[("founder", founder_add), ("peer", peer_add)])?;
    run.log_progress("tick founder side");
    run.tick_nodes(&founder_side, 3)?;
    run.log_progress("tick peer side");
    run.tick_nodes(&peer_side, 3)?;

    run.log_progress("wait founder-side machine ids with subnets");
    run.wait_for_machine_ids_with_subnets(
        "founder",
        &["founder", "peer", "joiner1", "joiner2", "joiner3"],
    )?;
    run.log_progress("wait peer-side machine ids with subnets");
    run.wait_for_machine_ids_with_subnets(
        "peer",
        &["founder", "peer", "joiner4", "joiner5", "joiner6"],
    )?;

    run.log_progress("clear partition");
    run.clear_partition_rules()?;
    run.log_progress("tick all nodes after clear");
    run.tick_nodes(&all_nodes, 3)?;
    run.log_progress("wait founder to see all machine ids with subnets");
    run.wait_for_machine_ids_with_subnets("founder", &all_nodes)?;
    run.log_progress("wait peer to see all machine ids with subnets");
    run.wait_for_machine_ids_with_subnets("peer", &all_nodes)?;

    run.log_progress("wait unique subnets");
    run.wait_for_unique_machine_subnets_with_ticks("founder", &all_nodes, 3)?;
    run.log_progress("wait all nodes enabled");
    run.wait_for_settled_machine_states_with_ticks(
        "founder",
        &[
            ("founder", "enabled"),
            ("peer", "enabled"),
            ("joiner1", "enabled"),
            ("joiner2", "enabled"),
            ("joiner3", "enabled"),
            ("joiner4", "enabled"),
            ("joiner5", "enabled"),
            ("joiner6", "enabled"),
        ],
        &all_nodes,
        3,
    )?;
    run.log_progress("scenario complete");
    Ok(())
}

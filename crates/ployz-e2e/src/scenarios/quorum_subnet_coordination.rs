use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    let all_nodes: &[&str] = &["founder", "peer1", "peer2", "joiner1", "joiner2"];

    // Phase 1: Setup a 3-node cluster
    run.log_progress("mesh init founder");
    run.mesh_init("founder", "alpha")?;
    run.log_progress("wait founder mesh ready");
    run.wait_mesh_ready_name("founder")?;

    run.log_progress("add peer1 from founder");
    run.machine_add("founder", "peer1")?;
    run.log_progress("add peer2 from founder");
    run.machine_add("founder", "peer2")?;

    run.log_progress("tick all nodes");
    run.tick_nodes(&["founder", "peer1", "peer2"], 3)?;
    run.log_progress("wait all three active");
    run.wait_for_settled_machine_states_with_ticks(
        "founder",
        &[
            ("founder", "active"),
            ("peer1", "active"),
            ("peer2", "active"),
        ],
        &["founder", "peer1", "peer2"],
        3,
    )?;

    // Phase 2: Equal split — founder alone vs peer1+peer2.
    // Quorum is 2 of 3 — founder (only self online) cannot allocate.
    run.log_progress("install equal partition: [founder] vs [peer1, peer2]");
    run.partition_groups(&["founder"], &["peer1", "peer2"])?;

    run.log_progress("expect machine add to fail on founder (no quorum)");
    run.expect_machine_add_fails("founder", "joiner1")?;

    run.log_progress("clear partition");
    run.clear_partition_rules()?;
    run.log_progress("tick all after equal split");
    run.tick_nodes(&["founder", "peer1", "peer2"], 3)?;

    // Phase 3: Majority split — [founder, peer1] vs [peer2].
    // Quorum is 2 of 3 — founder+peer1 have majority, peer2 alone does not.
    run.log_progress("install majority partition: [founder, peer1] vs [peer2]");
    run.partition_groups(&["founder", "peer1"], &["peer2"])?;

    run.log_progress("add joiner1 from founder (quorum met: founder+peer1)");
    run.machine_add("founder", "joiner1")?;

    run.log_progress("expect machine add to fail on peer2 (no quorum)");
    run.expect_machine_add_fails("peer2", "joiner2")?;

    run.log_progress("clear partition");
    run.clear_partition_rules()?;

    // Phase 4: Post-partition — full cluster available.
    run.log_progress("tick all nodes after partition clear");
    run.tick_nodes(&["founder", "peer1", "peer2", "joiner1"], 3)?;

    run.log_progress("add joiner2 from founder (full quorum)");
    run.machine_add("founder", "joiner2")?;

    run.log_progress("tick all nodes");
    run.tick_nodes(all_nodes, 3)?;

    run.log_progress("assert unique subnets (invariant — no healing needed)");
    run.assert_unique_machine_subnets("founder")?;

    run.log_progress("wait all nodes active");
    run.wait_for_settled_machine_states_with_ticks(
        "founder",
        &[
            ("founder", "active"),
            ("peer1", "active"),
            ("peer2", "active"),
            ("joiner1", "active"),
            ("joiner2", "active"),
        ],
        all_nodes,
        3,
    )?;

    run.log_progress("scenario complete");
    Ok(())
}

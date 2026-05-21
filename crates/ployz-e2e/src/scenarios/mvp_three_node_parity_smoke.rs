use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use serde::Serialize;

mod commands;

const NODES: &[&str] = &["founder", "peer", "edge"];

#[derive(Debug, Serialize)]
struct MvpParityPayloadReport {
    scenario: &'static str,
    nodes: Vec<MvpNodeInstallEvidence>,
    bootstrap: Vec<commands::MvpBootstrapEvidence>,
    daemons: Vec<commands::MvpDaemonEvidence>,
}

#[derive(Debug, Serialize)]
struct MvpNodeInstallEvidence {
    node: &'static str,
    binary: &'static str,
    help_contains: &'static str,
}

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    let mut nodes = Vec::new();
    for node in NODES {
        verify_mvp_node_payload(run, node)?;
        nodes.push(MvpNodeInstallEvidence {
            node,
            binary: "/root/.local/bin/mvp-node",
            help_contains: "mvp-node <command> [options]",
        });
    }
    let bootstrap = commands::bootstrap_cluster(run)?;
    let daemons = commands::start_cluster_daemons(run)?;
    write_report(
        run,
        &MvpParityPayloadReport {
            scenario: "mvp_three_node_parity_smoke",
            nodes,
            bootstrap,
            daemons,
        },
    )
}

fn verify_mvp_node_payload(run: &ScenarioRun, node: &str) -> Result<()> {
    run.ssh_expect_ok_name(
        node,
        "test -x /root/.local/bin/mvp-node && test -x /usr/local/bin/mvp-node",
    )?;
    let output = run.ssh_expect_ok_name(node, "mvp-node --help")?;
    if !output.stdout.contains("mvp-node <command> [options]") {
        return Err(Error::Message(format!(
            "mvp-node payload help on {node} did not look like the MVP CLI: {}",
            output.stdout
        )));
    }
    Ok(())
}

fn write_report(_run: &ScenarioRun, report: &MvpParityPayloadReport) -> Result<()> {
    let json = serde_json::to_string_pretty(report)
        .map_err(|error| Error::Message(format!("encode MVP parity report: {error}")))?;
    println!("{json}");
    Ok(())
}

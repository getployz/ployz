use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{BusSession, FactPayload, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore, SharedPandaFactStore};
use mvp_p2panda_transport::{
    PandaNetFactImportReport, PandaNetFactNode, PandaNetFactNodeConfig, PandaNetNetworkId,
    PandaNetNodeConfig, PandaNetNodeInfo, PandaNetNodeSeed, PandaNetTopic, PandaNetTransportError,
};
use serde::Serialize;

use crate::bus_syntax::{fact_key, fact_pattern};
use crate::metrics::{reset_dir, scenario_dir, write_json};

const ITERATIONS: usize = 12;
const IMPORT_DEADLINE: Duration = Duration::from_secs(8);
const IMPORT_IDLE: Duration = Duration::from_millis(250);

#[derive(Debug, Serialize)]
struct P2pandaNetFactNodeReliabilityReport {
    scenario: &'static str,
    iterations: usize,
    zero_import_iterations: usize,
    total_attempted_imports: usize,
    total_imported_operations: usize,
    total_idle_timeouts: u64,
    total_stream_refreshes: u64,
    total_stream_refresh_failures: u64,
    total_stream_ended: u64,
    total_stream_lagged: u64,
    total_stream_failed: u64,
    total_replayed_operations_skipped: u64,
    total_session_started: u64,
    total_sync_started: u64,
    total_sync_finished: u64,
    total_live_mode_started: u64,
    total_session_finished: u64,
    startup_p95_ms: u128,
    import_p95_ms: u128,
    elapsed_ms: u128,
    iteration_reports: Vec<P2pandaNetFactNodeReliabilityIteration>,
}

#[derive(Debug, Clone, Serialize)]
struct P2pandaNetFactNodeReliabilityIteration {
    iteration: usize,
    attempted_imports: usize,
    imported_operations: usize,
    idle_timeouts: u64,
    stream_refreshes: u64,
    stream_refresh_failures: u64,
    stream_ended: u64,
    stream_lagged: u64,
    stream_failed: u64,
    replayed_operations_skipped: u64,
    session_started: u64,
    sync_started: u64,
    sync_finished: u64,
    live_mode_started: u64,
    session_finished: u64,
    startup_ms: u128,
    import_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            format!("create tokio runtime for p2panda-net fact-node reliability: {error}")
        })?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("p2panda-net-fact-node-reliability-contract");
    reset_dir(&root)?;
    let mut iterations = Vec::with_capacity(ITERATIONS);

    for iteration in 0..ITERATIONS {
        iterations.push(run_iteration(iteration).await?);
    }

    let zero_import_iterations = iterations
        .iter()
        .filter(|iteration| iteration.attempted_imports == 0)
        .count();
    if zero_import_iterations != 0 {
        return Err(format!(
            "p2panda-net reliability saw {zero_import_iterations} zero-import iterations"
        ));
    }

    let mut startup_ms = iterations
        .iter()
        .map(|iteration| iteration.startup_ms)
        .collect::<Vec<_>>();
    let mut import_ms = iterations
        .iter()
        .map(|iteration| iteration.import_ms)
        .collect::<Vec<_>>();
    let report = P2pandaNetFactNodeReliabilityReport {
        scenario: "p2panda-net-fact-node-reliability-contract",
        iterations: ITERATIONS,
        zero_import_iterations,
        total_attempted_imports: iterations
            .iter()
            .map(|iteration| iteration.attempted_imports)
            .sum(),
        total_imported_operations: iterations
            .iter()
            .map(|iteration| iteration.imported_operations)
            .sum(),
        total_idle_timeouts: iterations
            .iter()
            .map(|iteration| iteration.idle_timeouts)
            .sum(),
        total_stream_refreshes: iterations
            .iter()
            .map(|iteration| iteration.stream_refreshes)
            .sum(),
        total_stream_refresh_failures: iterations
            .iter()
            .map(|iteration| iteration.stream_refresh_failures)
            .sum(),
        total_stream_ended: iterations
            .iter()
            .map(|iteration| iteration.stream_ended)
            .sum(),
        total_stream_lagged: iterations
            .iter()
            .map(|iteration| iteration.stream_lagged)
            .sum(),
        total_stream_failed: iterations
            .iter()
            .map(|iteration| iteration.stream_failed)
            .sum(),
        total_replayed_operations_skipped: iterations
            .iter()
            .map(|iteration| iteration.replayed_operations_skipped)
            .sum(),
        total_session_started: iterations
            .iter()
            .map(|iteration| iteration.session_started)
            .sum(),
        total_sync_started: iterations
            .iter()
            .map(|iteration| iteration.sync_started)
            .sum(),
        total_sync_finished: iterations
            .iter()
            .map(|iteration| iteration.sync_finished)
            .sum(),
        total_live_mode_started: iterations
            .iter()
            .map(|iteration| iteration.live_mode_started)
            .sum(),
        total_session_finished: iterations
            .iter()
            .map(|iteration| iteration.session_finished)
            .sum(),
        startup_p95_ms: percentile_95(&mut startup_ms),
        import_p95_ms: percentile_95(&mut import_ms),
        elapsed_ms: started.elapsed().as_millis(),
        iteration_reports: iterations,
    };
    let json = write_json(
        &root.join("p2panda-net-fact-node-reliability-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS p2panda-net-fact-node-reliability-contract");
    Ok(())
}

async fn run_iteration(iteration: usize) -> Result<P2pandaNetFactNodeReliabilityIteration, String> {
    let island = IslandId::new(format!("prod-fact-node-reliability-{iteration}"));
    let (bus, sessions) = reliability_bus_sessions(&island, iteration)?;
    let bus = Arc::new(bus);
    let writer = PandaFactAuthor::new(sessions.writer.principal().clone());
    let network_id = PandaNetNetworkId::new(stable_test_id(iteration, b'n'));
    let topic = PandaNetTopic::new(stable_test_id(iteration, b't'));

    let startup_started = Instant::now();
    let mut receiver = fact_node(
        &bus,
        &island,
        &sessions.receiver_replica,
        &[(&sessions.writer, &writer)],
        network_id,
        topic,
        PandaNetNodeSeed::new(stable_test_id(iteration, b'r')),
        Vec::new(),
    )
    .await?;
    let receiver_info = receiver.node_info();
    let mut sender = fact_node(
        &bus,
        &island,
        &sessions.receiver_replica,
        &[(&sessions.writer, &writer)],
        network_id,
        topic,
        PandaNetNodeSeed::new(stable_test_id(iteration, b's')),
        vec![receiver_info],
    )
    .await?;
    receiver
        .add_node_info(sender.node_info())
        .await
        .map_err(|error| {
            format!("iteration {iteration} add sender to receiver address book: {error}")
        })?;
    let startup_ms = startup_started.elapsed().as_millis();

    publish_iteration_fact(iteration, &mut sender, &sessions, &writer).await?;

    let import_started = Instant::now();
    let import_report = import_until_iteration_terminal(iteration, &mut receiver).await?;
    let import_ms = import_started.elapsed().as_millis();
    let stats = receiver.stats();
    if import_report.attempted == 0 {
        return Err(format!(
            "iteration {iteration} attempted zero imports after publish; report={import_report:?}; stats={stats:?}"
        ));
    }

    Ok(P2pandaNetFactNodeReliabilityIteration {
        iteration,
        attempted_imports: import_report.attempted,
        imported_operations: import_report.imported,
        idle_timeouts: stats.idle_timeouts,
        stream_refreshes: stats.stream_refreshes,
        stream_refresh_failures: stats.stream_refresh_failures,
        stream_ended: stats.stream_ended,
        stream_lagged: stats.stream_lagged,
        stream_failed: stats.stream_failed,
        replayed_operations_skipped: stats.replayed_operations_skipped,
        session_started: stats.session_started,
        sync_started: stats.sync_started,
        sync_finished: stats.sync_finished,
        live_mode_started: stats.live_mode_started,
        session_finished: stats.session_finished,
        startup_ms,
        import_ms,
    })
}

async fn publish_iteration_fact(
    iteration: usize,
    sender: &mut PandaNetFactNode,
    sessions: &ReliabilityBusSessions,
    writer: &PandaFactAuthor,
) -> Result<(), String> {
    sender
        .publish_fact_payload(
            &sessions.writer,
            writer,
            fact_key(&format!("/facts/node/reliability-{iteration}/joined/1"))?,
            payload(format!("node-{iteration}-a")),
        )
        .await
        .map_err(|error| format!("iteration {iteration} publish node fact: {error}"))?;
    Ok(())
}

async fn import_until_iteration_terminal(
    iteration: usize,
    receiver: &mut PandaNetFactNode,
) -> Result<PandaNetFactImportReport, String> {
    let deadline = Instant::now() + IMPORT_DEADLINE;
    let mut report = PandaNetFactImportReport::new(0);
    while Instant::now() < deadline {
        let batch = match receiver
            .import_next_fact_batch_with_idle_timeout(IMPORT_IDLE)
            .await
        {
            Ok(batch) => batch,
            Err(PandaNetTransportError::StreamEnded { .. }) => {
                receiver.refresh_stream().await.map_err(|error| {
                    format!("iteration {iteration} refresh ended p2panda-net stream: {error}")
                })?;
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "iteration {iteration} import p2panda-net fact stream: {error}; stats={:?}",
                    receiver.stats()
                ));
            }
        };
        let Some(batch) = batch else {
            receiver.refresh_stream().await.map_err(|error| {
                format!("iteration {iteration} refresh idle p2panda-net stream: {error}")
            })?;
            continue;
        };
        report.attempted += 1;
        for outcome in batch {
            report.record(outcome);
        }
        if iteration_terminal_outcomes_arrived(&report) {
            return Ok(report);
        }
    }
    Err(format!(
        "iteration {iteration} terminal outcomes did not arrive: report={report:?}; stats={:?}",
        receiver.stats()
    ))
}

fn iteration_terminal_outcomes_arrived(report: &PandaNetFactImportReport) -> bool {
    report.imported >= 1
        && report.conflict == 0
        && report.rejected.is_empty()
        && report.deferred.is_empty()
        && report.failed.is_empty()
}

struct ReliabilityBusSessions {
    writer: BusSession,
    receiver_replica: BusSession,
}

fn reliability_bus_sessions(
    island: &IslandId,
    iteration: usize,
) -> Result<(InMemoryBus, ReliabilityBusSessions), String> {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer_grant = Grant::empty()
        .with_fact_write(fact_pattern("/facts/>")?)
        .with_fact_read(fact_pattern("/facts/>")?);
    let sessions = ReliabilityBusSessions {
        writer: authority.grant_in(
            island.clone(),
            PrincipalId::new(format!("fact-node-reliability-writer-{iteration}")),
            writer_grant,
        ),
        receiver_replica: authority.grant_in(
            island.clone(),
            PrincipalId::new(format!("fact-node-reliability-replica-{iteration}")),
            Grant::empty(),
        ),
    };
    Ok((bus, sessions))
}

async fn fact_node(
    bus: &Arc<InMemoryBus>,
    island: &IslandId,
    replica_session: &BusSession,
    authors: &[(&BusSession, &PandaFactAuthor)],
    network_id: PandaNetNetworkId,
    topic: PandaNetTopic,
    seed: PandaNetNodeSeed,
    bootstrap: Vec<PandaNetNodeInfo>,
) -> Result<PandaNetFactNode, String> {
    let store = SharedPandaFactStore::new(PandaFactStore::new(bus.clone()));
    for &(session, author) in authors {
        store
            .trust_author_key(island, session.principal().clone(), author.author_key())
            .await
            .map_err(|error| format!("trust reliability fact-node author: {error}"))?;
    }
    store
        .trust_replica_peer(island, replica_session.principal().clone())
        .await;
    PandaNetFactNode::spawn(PandaNetFactNodeConfig::new(
        PandaNetNodeConfig::localhost_ephemeral(network_id, seed, bootstrap),
        topic,
        store,
        replica_session.clone(),
    ))
    .await
    .map_err(|error| format!("spawn reliability p2panda-net fact node: {error}"))
}

fn payload(value: impl Into<String>) -> FactPayload {
    FactPayload::from(value.into())
}

fn percentile_95(values: &mut [u128]) -> u128 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let index = ((values.len() - 1) * 95) / 100;
    values[index]
}

fn stable_test_id(iteration: usize, salt: u8) -> [u8; 32] {
    let mut bytes = [salt; 32];
    for (index, byte) in iteration.to_le_bytes().iter().enumerate() {
        bytes[index] = bytes[index]
            .wrapping_mul(31)
            .wrapping_add(*byte)
            .wrapping_add(index as u8);
    }
    bytes
}

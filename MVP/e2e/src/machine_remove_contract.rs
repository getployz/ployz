use std::collections::BTreeSet;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mvp_bus::{BusError, BusSession, FactKey, Grant, HandlerFailure, IslandId, PrincipalId};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_machine::{
    MachineFactWriter, MachineRemoveCleanupDoneFact, MachineRemoveCoordinator, MachineRemoveId,
    MachineRemoveOutcome, MachineRemoveRecovery, MachineRemoveRequest, MachineRemoveResult,
    MachineRemoveTimeouts, PrepareRemoveIntent, PrepareRemoveOutcome, PrepareRemoveReply,
    PrepareRemoveRequest, RemoveCleanupStatus, StopRemovedWorkloadsOutcome,
    StopRemovedWorkloadsReply, StopRemovedWorkloadsRequest, WrittenMachineFact,
    prepare_remove_subject, recover_pending_machine_remove_cleanup, stop_removed_workloads_subject,
};
use mvp_machine_p2panda::{PandaMachineFactStore, PandaMachineFactWriter};
use mvp_mesh::{
    InviteId, InviteSecret, IrohEndpointId, JoinCommand, JoinRequest, MachineInvite,
    WireGuardAppliedSnapshot, WireGuardPublicKey, WireGuardSnapshotPaths, plan_full_mesh,
    write_applied_snapshot,
};
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore};
use mvp_projection::{
    BackendEndpoint, DnsRecordFact, FactSource, NodeRemovalStartedFact, NodeTombstonedFact,
    ProjectionIgnoreReason, ProjectionState, RouteId,
};
use mvp_routing::{
    DnsCommitId, GatewayCommitId, ProjectionCatchUp, RouteCommitId, ServingCommitId,
    ServingCommitPlan, ServingFactWriter,
};
use mvp_routing_p2panda::PandaServingFactWriter;
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::{fact_pattern, pattern};
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::p2panda_projection_fixture::status_count;
use crate::process_role_harness::{
    MeshRoleClientError, MeshRoleFailureKind as HarnessMeshFailureKind, MeshRoleRequest,
    MeshRoleSuccess, cleanup_orphaned_children as cleanup_process_children, request_mesh_role,
    spawn_process_role, wait_for_mesh_data_plane,
};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
struct MachineRemoveReport {
    scenario: &'static str,
    visible_nodes_at_decision: usize,
    remove_duration_ms: u128,
    route_commit_to_projection_ms: u128,
    coordinator_outage_ms: u128,
    recovery_read_ms: u128,
    projection_rebuild_ms: u128,
    tombstone_convergence_ms: u128,
    wireguard_peer_plan_ms: u128,
    remaining_traffic_success_count: usize,
    target_removed_from_active_backends: bool,
    target_kept_in_old_backends_until_cleanup: bool,
    prepare_no_new_work_and_drained: bool,
    stop_after_projection_catch_up: bool,
    tombstone_after_stop: bool,
    cleanup_done_recovered: bool,
    no_precommit_replay_after_recovery: bool,
    removed_peer_rejected: bool,
    removal_started_before_route_cutover: bool,
    fresh_rebuild_conflict_count: usize,
    elapsed_ms: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoveEvent {
    Probe,
    Decision,
    RemovalStarted,
    PrepareNoNewWorkAndDrained,
    RouteCutoverProjected,
    Stop,
    Tombstone,
    CleanupDone,
}

#[derive(Default)]
struct ParticipantState {
    events: Mutex<Vec<RemoveEvent>>,
    stop_attempts: AtomicUsize,
    projection_caught_up: AtomicBool,
}

impl ParticipantState {
    fn record(&self, event: RemoveEvent) -> Result<(), BusError> {
        self.events
            .lock()
            .map_err(|_| handler_failed("machine remove event log"))?
            .push(event);
        Ok(())
    }

    fn events(&self) -> Result<Vec<RemoveEvent>, String> {
        self.events
            .lock()
            .map_err(|_| "machine remove event log mutex poisoned".to_string())
            .map(|events| events.clone())
    }

    fn mark_projection_caught_up(&self) -> Result<(), String> {
        self.projection_caught_up.store(true, Ordering::SeqCst);
        self.record(RemoveEvent::RouteCutoverProjected)
            .map_err(|error| format!("record projection catch-up: {error}"))
    }
}

#[derive(Clone)]
struct RecordingMachineFactWriter<W> {
    inner: W,
    events: Arc<ParticipantState>,
}

impl<W> RecordingMachineFactWriter<W> {
    fn new(inner: W, events: Arc<ParticipantState>) -> Self {
        Self { inner, events }
    }
}

impl<W> RecordingMachineFactWriter<W>
where
    W: MachineFactWriter,
{
    fn record(&self, event: RemoveEvent) -> MachineRemoveResult<()> {
        self.events.record(event).map_err(|_| {
            mvp_machine::MachineRemoveError::Bus(BusError::HandlerFailed {
                subject: "machine remove event log".to_string(),
                failure: HandlerFailure::Application,
            })
        })
    }
}

impl<W> MachineFactWriter for RecordingMachineFactWriter<W>
where
    W: MachineFactWriter,
{
    fn write_remove_decision<'a>(
        &'a self,
        fact: mvp_machine::MachineRemoveDecisionFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let written = self.inner.write_remove_decision(fact).await?;
            self.record(RemoveEvent::Decision)?;
            Ok(written)
        })
    }

    fn write_removal_started<'a>(
        &'a self,
        fact: NodeRemovalStartedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let written = self.inner.write_removal_started(fact).await?;
            self.record(RemoveEvent::RemovalStarted)?;
            Ok(written)
        })
    }

    fn write_tombstone<'a>(
        &'a self,
        fact: NodeTombstonedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let written = self.inner.write_tombstone(fact).await?;
            self.record(RemoveEvent::Tombstone)?;
            Ok(written)
        })
    }

    fn write_cleanup_done<'a>(
        &'a self,
        fact: MachineRemoveCleanupDoneFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let written = self.inner.write_cleanup_done(fact).await?;
            self.record(RemoveEvent::CleanupDone)?;
            Ok(written)
        })
    }
}

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    cleanup_process_children(&scenario_dir("machine-remove-contract"))
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create machine-remove runtime: {error}"))?;
    let result = runtime.block_on(run_async());
    if result.is_err() {
        let _ = cleanup_orphaned_children();
    }
    result
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("machine-remove-contract");
    reset_dir(&root)?;

    let island = IslandId::new("prod");
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        island.clone(),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let participant = authority.grant_in(
        island.clone(),
        PrincipalId::new("target-node"),
        Grant::allow_all(),
    );
    let join_writer_session = authority.grant_in(
        island.clone(),
        PrincipalId::new("join-writer"),
        Grant::empty().with_fact_write(fact_pattern("/facts/node/*/joined/>")?),
    );
    let projection_session = authority.grant_in(
        island.clone(),
        PrincipalId::new("projection"),
        Grant::empty().with_fact_read(fact_pattern("/facts/>")?),
    );
    let replica_session = authority.grant_in(
        island.clone(),
        PrincipalId::new("machine-remove-replica"),
        Grant::empty(),
    );
    let machine_writer_session = authority.grant_in(
        island.clone(),
        PrincipalId::new("machine-remove-writer"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/machine-remove/>")?)
            .with_fact_write(fact_pattern("/facts/node/*/removal_started/>")?)
            .with_fact_write(fact_pattern("/facts/node/*/tombstoned/>")?),
    );
    let routing_writer_session = authority.grant_in(
        island.clone(),
        PrincipalId::new("routing-writer"),
        Grant::empty().with_fact_write(fact_pattern("/facts/serving/>")?),
    );
    let join_author = Arc::new(PandaFactAuthor::new(
        join_writer_session.principal().clone(),
    ));
    let machine_author = Arc::new(PandaFactAuthor::new(
        machine_writer_session.principal().clone(),
    ));
    let routing_author = Arc::new(PandaFactAuthor::new(
        routing_writer_session.principal().clone(),
    ));
    let facts = PandaMachineFactStore::new(PandaFactStore::new(Arc::new(raw_bus.clone())));

    let node_ids = [
        NodeId::new("node-target"),
        NodeId::new("node-source"),
        NodeId::new("node-destination"),
        NodeId::new("node-observer"),
    ];
    for (index, node_id) in node_ids.iter().enumerate() {
        let joined = JoinCommand::admit(join_request(
            invite(island.clone()),
            node_id.clone(),
            1,
            index as u64,
        ))
        .map_err(|error| format!("admit {}: {error}", node_id.as_str()))?;
        facts
            .write_fact_payload(
                &join_writer_session,
                join_author.as_ref(),
                joined.fact_key,
                joined.fact_payload,
            )
            .await
            .map_err(|error| format!("write joined fact for {}: {error}", node_id.as_str()))?;
    }

    let initial_commit = initial_serving_commit();
    let serving_writer = PandaServingFactWriter::new(
        facts.shared(),
        routing_writer_session.clone(),
        Arc::clone(&routing_author),
    );
    serving_writer
        .write_serving_commit(&initial_commit)
        .await
        .map_err(|error| format!("write initial serving commit: {error}"))?;
    let fact_source: Arc<dyn FactSource> = Arc::new(facts.clone());
    let projection = projection_actor(fact_source.clone(), projection_session.clone(), &root)?;
    let initial_projection = projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project initial machine remove state: {error}"))?;
    assert_initial_route_includes_target(&initial_projection.state)?;

    let events = Arc::new(ParticipantState::default());
    register_prepare_handler(&bus, &participant, Arc::clone(&events)).await?;
    register_stop_handler(&bus, &participant, Arc::clone(&events)).await?;

    let remove_commit = remove_serving_commit();
    let machine_writer = PandaMachineFactWriter::new(
        facts.clone(),
        machine_writer_session.clone(),
        Arc::clone(&machine_author),
    );
    let writer = RecordingMachineFactWriter::new(machine_writer, Arc::clone(&events));
    let serving_writer = PandaServingFactWriter::new(
        facts.shared(),
        routing_writer_session.clone(),
        Arc::clone(&routing_author),
    );
    let coordinator = MachineRemoveCoordinator::with_fact_writers(
        bus.clone(),
        operator.clone(),
        writer,
        serving_writer,
        MachineRemoveTimeouts {
            participant: Duration::from_secs(2),
        },
    );
    let request = MachineRemoveRequest {
        target_node_id: NodeId::new("node-target"),
        removal_epoch: 2,
        tombstone_epoch: 3,
        reason: "graceful-remove".to_string(),
        visible_nodes: VisibleNodes::new(node_ids.iter().cloned()),
        current_projection: initial_projection.state.clone(),
        serving_commit: remove_commit.clone(),
    };

    let remove_started = Instant::now();
    let pending = coordinator
        .execute_until_serving_commit(request)
        .await
        .map_err(|error| format!("execute graceful remove until serving commit: {error}"))?;
    drop(pending);
    drop(coordinator);
    let remove_duration_ms = remove_started.elapsed().as_millis();
    assert_eq_named(
        "stop attempts before projection",
        events.stop_attempts.load(Ordering::SeqCst),
        0,
    )?;

    let route_projection_started = Instant::now();
    let projected_cutover = projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project route cutover: {error}"))?;
    let route_commit_to_projection_ms = route_projection_started.elapsed().as_millis();
    events.mark_projection_caught_up()?;
    assert_cutover_projection(&projected_cutover.state)?;
    let catch_up = ProjectionCatchUp::from_report(&remove_commit, &projected_cutover)
        .map_err(|error| format!("build remove projection catch-up: {error}"))?;

    let outage_started = Instant::now();
    let exported = facts.export_operations().await;
    let rebuilt_facts = PandaMachineFactStore::new(PandaFactStore::new(Arc::new(raw_bus.clone())));
    rebuilt_facts
        .trust_replica_peer(&island, replica_session.principal().clone())
        .await;
    trust_fresh_store_authors(
        &rebuilt_facts,
        &island,
        &[
            (&join_writer_session, join_author.as_ref()),
            (&machine_writer_session, machine_author.as_ref()),
            (&routing_writer_session, routing_author.as_ref()),
        ],
    )
    .await?;
    for operation in &exported {
        rebuilt_facts
            .import_replica_operation(&replica_session, operation)
            .await
            .map_err(|error| format!("import p2panda operation for recovery: {error}"))?;
    }
    let coordinator_outage_ms = outage_started.elapsed().as_millis();
    let recovered_machine_writer = PandaMachineFactWriter::new(
        rebuilt_facts.clone(),
        machine_writer_session.clone(),
        Arc::clone(&machine_author),
    );
    let recovered_writer =
        RecordingMachineFactWriter::new(recovered_machine_writer, Arc::clone(&events));
    let recovered_serving_writer = PandaServingFactWriter::new(
        rebuilt_facts.shared(),
        routing_writer_session.clone(),
        Arc::clone(&routing_author),
    );
    let recovery_coordinator = MachineRemoveCoordinator::with_fact_writers(
        bus.clone(),
        operator.clone(),
        recovered_writer,
        recovered_serving_writer,
        MachineRemoveTimeouts {
            participant: Duration::from_secs(2),
        },
    );
    let recovery_read_started = Instant::now();
    let recovered_pending = match recover_pending_machine_remove_cleanup(
        &rebuilt_facts,
        &island,
        &projection_session,
        &MachineRemoveId::new(NodeId::new("node-target"), 2),
    )
    .map_err(|error| format!("recover machine remove cleanup: {error}"))?
    {
        MachineRemoveRecovery::Pending(pending) => pending,
        other => {
            return Err(format!(
                "expected pending machine remove recovery, got {other:?}"
            ));
        }
    };
    let recovery_read_ms = recovery_read_started.elapsed().as_millis();
    let events_after_recovery = events.events()?;
    let no_precommit_replay_after_recovery =
        event_count(&events_after_recovery, RemoveEvent::Probe) == 1
            && event_count(
                &events_after_recovery,
                RemoveEvent::PrepareNoNewWorkAndDrained,
            ) == 1
            && event_count(&events_after_recovery, RemoveEvent::RemovalStarted) == 1
            && event_count(&events_after_recovery, RemoveEvent::Decision) == 1;
    assert!(no_precommit_replay_after_recovery);
    assert_eq_named(
        "stop attempts before recovered cleanup",
        events.stop_attempts.load(Ordering::SeqCst),
        0,
    )?;

    let cleanup_result = recovery_coordinator
        .finish_cleanup(recovered_pending, catch_up)
        .await
        .map_err(|error| format!("finish recovered graceful remove cleanup: {error}"))?;
    assert_eq_named(
        "machine remove outcome",
        cleanup_result.outcome.clone(),
        MachineRemoveOutcome::Removed,
    )?;
    assert_eq_named("visible nodes", cleanup_result.visible_nodes.len(), 4)?;
    assert_visible_nodes(
        &cleanup_result.visible_nodes,
        &[
            NodeId::new("node-destination"),
            NodeId::new("node-observer"),
            NodeId::new("node-source"),
            NodeId::new("node-target"),
        ],
    )?;
    let removal_started_key = cleanup_result
        .removal_started_fact_key
        .clone()
        .ok_or_else(|| {
            "machine remove cleanup did not return removal-started fact key".to_string()
        })?;
    assert_eq_named(
        "removal-started fact key",
        removal_started_key.as_str(),
        "/facts/node/node-target/removal_started/2",
    )?;
    assert_eq_named(
        "cleanup status",
        cleanup_result.cleanup_status.clone(),
        RemoveCleanupStatus::Done,
    )?;

    let tombstone_started = Instant::now();
    let tombstone_key = cleanup_result
        .tombstone_fact_key
        .clone()
        .ok_or_else(|| "machine remove cleanup did not return tombstone fact key".to_string())?;
    assert_eq_named(
        "tombstone fact key",
        tombstone_key.as_str(),
        "/facts/node/node-target/tombstoned/3",
    )?;
    assert_fact_key_present(&rebuilt_facts, &projection_session, &tombstone_key)?;
    let tombstone_convergence_ms = tombstone_started.elapsed().as_millis();

    let stop_attempts_after_cleanup = events.stop_attempts.load(Ordering::SeqCst);
    let completed_operations = rebuilt_facts.export_operations().await;
    let completed_replay =
        PandaMachineFactStore::new(PandaFactStore::new(Arc::new(raw_bus.clone())));
    completed_replay
        .trust_replica_peer(&island, replica_session.principal().clone())
        .await;
    trust_fresh_store_authors(
        &completed_replay,
        &island,
        &[
            (&join_writer_session, join_author.as_ref()),
            (&machine_writer_session, machine_author.as_ref()),
            (&routing_writer_session, routing_author.as_ref()),
        ],
    )
    .await?;
    for operation in &completed_operations {
        completed_replay
            .import_replica_operation(&replica_session, operation)
            .await
            .map_err(|error| format!("import completed machine remove operation: {error}"))?;
    }
    let recovered_complete = recover_pending_machine_remove_cleanup(
        &completed_replay,
        &island,
        &projection_session,
        &MachineRemoveId::new(NodeId::new("node-target"), 2),
    )
    .map_err(|error| format!("recover completed machine remove cleanup: {error}"))?;
    let cleanup_done_recovered = matches!(recovered_complete, MachineRemoveRecovery::Complete(_));
    assert!(cleanup_done_recovered);
    assert_eq_named(
        "second recovery does not stop again",
        events.stop_attempts.load(Ordering::SeqCst),
        stop_attempts_after_cleanup,
    )?;

    let rebuild_root = root.join("rebuild");
    reset_dir(&rebuild_root)?;
    let rebuild_started = Instant::now();
    let rebuild_projection = projection_actor(
        Arc::new(rebuilt_facts.clone()),
        projection_session.clone(),
        &rebuild_root,
    )?;
    let rebuilt = rebuild_projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("rebuild machine remove projection: {error}"))?;
    let projection_rebuild_ms = rebuild_started.elapsed().as_millis();
    assert_eq_named("post-remove live nodes", rebuilt.state.nodes.len(), 3)?;
    assert_eq_named(
        "projected tombstone epoch",
        rebuilt
            .state
            .tombstoned_nodes
            .get(&NodeId::new("node-target"))
            .copied(),
        Some(3),
    )?;
    assert!(
        !rebuilt
            .state
            .removing_nodes
            .contains_key(&NodeId::new("node-target"))
    );
    let fresh_rebuild_conflict_count =
        status_count(&rebuilt.state.statuses, ProjectionIgnoreReason::Conflict);
    assert_eq_named(
        "fresh rebuild conflict status count",
        fresh_rebuild_conflict_count,
        0,
    )?;
    assert_join_writer_cannot_tombstone(&facts, &join_writer_session, Arc::clone(&join_author))
        .await?;
    assert_machine_writer_cannot_join(&facts, &machine_writer_session, machine_author.as_ref())
        .await?;
    assert_node_writer_cannot_write_command_fact(&island).await?;
    assert_conflicting_tombstone_not_projected(&root, &island).await?;

    let mesh_started = Instant::now();
    let (remaining_traffic_success_count, removed_peer_rejected) =
        prove_remaining_mesh_traffic(&root, &island, &rebuilt.state).await?;
    let wireguard_peer_plan_ms = mesh_started.elapsed().as_millis();

    let events_seen = events.events()?;
    let removal_started_before_route_cutover =
        event_index(&events_seen, RemoveEvent::RemovalStarted)?
            < event_index(&events_seen, RemoveEvent::RouteCutoverProjected)?;
    let stop_after_projection_catch_up =
        event_index(&events_seen, RemoveEvent::RouteCutoverProjected)?
            < event_index(&events_seen, RemoveEvent::Stop)?;
    let tombstone_after_stop = event_index(&events_seen, RemoveEvent::Stop)?
        < event_index(&events_seen, RemoveEvent::Tombstone)?;
    let cleanup_done_after_tombstone = event_index(&events_seen, RemoveEvent::Tombstone)?
        < event_index(&events_seen, RemoveEvent::CleanupDone)?;
    let prepare_no_new_work_and_drained =
        events_seen.contains(&RemoveEvent::PrepareNoNewWorkAndDrained);
    let target_removed_from_active_backends = target_removed_from_active(&projected_cutover.state)?;
    let target_kept_in_old_backends_until_cleanup =
        target_kept_in_old_backends(&projected_cutover.state)?;

    cleanup_process_children(&root)?;

    let report = MachineRemoveReport {
        scenario: "machine-remove-contract",
        visible_nodes_at_decision: cleanup_result.visible_nodes.len(),
        remove_duration_ms,
        route_commit_to_projection_ms,
        coordinator_outage_ms,
        recovery_read_ms,
        projection_rebuild_ms,
        tombstone_convergence_ms,
        wireguard_peer_plan_ms,
        remaining_traffic_success_count,
        target_removed_from_active_backends,
        target_kept_in_old_backends_until_cleanup,
        prepare_no_new_work_and_drained,
        stop_after_projection_catch_up,
        tombstone_after_stop: tombstone_after_stop && cleanup_done_after_tombstone,
        cleanup_done_recovered,
        no_precommit_replay_after_recovery,
        removed_peer_rejected,
        removal_started_before_route_cutover,
        fresh_rebuild_conflict_count,
        elapsed_ms: started.elapsed().as_millis(),
    };

    assert_eq_named(
        "remaining traffic successes",
        report.remaining_traffic_success_count,
        1,
    )?;
    assert!(report.target_removed_from_active_backends);
    assert!(report.target_kept_in_old_backends_until_cleanup);
    assert!(report.prepare_no_new_work_and_drained);
    assert!(report.stop_after_projection_catch_up);
    assert!(report.tombstone_after_stop);
    assert!(report.cleanup_done_recovered);
    assert!(report.no_precommit_replay_after_recovery);
    assert!(report.removed_peer_rejected);
    assert!(report.removal_started_before_route_cutover);
    assert_eq!(report.fresh_rebuild_conflict_count, 0);

    let json = write_json(&root.join("machine-remove-contract-metrics.json"), &report)?;
    println!("{json}");
    eprintln!("PASS machine-remove-contract");
    Ok(())
}

async fn register_prepare_handler(
    bus: &mvp_bus::BusActorHandle,
    session: &BusSession,
    events: Arc<ParticipantState>,
) -> Result<(), String> {
    let subject = prepare_remove_subject(&NodeId::new("node-target"))
        .map_err(|error| format!("build prepare_remove subject: {error}"))?;
    bus.subscribe(session, pattern(subject.as_str())?, move |ctx| {
        let request: PrepareRemoveRequest =
            serde_json::from_slice(ctx.message.payload().as_bytes())
                .map_err(|_| handler_failed(ctx.message.subject()))?;
        let outcome = match request.intent {
            PrepareRemoveIntent::Probe => {
                events.record(RemoveEvent::Probe)?;
                PrepareRemoveOutcome::ResponderReady
            }
            PrepareRemoveIntent::Drain => {
                events.record(RemoveEvent::PrepareNoNewWorkAndDrained)?;
                PrepareRemoveOutcome::NoNewWorkAndDrained
            }
        };
        ctx.reply(
            serde_json::to_vec(&PrepareRemoveReply {
                target_node_id: request.target_node_id,
                outcome,
            })
            .map_err(|_| handler_failed(ctx.message.subject()))?,
        )
    })
    .await
    .map(|_| ())
    .map_err(|error| format!("register prepare_remove handler: {error}"))
}

async fn register_stop_handler(
    bus: &mvp_bus::BusActorHandle,
    session: &BusSession,
    events: Arc<ParticipantState>,
) -> Result<(), String> {
    let subject = stop_removed_workloads_subject(&NodeId::new("node-target"))
        .map_err(|error| format!("build stop_removed_workloads subject: {error}"))?;
    bus.subscribe(session, pattern(subject.as_str())?, move |ctx| {
        events.stop_attempts.fetch_add(1, Ordering::SeqCst);
        if !events.projection_caught_up.load(Ordering::SeqCst) {
            return Err(handler_failed(ctx.message.subject()));
        }
        let request: StopRemovedWorkloadsRequest =
            serde_json::from_slice(ctx.message.payload().as_bytes())
                .map_err(|_| handler_failed(ctx.message.subject()))?;
        events.record(RemoveEvent::Stop)?;
        ctx.reply(
            serde_json::to_vec(&StopRemovedWorkloadsReply {
                target_node_id: request.target_node_id,
                outcome: StopRemovedWorkloadsOutcome::Stopped,
            })
            .map_err(|_| handler_failed(ctx.message.subject()))?,
        )
    })
    .await
    .map(|_| ())
    .map_err(|error| format!("register stop_removed_workloads handler: {error}"))
}

async fn trust_fresh_store_authors(
    facts: &PandaMachineFactStore,
    island: &IslandId,
    authors: &[(&BusSession, &PandaFactAuthor)],
) -> Result<(), String> {
    for (session, author) in authors {
        facts
            .trust_author_key(island, session.principal().clone(), author.author_key())
            .await
            .map_err(|error| {
                format!(
                    "trust p2panda author key for {}: {error}",
                    session.principal().as_str()
                )
            })?;
    }
    Ok(())
}

fn assert_fact_key_present(
    source: &impl FactSource,
    session: &BusSession,
    key: &FactKey,
) -> Result<(), String> {
    let pattern = fact_pattern(key.as_str())?;
    let candidates = source
        .list_candidates(session.island(), &pattern, session)
        .map_err(|error| format!("list candidates for {key}: {error}"))?;
    if candidates.iter().any(|candidate| candidate.key() == key) {
        return Ok(());
    }
    Err(format!("fact key {key} was not present"))
}

async fn assert_join_writer_cannot_tombstone(
    facts: &PandaMachineFactStore,
    join_writer_session: &BusSession,
    join_author: Arc<PandaFactAuthor>,
) -> Result<(), String> {
    let writer =
        PandaMachineFactWriter::new(facts.clone(), join_writer_session.clone(), join_author);
    let result = writer
        .write_tombstone(NodeTombstonedFact {
            node_id: NodeId::new("node-denied"),
            epoch: 1,
            reason: "forbidden".to_string(),
        })
        .await;
    match result {
        Err(mvp_machine::MachineRemoveError::UnauthorizedFactWrite { .. }) => Ok(()),
        other => Err(format!(
            "expected join-writer tombstone denial, got {other:?}"
        )),
    }
}

async fn assert_machine_writer_cannot_join(
    facts: &PandaMachineFactStore,
    machine_writer_session: &BusSession,
    machine_author: &PandaFactAuthor,
) -> Result<(), String> {
    let joined = JoinCommand::admit(join_request(
        invite(machine_writer_session.island().clone()),
        NodeId::new("node-denied"),
        1,
        0,
    ))
    .map_err(|error| format!("build denied joined fact: {error}"))?;
    let result = facts
        .write_fact_payload(
            machine_writer_session,
            machine_author,
            joined.fact_key,
            joined.fact_payload,
        )
        .await;
    match result {
        Err(mvp_p2panda_facts::PandaFactError::UnauthorizedWrite { .. }) => Ok(()),
        other => Err(format!(
            "expected machine-writer join denial, got {other:?}"
        )),
    }
}

async fn assert_node_writer_cannot_write_command_fact(island: &IslandId) -> Result<(), String> {
    let (raw_bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
    let node_only = authority.grant_in(
        island.clone(),
        PrincipalId::new("node-fact-only"),
        Grant::empty().with_fact_write(fact_pattern("/facts/node/*/tombstoned/>")?),
    );
    let facts = PandaMachineFactStore::new(PandaFactStore::new(Arc::new(raw_bus)));
    let author = Arc::new(PandaFactAuthor::new(node_only.principal().clone()));
    let writer = PandaMachineFactWriter::new(facts, node_only, author);
    let decision = mvp_machine::MachineRemoveDecisionFact::new(
        NodeId::new("node-target"),
        2,
        3,
        "forbidden".to_string(),
        VisibleNodes::new([NodeId::new("node-target")]),
        remove_serving_commit(),
    );
    let result = writer.write_remove_decision(decision).await;
    match result {
        Err(mvp_machine::MachineRemoveError::UnauthorizedFactWrite { .. }) => Ok(()),
        other => Err(format!(
            "expected node-only writer command-fact denial, got {other:?}"
        )),
    }
}

async fn assert_conflicting_tombstone_not_projected(
    root: &std::path::Path,
    island: &IslandId,
) -> Result<(), String> {
    let conflict_root = root.join("conflicting-tombstone");
    reset_dir(&conflict_root)?;
    let (raw_bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
    let grant = Grant::empty().with_fact_write(fact_pattern("/facts/node/*/tombstoned/>")?);
    let writer_a = authority.grant_in(
        island.clone(),
        PrincipalId::new("machine-conflict-a"),
        grant.clone(),
    );
    let writer_b = authority.grant_in(
        island.clone(),
        PrincipalId::new("machine-conflict-b"),
        grant,
    );
    let reader = authority.grant_in(
        island.clone(),
        PrincipalId::new("machine-conflict-reader"),
        Grant::empty().with_fact_read(fact_pattern("/facts/>")?),
    );
    let author_a = Arc::new(PandaFactAuthor::new(writer_a.principal().clone()));
    let author_b = Arc::new(PandaFactAuthor::new(writer_b.principal().clone()));
    let facts = PandaMachineFactStore::new(PandaFactStore::new(Arc::new(raw_bus)));
    let panda_writer_a =
        PandaMachineFactWriter::new(facts.clone(), writer_a, Arc::clone(&author_a));
    let panda_writer_b =
        PandaMachineFactWriter::new(facts.clone(), writer_b, Arc::clone(&author_b));
    panda_writer_a
        .write_tombstone(NodeTombstonedFact {
            node_id: NodeId::new("node-conflict"),
            epoch: 1,
            reason: "first".to_string(),
        })
        .await
        .map_err(|error| format!("write first conflicting tombstone: {error}"))?;
    let conflict = panda_writer_b
        .write_tombstone(NodeTombstonedFact {
            node_id: NodeId::new("node-conflict"),
            epoch: 1,
            reason: "second".to_string(),
        })
        .await;
    if !matches!(
        conflict,
        Err(mvp_machine::MachineRemoveError::FactConflict { .. })
    ) {
        return Err(format!(
            "expected conflicting tombstone write failure, got {conflict:?}"
        ));
    }
    let projection = projection_actor(Arc::new(facts), reader, &conflict_root)?;
    let projected = projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project conflicting tombstone: {error}"))?;
    if status_count(&projected.state.statuses, ProjectionIgnoreReason::Conflict) == 0 {
        return Err("conflicting tombstone did not surface projection conflict status".to_string());
    }
    if projected
        .state
        .tombstoned_nodes
        .contains_key(&NodeId::new("node-conflict"))
    {
        return Err("conflicting tombstone was projected as current truth".to_string());
    }
    Ok(())
}

async fn prove_remaining_mesh_traffic(
    root: &std::path::Path,
    island: &IslandId,
    state: &ProjectionState,
) -> Result<(usize, bool), String> {
    let destination_plan = plan_full_mesh(state, &NodeId::new("node-destination"), 4)
        .map_err(|error| format!("plan destination mesh: {error}"))?;
    let destination_snapshot = root.join("node-destination.wireguard.snapshot");
    write_applied_snapshot(
        &WireGuardSnapshotPaths {
            applied: destination_snapshot.clone(),
        },
        &WireGuardAppliedSnapshot::from_plan(island.clone(), destination_plan),
    )
    .map_err(|error| format!("write destination mesh snapshot: {error}"))?;

    let destination_socket = root.join("node-destination-mesh.sock");
    let mut destination_role = spawn_mesh_role(
        root,
        "machine-remove-destination",
        &destination_socket,
        &destination_snapshot,
        loopback_any_port()?,
        island,
    )?;
    let destination_addr = wait_for_mesh_data_plane(&destination_socket).await?.listen;

    let mut source_plan = plan_full_mesh(state, &NodeId::new("node-source"), 4)
        .map_err(|error| format!("plan source mesh: {error}"))?;
    patch_peer_endpoint(
        &mut source_plan,
        &NodeId::new("node-destination"),
        destination_addr,
    )?;
    let source_snapshot = root.join("node-source.wireguard.snapshot");
    write_applied_snapshot(
        &WireGuardSnapshotPaths {
            applied: source_snapshot.clone(),
        },
        &WireGuardAppliedSnapshot::from_plan(island.clone(), source_plan),
    )
    .map_err(|error| format!("write source mesh snapshot: {error}"))?;

    let source_socket = root.join("node-source-mesh.sock");
    let mut source_role = spawn_mesh_role(
        root,
        "machine-remove-source",
        &source_socket,
        &source_snapshot,
        loopback_any_port()?,
        island,
    )?;
    let _source_addr = wait_for_mesh_data_plane(&source_socket).await?.listen;
    let traffic_ok = send_between(
        &source_socket,
        &NodeId::new("node-destination"),
        b"after-remove",
    )
    .await?;
    let removed_peer_rejected = matches!(
        request_mesh_role(
            &source_socket,
            &MeshRoleRequest::Send {
                target_node_id: NodeId::new("node-target"),
                payload: b"removed".to_vec(),
            },
        )
        .await,
        Err(MeshRoleClientError::Failure(error))
            if error.kind == HarnessMeshFailureKind::PeerNotApplied
    );

    source_role.kill_and_wait().await?;
    destination_role.kill_and_wait().await?;
    Ok((usize::from(traffic_ok), removed_peer_rejected))
}

fn assert_initial_route_includes_target(state: &ProjectionState) -> Result<(), String> {
    let route = single_route(state)?;
    if route
        .backends
        .iter()
        .any(|backend| backend.node_id == NodeId::new("node-target"))
    {
        return Ok(());
    }
    Err("initial route did not include target backend".to_string())
}

fn assert_cutover_projection(state: &ProjectionState) -> Result<(), String> {
    let Some(removing) = state.removing_nodes.get(&NodeId::new("node-target")) else {
        return Err("removal-started fact did not project".to_string());
    };
    assert_eq_named("removal-started epoch", removing.epoch, 2)?;
    assert_eq_named(
        "removal-started reason",
        removing.reason.as_str(),
        "graceful-remove",
    )?;
    assert!(target_removed_from_active(state)?);
    assert!(target_kept_in_old_backends(state)?);
    Ok(())
}

fn target_removed_from_active(state: &ProjectionState) -> Result<bool, String> {
    Ok(!single_route(state)?
        .backends
        .iter()
        .any(|backend| backend.node_id == NodeId::new("node-target")))
}

fn target_kept_in_old_backends(state: &ProjectionState) -> Result<bool, String> {
    Ok(single_route(state)?
        .old_backends_to_drain
        .iter()
        .any(|backend| backend.node_id == NodeId::new("node-target")))
}

fn single_route(
    state: &ProjectionState,
) -> Result<&mvp_projection::GatewayRouteProjection, String> {
    let gateway = state
        .gateway
        .as_ref()
        .ok_or_else(|| "gateway projection missing".to_string())?;
    let [route] = gateway.routes.as_slice() else {
        return Err(format!(
            "expected one gateway route, got {}",
            gateway.routes.len()
        ));
    };
    Ok(route)
}

fn event_index(events: &[RemoveEvent], needle: RemoveEvent) -> Result<usize, String> {
    events
        .iter()
        .position(|event| *event == needle)
        .ok_or_else(|| format!("missing event {needle:?} in {events:?}"))
}

fn event_count(events: &[RemoveEvent], needle: RemoveEvent) -> usize {
    events.iter().filter(|event| **event == needle).count()
}

fn assert_visible_nodes(actual: &VisibleNodes, expected: &[NodeId]) -> Result<(), String> {
    let actual = actual.iter().cloned().collect::<Vec<_>>();
    assert_eq_named("visible node ids", actual, expected.to_vec())
}

fn patch_peer_endpoint(
    plan: &mut mvp_mesh::WireGuardPeerPlan,
    peer_node_id: &NodeId,
    endpoint: SocketAddr,
) -> Result<(), String> {
    let Some(peer) = plan
        .peers
        .iter_mut()
        .find(|peer| &peer.node_id == peer_node_id)
    else {
        return Err(format!(
            "plan for {} is missing peer {}",
            plan.local_node_id.as_str(),
            peer_node_id.as_str()
        ));
    };
    peer.endpoint = IrohEndpointId::new(endpoint.to_string());
    Ok(())
}

fn spawn_mesh_role(
    root: &std::path::Path,
    name: &'static str,
    socket: &std::path::Path,
    snapshot: &std::path::Path,
    listen: SocketAddr,
    island: &IslandId,
) -> Result<crate::process_role_harness::RunningChild, String> {
    let snapshot = snapshot.display().to_string();
    let listen = listen.to_string();
    let island = island.as_str().to_string();
    spawn_process_role(
        name,
        "mesh-data-plane",
        root,
        socket,
        &[
            "--snapshot",
            snapshot.as_str(),
            "--listen",
            listen.as_str(),
            "--island",
            island.as_str(),
        ],
    )
}

async fn send_between(
    source_socket: &std::path::Path,
    target_node_id: &NodeId,
    payload: &[u8],
) -> Result<bool, String> {
    match request_mesh_role(
        source_socket,
        &MeshRoleRequest::Send {
            target_node_id: target_node_id.clone(),
            payload: payload.to_vec(),
        },
    )
    .await
    {
        Ok(MeshRoleSuccess::Sent { payload: returned }) => Ok(returned == payload),
        other => Err(format!("unexpected mesh send response: {other:?}")),
    }
}

fn join_request(
    invite: MachineInvite,
    node_id: NodeId,
    epoch: u64,
    visible_count: u64,
) -> JoinRequest {
    let node = node_id.as_str().to_string();
    JoinRequest {
        invite,
        presented_secret: InviteSecret::new("secret"),
        node_id,
        epoch,
        iroh_endpoint_id: IrohEndpointId::new(format!("iroh-{node}")),
        wg_public_key: WireGuardPublicKey::new(format!("wg-{node}")),
        now_ms: 10,
        visible_nodes: VisibleNodes::new(
            (0..visible_count).map(|index| NodeId::new(format!("visible-{index}"))),
        ),
        tombstoned_nodes: BTreeSet::new(),
    }
}

fn invite(island: IslandId) -> MachineInvite {
    MachineInvite {
        island,
        bootstrap_endpoint: IrohEndpointId::new("iroh-bootstrap"),
        invite_id: InviteId::new("invite-1"),
        secret: InviteSecret::new("secret"),
        expires_at_ms: 60_000,
    }
}

fn initial_serving_commit() -> ServingCommitPlan {
    ServingCommitPlan {
        serving_commit_id: ServingCommitId::new("serving-before-remove"),
        route_commit_id: RouteCommitId::new("route-before-remove"),
        gateway_commit_id: GatewayCommitId::new("gateway-before-remove"),
        dns_commit_id: DnsCommitId::new("dns-before-remove"),
        route_id: RouteId::new("web"),
        hostnames: vec!["web.example.test".to_string()],
        active_backends: vec![
            BackendEndpoint {
                node_id: NodeId::new("node-target"),
                address: "fd00::10:8080".to_string(),
            },
            BackendEndpoint {
                node_id: NodeId::new("node-destination"),
                address: "fd00::12:8080".to_string(),
            },
        ],
        old_backends_to_drain: Vec::new(),
        dns_records: vec![DnsRecordFact {
            name: "web.example.test".to_string(),
            record_type: "AAAA".to_string(),
            value: "fd00::12".to_string(),
            ttl_seconds: 30,
        }],
        epoch: 1,
    }
}

fn remove_serving_commit() -> ServingCommitPlan {
    ServingCommitPlan {
        serving_commit_id: ServingCommitId::new("serving-after-remove"),
        route_commit_id: RouteCommitId::new("route-after-remove"),
        gateway_commit_id: GatewayCommitId::new("gateway-after-remove"),
        dns_commit_id: DnsCommitId::new("dns-after-remove"),
        route_id: RouteId::new("web"),
        hostnames: vec!["web.example.test".to_string()],
        active_backends: vec![BackendEndpoint {
            node_id: NodeId::new("node-destination"),
            address: "fd00::12:8080".to_string(),
        }],
        old_backends_to_drain: vec![BackendEndpoint {
            node_id: NodeId::new("node-target"),
            address: "fd00::10:8080".to_string(),
        }],
        dns_records: vec![DnsRecordFact {
            name: "web.example.test".to_string(),
            record_type: "AAAA".to_string(),
            value: "fd00::12".to_string(),
            ttl_seconds: 30,
        }],
        epoch: 2,
    }
}

fn handler_failed(subject: impl ToString) -> BusError {
    BusError::HandlerFailed {
        subject: subject.to_string(),
        failure: HandlerFailure::Application,
    }
}

fn loopback_any_port() -> Result<SocketAddr, String> {
    "127.0.0.1:0"
        .parse()
        .map_err(|error| format!("parse loopback any-port addr: {error}"))
}

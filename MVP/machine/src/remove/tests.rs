use std::sync::{Arc, Mutex};
use std::time::Duration;

use mvp_bus::{
    BusActorHandle, BusSession, FactContentHash, Grant, IslandId, PrincipalId, SubjectPattern,
};
use mvp_commands::{CommandContext, InMemoryCommandPhaseStore};
use mvp_identity::NodeId;
use mvp_projection::{
    BackendEndpoint, DnsRecordFact, GatewayProjection, GatewayRouteProjection, NodeProjection,
    ProjectionReport, RemovingNodeProjection, RouteId, SnapshotWriteReport,
};
use mvp_routing::{
    DnsCommitId, GatewayCommitId, RouteCommitId, RoutingError, RoutingResult, ServingFactWriter,
    WrittenServingFact, serving_commit_fact_key, serving_commit_fact_payload,
};

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordedEvent {
    Decision(NodeId),
    RemovalStarted(NodeId),
    Tombstone(NodeId),
    CleanupDone(NodeId),
    Probe,
    PrepareDrain,
    ServingCommit(ServingCommitId),
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordingServingOutcome {
    Succeed,
    Conflict,
}

#[derive(Clone, Default)]
struct RecordingFactWriter {
    events: Arc<Mutex<Vec<RecordedEvent>>>,
    fail_decision: bool,
}

impl RecordingFactWriter {
    fn failing_decision() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            fail_decision: true,
        }
    }

    fn events(&self) -> Vec<RecordedEvent> {
        self.events.lock().expect("event log").clone()
    }
}

impl MachineFactWriter for RecordingFactWriter {
    fn write_remove_decision<'a>(
        &'a self,
        fact: MachineRemoveDecisionFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = machine_remove_decision_fact_key(&fact.remove_id())?;
            if self.fail_decision {
                return Err(MachineRemoveError::FactConflict { key });
            }
            self.events
                .lock()
                .expect("event log")
                .push(RecordedEvent::Decision(fact.target_node_id.clone()));
            Ok(WrittenMachineFact { key })
        })
    }

    fn write_removal_started<'a>(
        &'a self,
        fact: NodeRemovalStartedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            self.events
                .lock()
                .expect("event log")
                .push(RecordedEvent::RemovalStarted(fact.node_id.clone()));
            Ok(WrittenMachineFact {
                key: removal_started_fact_key(&fact.node_id, fact.epoch)?,
            })
        })
    }

    fn write_tombstone<'a>(
        &'a self,
        fact: NodeTombstonedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            self.events
                .lock()
                .expect("event log")
                .push(RecordedEvent::Tombstone(fact.node_id.clone()));
            Ok(WrittenMachineFact {
                key: tombstone_fact_key(&fact.node_id, fact.epoch)?,
            })
        })
    }

    fn write_cleanup_done<'a>(
        &'a self,
        fact: MachineRemoveCleanupDoneFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            self.events
                .lock()
                .expect("event log")
                .push(RecordedEvent::CleanupDone(fact.target_node_id.clone()));
            Ok(WrittenMachineFact {
                key: machine_remove_cleanup_done_fact_key(&fact.remove_id())?,
            })
        })
    }
}

#[derive(Clone)]
struct RecordingServingFactWriter {
    events: Arc<Mutex<Vec<RecordedEvent>>>,
    outcome: RecordingServingOutcome,
}

impl RecordingServingFactWriter {
    fn succeeds(events: Arc<Mutex<Vec<RecordedEvent>>>) -> Self {
        Self {
            events,
            outcome: RecordingServingOutcome::Succeed,
        }
    }

    fn conflicts(events: Arc<Mutex<Vec<RecordedEvent>>>) -> Self {
        Self {
            events,
            outcome: RecordingServingOutcome::Conflict,
        }
    }
}

impl ServingFactWriter for RecordingServingFactWriter {
    fn write_serving_commit<'a>(
        &'a self,
        commit: &'a ServingCommitPlan,
    ) -> Pin<Box<dyn Future<Output = RoutingResult<WrittenServingFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = serving_commit_fact_key(&commit.serving_commit_id)?;
            match self.outcome {
                RecordingServingOutcome::Succeed => {
                    self.events
                        .lock()
                        .expect("event log")
                        .push(RecordedEvent::ServingCommit(
                            commit.serving_commit_id.clone(),
                        ));
                    let payload = serving_commit_fact_payload(commit)?;
                    Ok(WrittenServingFact::inserted(
                        key,
                        FactContentHash::for_payload(&payload),
                    ))
                }
                RecordingServingOutcome::Conflict => Err(RoutingError::ServingFactConflict { key }),
            }
        })
    }
}

#[tokio::test]
async fn missing_target_fails_before_fact_write() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    let coordinator = coordinator(bus, session, writer.clone());
    let mut request = remove_request();
    request.current_projection.nodes.clear();

    let error = execute_start(&coordinator, request, None)
        .await
        .expect_err("target missing");

    assert!(matches!(error, MachineRemoveError::TargetMissing { .. }));
    assert!(writer.events().is_empty());
}

#[tokio::test]
async fn tombstoned_target_fails_before_fact_write() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    let coordinator = coordinator(bus, session, writer.clone());
    let mut request = remove_request();
    request
        .current_projection
        .tombstoned_nodes
        .insert(NodeId::new("node-old"), 9);

    let error = execute_start(&coordinator, request, None)
        .await
        .expect_err("target tombstoned");

    assert!(matches!(error, MachineRemoveError::TargetTombstoned { .. }));
    assert!(writer.events().is_empty());
}

#[tokio::test]
async fn already_removing_target_fails_before_fact_write() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    let coordinator = coordinator(bus, session, writer.clone());
    let mut request = remove_request();
    request.current_projection.removing_nodes.insert(
        NodeId::new("node-old"),
        RemovingNodeProjection {
            node_id: NodeId::new("node-old"),
            epoch: 2,
            reason: "existing-remove".to_string(),
        },
    );

    let error = execute_start(&coordinator, request, None)
        .await
        .expect_err("target already removing");

    assert!(matches!(
        error,
        MachineRemoveError::TargetAlreadyRemoving { .. }
    ));
    assert!(writer.events().is_empty());
}

#[tokio::test]
async fn target_still_active_in_serving_commit_fails_before_fact_write() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    let coordinator = coordinator(bus, session, writer.clone());
    let mut request = remove_request();
    request
        .serving_commit
        .active_backends
        .push(BackendEndpoint {
            node_id: NodeId::new("node-old"),
            address: "fd00::1:8080".to_string(),
        });

    let error = execute_start(&coordinator, request, None)
        .await
        .expect_err("target still active");

    assert!(matches!(
        error,
        MachineRemoveError::InvalidServingCommit {
            reason: MachineRemoveValidationError::TargetStillActive,
            ..
        }
    ));
    assert!(writer.events().is_empty());
}

#[tokio::test]
async fn target_missing_from_drain_set_fails_before_fact_write() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    let coordinator = coordinator(bus, session, writer.clone());
    let mut request = remove_request();
    request.serving_commit.old_backends_to_drain.clear();

    let error = execute_start(&coordinator, request, None)
        .await
        .expect_err("target missing from drain set");

    assert!(matches!(
        error,
        MachineRemoveError::InvalidServingCommit {
            reason: MachineRemoveValidationError::TargetMissingFromDrainSet,
            ..
        }
    ));
    assert!(writer.events().is_empty());
}

#[tokio::test]
async fn no_prepare_responder_fails_before_fact_write() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    let coordinator = coordinator(bus, session, writer.clone());

    let error = execute_start(&coordinator, remove_request(), None)
        .await
        .expect_err("no responder");

    assert!(matches!(
        error,
        MachineRemoveError::Bus(mvp_bus::BusError::NoResponders { .. })
    ));
    assert!(writer.events().is_empty());
}

#[tokio::test]
async fn prepare_rejection_fails_before_serving_commit_or_tombstone() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    register_prepare(
        &bus,
        &session,
        writer.events.clone(),
        PrepareRemoveOutcome::NotDrained {
            reason: "busy".to_string(),
        },
    )
    .await;
    let coordinator = coordinator(bus, session, writer.clone());

    let error = execute_start(&coordinator, remove_request(), None)
        .await
        .expect_err("prepare rejected");

    assert!(matches!(
        error,
        MachineRemoveError::PrepareRemoveRejected { .. }
    ));
    assert_eq!(
        writer.events(),
        vec![
            RecordedEvent::Probe,
            RecordedEvent::Decision(NodeId::new("node-old")),
            RecordedEvent::RemovalStarted(NodeId::new("node-old")),
            RecordedEvent::PrepareDrain
        ]
    );
}

#[tokio::test]
async fn decision_fact_failure_stops_before_removal_started_or_drain() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::failing_decision();
    register_prepare(
        &bus,
        &session,
        writer.events.clone(),
        PrepareRemoveOutcome::NoNewWorkAndDrained,
    )
    .await;
    let coordinator = coordinator(bus, session, writer.clone());

    let error = execute_start(&coordinator, remove_request(), None)
        .await
        .expect_err("decision conflict");

    assert!(matches!(error, MachineRemoveError::FactConflict { .. }));
    assert_eq!(writer.events(), vec![RecordedEvent::Probe]);
}

#[tokio::test]
async fn serving_commit_failure_leaves_only_pre_cutover_intent() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    register_prepare(
        &bus,
        &session,
        writer.events.clone(),
        PrepareRemoveOutcome::NoNewWorkAndDrained,
    )
    .await;
    let request = remove_request();
    let serving_writer = RecordingServingFactWriter::conflicts(writer.events.clone());
    let coordinator = coordinator_with_serving(bus, session, writer.clone(), serving_writer);

    let error = execute_start(&coordinator, request, None)
        .await
        .expect_err("serving commit conflict");

    assert!(matches!(
        error,
        MachineRemoveError::Routing(mvp_routing::RoutingError::ServingFactConflict { .. })
    ));
    assert_eq!(
        writer.events(),
        vec![
            RecordedEvent::Probe,
            RecordedEvent::Decision(NodeId::new("node-old")),
            RecordedEvent::RemovalStarted(NodeId::new("node-old")),
            RecordedEvent::PrepareDrain
        ]
    );
}

#[tokio::test]
async fn projection_mismatch_returns_cleanup_pending_without_tombstone() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    register_prepare(
        &bus,
        &session,
        writer.events.clone(),
        PrepareRemoveOutcome::NoNewWorkAndDrained,
    )
    .await;
    register_stop(&bus, &session, writer.events.clone()).await;
    let coordinator = coordinator(bus, session, writer.clone());
    let mismatch_commit = alternate_commit();
    let mismatch =
        ProjectionCatchUp::from_report(&mismatch_commit, &projection_report_for(&mismatch_commit))
            .expect("mismatched catchup object");

    let result = execute_start(&coordinator, remove_request(), Some(mismatch))
        .await
        .expect("cleanup pending result");

    assert_eq!(result.outcome, MachineRemoveOutcome::CleanupPending);
    assert!(result.tombstone_fact_key.is_none());
    assert_eq!(
        writer.events(),
        vec![
            RecordedEvent::Probe,
            RecordedEvent::Decision(NodeId::new("node-old")),
            RecordedEvent::RemovalStarted(NodeId::new("node-old")),
            RecordedEvent::PrepareDrain,
            RecordedEvent::ServingCommit(ServingCommitId::new("serving-remove-1")),
        ]
    );
}

#[tokio::test]
async fn stop_failure_returns_cleanup_pending_without_tombstone() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    register_prepare(
        &bus,
        &session,
        writer.events.clone(),
        PrepareRemoveOutcome::NoNewWorkAndDrained,
    )
    .await;
    let coordinator = coordinator(bus, session, writer.clone());
    let request = remove_request();
    let catch_up = ProjectionCatchUp::from_report(
        &request.serving_commit,
        &projection_report_for(&request.serving_commit),
    )
    .expect("catchup");
    let result = execute_start(&coordinator, request, Some(catch_up))
        .await
        .expect("cleanup pending");

    assert_eq!(result.outcome, MachineRemoveOutcome::CleanupPending);
    assert!(matches!(
        result.cleanup_status,
        RemoveCleanupStatus::Pending {
            reason: RemoveCleanupPendingReason::StopUnavailable {
                cause: CleanupFailureKind::NoResponders,
                ..
            }
        }
    ));
    assert!(result.tombstone_fact_key.is_none());
}

#[tokio::test]
async fn stop_failure_reply_returns_cleanup_pending_without_tombstone() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    register_prepare(
        &bus,
        &session,
        writer.events.clone(),
        PrepareRemoveOutcome::NoNewWorkAndDrained,
    )
    .await;
    register_stop_with_outcome(
        &bus,
        &session,
        writer.events.clone(),
        StopRemovedWorkloadsOutcome::Failed {
            reason: "still draining".to_string(),
        },
    )
    .await;
    let coordinator = coordinator(bus, session, writer.clone());
    let request = remove_request();
    let catch_up = ProjectionCatchUp::from_report(
        &request.serving_commit,
        &projection_report_for(&request.serving_commit),
    )
    .expect("catchup");
    let result = execute_start(&coordinator, request, Some(catch_up))
        .await
        .expect("cleanup pending");

    assert_eq!(result.outcome, MachineRemoveOutcome::CleanupPending);
    assert!(matches!(
        result.cleanup_status,
        RemoveCleanupStatus::Pending {
            reason: RemoveCleanupPendingReason::StopUnavailable {
                cause: CleanupFailureKind::HandlerFailed,
                ..
            }
        }
    ));
    assert!(result.tombstone_fact_key.is_none());
    assert_eq!(
        writer.events(),
        vec![
            RecordedEvent::Probe,
            RecordedEvent::Decision(NodeId::new("node-old")),
            RecordedEvent::RemovalStarted(NodeId::new("node-old")),
            RecordedEvent::PrepareDrain,
            RecordedEvent::ServingCommit(ServingCommitId::new("serving-remove-1")),
            RecordedEvent::Stop,
        ]
    );
}

#[tokio::test]
async fn successful_remove_writes_intent_serving_stop_tombstone_in_order() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    register_prepare(
        &bus,
        &session,
        writer.events.clone(),
        PrepareRemoveOutcome::NoNewWorkAndDrained,
    )
    .await;
    register_stop(&bus, &session, writer.events.clone()).await;
    let coordinator = coordinator(bus, session, writer.clone());
    let request = remove_request();
    let catch_up = ProjectionCatchUp::from_report(
        &request.serving_commit,
        &projection_report_for(&request.serving_commit),
    )
    .expect("catchup");

    let result = execute_start(&coordinator, request, Some(catch_up))
        .await
        .expect("removed");

    assert_eq!(result.outcome, MachineRemoveOutcome::Removed);
    assert_eq!(result.visible_nodes.len(), 2);
    assert_eq!(
        result.tombstone_fact_key.expect("tombstone").as_str(),
        "/facts/node/node-old/tombstoned/3"
    );
    assert_eq!(
        writer.events(),
        vec![
            RecordedEvent::Probe,
            RecordedEvent::Decision(NodeId::new("node-old")),
            RecordedEvent::RemovalStarted(NodeId::new("node-old")),
            RecordedEvent::PrepareDrain,
            RecordedEvent::ServingCommit(ServingCommitId::new("serving-remove-1")),
            RecordedEvent::Stop,
            RecordedEvent::Tombstone(NodeId::new("node-old")),
            RecordedEvent::CleanupDone(NodeId::new("node-old")),
        ]
    );
}

#[tokio::test]
async fn phased_remove_resumes_after_serving_commit_without_precutover_replay() {
    let (bus, session) = test_bus();
    let writer = RecordingFactWriter::default();
    register_prepare(
        &bus,
        &session,
        writer.events.clone(),
        PrepareRemoveOutcome::NoNewWorkAndDrained,
    )
    .await;
    register_stop(&bus, &session, writer.events.clone()).await;
    let coordinator = coordinator(bus, session, writer.clone());
    let request = remove_request();
    let remove_id = MachineRemoveId::new(request.target_node_id.clone(), request.removal_epoch);
    let catch_up = ProjectionCatchUp::from_report(
        &request.serving_commit,
        &projection_report_for(&request.serving_commit),
    )
    .expect("catchup");
    let cx = CommandContext::new(Arc::new(InMemoryCommandPhaseStore::empty()));

    let pending = coordinator
        .execute_phased(
            &cx,
            MachineRemoveCommandInput::Start(Box::new(request)),
            None,
        )
        .await
        .expect("pending after serving commit");

    assert_eq!(pending.outcome, MachineRemoveOutcome::CleanupPending);
    assert!(matches!(
        pending.cleanup_status,
        RemoveCleanupStatus::Pending {
            reason: RemoveCleanupPendingReason::ProjectionCatchUpMissing { .. }
        }
    ));
    assert_eq!(
        writer.events(),
        vec![
            RecordedEvent::Probe,
            RecordedEvent::Decision(NodeId::new("node-old")),
            RecordedEvent::RemovalStarted(NodeId::new("node-old")),
            RecordedEvent::PrepareDrain,
            RecordedEvent::ServingCommit(ServingCommitId::new("serving-remove-1")),
        ]
    );

    let result = coordinator
        .execute_phased(
            &cx,
            MachineRemoveCommandInput::Resume(remove_id),
            Some(catch_up),
        )
        .await
        .expect("resumed remove");

    assert_eq!(result.outcome, MachineRemoveOutcome::Removed);
    assert_eq!(
        writer.events(),
        vec![
            RecordedEvent::Probe,
            RecordedEvent::Decision(NodeId::new("node-old")),
            RecordedEvent::RemovalStarted(NodeId::new("node-old")),
            RecordedEvent::PrepareDrain,
            RecordedEvent::ServingCommit(ServingCommitId::new("serving-remove-1")),
            RecordedEvent::Stop,
            RecordedEvent::Tombstone(NodeId::new("node-old")),
            RecordedEvent::CleanupDone(NodeId::new("node-old")),
        ]
    );
}

fn coordinator(
    bus: BusActorHandle,
    session: BusSession,
    writer: RecordingFactWriter,
) -> MachineRemoveCoordinator<RecordingFactWriter, RecordingServingFactWriter> {
    let serving_writer = RecordingServingFactWriter::succeeds(writer.events.clone());
    coordinator_with_serving(bus, session, writer, serving_writer)
}

async fn execute_start(
    coordinator: &MachineRemoveCoordinator<RecordingFactWriter, RecordingServingFactWriter>,
    request: MachineRemoveRequest,
    projection: Option<ProjectionCatchUp>,
) -> MachineRemoveResult<MachineRemoveCommandResult> {
    let cx = CommandContext::new(Arc::new(InMemoryCommandPhaseStore::empty()));
    coordinator
        .execute_phased(
            &cx,
            MachineRemoveCommandInput::Start(Box::new(request)),
            projection,
        )
        .await
}

fn coordinator_with_serving(
    bus: BusActorHandle,
    session: BusSession,
    writer: RecordingFactWriter,
    serving_writer: RecordingServingFactWriter,
) -> MachineRemoveCoordinator<RecordingFactWriter, RecordingServingFactWriter> {
    MachineRemoveCoordinator::with_fact_writers(
        bus,
        session,
        writer,
        serving_writer,
        MachineRemoveTimeouts {
            participant: Duration::from_secs(1),
        },
    )
}

fn test_bus() -> (BusActorHandle, BusSession) {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let session = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    (bus, session)
}

async fn register_prepare(
    bus: &BusActorHandle,
    session: &BusSession,
    events: Arc<Mutex<Vec<RecordedEvent>>>,
    drain_outcome: PrepareRemoveOutcome,
) {
    bus.subscribe(
        session,
        SubjectPattern::parse("node.node-old.rpc.prepare_remove").expect("pattern"),
        move |ctx| {
            let request: PrepareRemoveRequest =
                serde_json::from_slice(ctx.message.payload().as_bytes()).map_err(|_| {
                    mvp_bus::BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: mvp_bus::HandlerFailure::Application,
                    }
                })?;
            let outcome = match request.intent {
                PrepareRemoveIntent::Probe => {
                    events.lock().expect("event log").push(RecordedEvent::Probe);
                    PrepareRemoveOutcome::ResponderReady
                }
                PrepareRemoveIntent::Drain => {
                    events
                        .lock()
                        .expect("event log")
                        .push(RecordedEvent::PrepareDrain);
                    drain_outcome.clone()
                }
            };
            ctx.reply(
                serde_json::to_vec(&PrepareRemoveReply {
                    target_node_id: request.target_node_id,
                    outcome,
                })
                .map_err(|_| mvp_bus::BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: mvp_bus::HandlerFailure::Application,
                })?,
            )
        },
    )
    .await
    .expect("register prepare");
}

async fn register_stop(
    bus: &BusActorHandle,
    session: &BusSession,
    events: Arc<Mutex<Vec<RecordedEvent>>>,
) {
    register_stop_with_outcome(bus, session, events, StopRemovedWorkloadsOutcome::Stopped).await;
}

async fn register_stop_with_outcome(
    bus: &BusActorHandle,
    session: &BusSession,
    events: Arc<Mutex<Vec<RecordedEvent>>>,
    outcome: StopRemovedWorkloadsOutcome,
) {
    bus.subscribe(
        session,
        SubjectPattern::parse("node.node-old.rpc.stop_removed_workloads").expect("pattern"),
        move |ctx| {
            let request: StopRemovedWorkloadsRequest =
                serde_json::from_slice(ctx.message.payload().as_bytes()).map_err(|_| {
                    mvp_bus::BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: mvp_bus::HandlerFailure::Application,
                    }
                })?;
            events.lock().expect("event log").push(RecordedEvent::Stop);
            ctx.reply(
                serde_json::to_vec(&StopRemovedWorkloadsReply {
                    target_node_id: request.target_node_id,
                    outcome: outcome.clone(),
                })
                .map_err(|_| mvp_bus::BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: mvp_bus::HandlerFailure::Application,
                })?,
            )
        },
    )
    .await
    .expect("register stop");
}

fn remove_request() -> MachineRemoveRequest {
    MachineRemoveRequest {
        target_node_id: NodeId::new("node-old"),
        removal_epoch: 2,
        tombstone_epoch: 3,
        reason: "graceful-remove".to_string(),
        visible_nodes: VisibleNodes::new([NodeId::new("node-old"), NodeId::new("node-new")]),
        current_projection: projection_state(),
        serving_commit: serving_commit(),
    }
}

fn projection_state() -> ProjectionState {
    let mut state = ProjectionState::for_island(IslandId::new("prod"));
    for node in ["node-old", "node-new"] {
        state.nodes.insert(
            NodeId::new(node),
            NodeProjection {
                node_id: NodeId::new(node),
                epoch: 1,
                overlay_ip: format!("fd00::{node}"),
                iroh_endpoint_id: format!("iroh-{node}"),
                wg_public_key: format!("wg-{node}"),
            },
        );
    }
    state
}

fn serving_commit() -> ServingCommitPlan {
    ServingCommitPlan {
        serving_commit_id: ServingCommitId::new("serving-remove-1"),
        route_commit_id: RouteCommitId::new("route-remove-1"),
        gateway_commit_id: GatewayCommitId::new("gateway-remove-1"),
        dns_commit_id: DnsCommitId::new("dns-remove-1"),
        route_id: RouteId::new("web"),
        hostnames: vec!["web.example.test".to_string()],
        active_backends: vec![BackendEndpoint {
            node_id: NodeId::new("node-new"),
            address: "fd00::2:8080".to_string(),
        }],
        old_backends_to_drain: vec![BackendEndpoint {
            node_id: NodeId::new("node-old"),
            address: "fd00::1:8080".to_string(),
        }],
        dns_records: vec![DnsRecordFact {
            name: "web.example.test".to_string(),
            record_type: "AAAA".to_string(),
            value: "fd00::2".to_string(),
            ttl_seconds: 30,
        }],
        epoch: 1,
    }
}

fn alternate_commit() -> ServingCommitPlan {
    let mut commit = serving_commit();
    commit.serving_commit_id = ServingCommitId::new("serving-other");
    commit.route_commit_id = RouteCommitId::new("route-other");
    commit.gateway_commit_id = GatewayCommitId::new("gateway-other");
    commit.dns_commit_id = DnsCommitId::new("dns-other");
    commit
}

fn projection_report_for(commit: &ServingCommitPlan) -> ProjectionReport {
    let mut state = ProjectionState::for_island(IslandId::new("prod"));
    state.gateway = Some(GatewayProjection {
        gateway_commit_id: commit.gateway_commit_id.to_string(),
        route_commit_id: commit.route_commit_id.to_string(),
        routes: vec![GatewayRouteProjection {
            route_id: commit.route_id.clone(),
            hostnames: commit.hostnames.clone(),
            backends: commit.active_backends.clone(),
            old_backends_to_drain: commit.old_backends_to_drain.clone(),
        }],
    });
    state.dns = Some(mvp_projection::DnsProjection {
        dns_commit_id: commit.dns_commit_id.to_string(),
        records: commit
            .dns_records
            .clone()
            .into_iter()
            .map(mvp_projection::DnsRecordProjection::from)
            .collect(),
    });
    ProjectionReport {
        state,
        sqlite_path: std::path::PathBuf::from("unused.sqlite"),
        gateway_snapshot: Some(SnapshotWriteReport {
            path: std::path::PathBuf::from("gateway.snapshot"),
            bytes_written: 1,
            revision: format!(
                "gateway:{}:{}",
                commit.gateway_commit_id, commit.route_commit_id
            ),
        }),
        dns_snapshot: Some(SnapshotWriteReport {
            path: std::path::PathBuf::from("dns.snapshot"),
            bytes_written: 1,
            revision: format!("dns:{}", commit.dns_commit_id),
        }),
        duration: Duration::from_millis(1),
    }
}

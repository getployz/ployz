use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mvp_bus::{
    BusActorHandle, BusError, BusSession, FactContentHash, FactKeyPattern, Grant, HandlerFailure,
    IslandId, PrincipalId, SubjectPattern,
};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_projection::{
    BackendEndpoint, DnsProjection, DnsRecordFact, DnsRecordProjection, GatewayProjection,
    GatewayRouteProjection, ProjectionReport, ProjectionState, RouteId, SnapshotWriteReport,
    fixtures::BusFactFixtureSource,
};
use mvp_routing::{ServingFactWriter, WrittenServingFact, write_serving_commit};

use crate::wire::{decode, encode};
use crate::{
    BusDeployFactWriter, CandidateCleanupState, CandidateCleanupStatus, CandidateCleanupTarget,
    CapacityRequest, CleanupDeployCandidatesRequest, CleanupFailureKind, CleanupStatus,
    DeployDecisionCandidate, DeployDecisionFact, DeployError, DeployFactWriteStatus,
    DeployFactWriter, DeployId, DeployManifest, DeployOutcome, DeployRecovery, DeployStateMachine,
    DeployTimeouts, DnsCommitId, DrainInstanceRequest, GatewayCommitId,
    InstanceCapacityRequirement, InstanceCommandReply, InstanceCommandRequest, InstanceId,
    InstancePlan, InstanceStartOutcome, PhaseId, PhasePolicy, PhaseReversibility,
    ProjectionCatchUp, RevisionId, RouteCommitId, ServingCommitId, ServingCommitPlan,
    StopInstanceRequest, WrittenDeployFact, decode_deploy_decision_fact,
    deploy_cleanup_done_fact_key, deploy_decision_fact_key, deploy_decision_fact_payload,
    read_deploy_decision, select_deploy_decision,
};

fn serving_commit() -> ServingCommitPlan {
    ServingCommitPlan {
        serving_commit_id: ServingCommitId::new("serving-commit-1"),
        route_commit_id: RouteCommitId::new("route-commit-1"),
        gateway_commit_id: GatewayCommitId::new("gateway-commit-1"),
        dns_commit_id: DnsCommitId::new("dns-commit-1"),
        route_id: RouteId::new("web"),
        hostnames: vec!["app.example.test".to_string()],
        active_backends: vec![BackendEndpoint {
            node_id: NodeId::new("node-new"),
            address: "fd00::2:8080".to_string(),
        }],
        old_backends_to_drain: vec![BackendEndpoint {
            node_id: NodeId::new("node-old"),
            address: "fd00::1:8080".to_string(),
        }],
        dns_records: vec![DnsRecordFact {
            name: "app.example.test".to_string(),
            record_type: "AAAA".to_string(),
            value: "fd00::2".to_string(),
            ttl_seconds: 30,
        }],
        epoch: 1,
    }
}

fn manifest(deploy_id: &str, serving_epoch: u64) -> DeployManifest {
    let mut serving = serving_commit();
    serving.epoch = serving_epoch;
    DeployManifest::new(DeployId::new(deploy_id), Vec::new(), serving)
}

fn visible_nodes() -> VisibleNodes {
    VisibleNodes::new([NodeId::new("node-new"), NodeId::new("node-old")])
}

fn decision_fact(deploy_id: &str, serving_epoch: u64) -> DeployDecisionFact {
    DeployDecisionFact::new(manifest(deploy_id, serving_epoch), visible_nodes())
}

fn coordinator_manifest() -> DeployManifest {
    DeployManifest::new(
        DeployId::new("deploy-coordinator"),
        vec![crate::PhasePlan::new(
            PhaseId::new(1),
            vec![InstancePlan::new(
                InstanceId::new("web-1"),
                NodeId::new("node-new"),
                mvp_projection::ServiceName::new("web"),
                RevisionId::new("rev-web"),
                InstanceCapacityRequirement::General,
            )],
            PhasePolicy::serving(PhaseReversibility::Reversible),
        )],
        serving_commit(),
    )
}

fn test_timeouts() -> DeployTimeouts {
    DeployTimeouts {
        capacity: Duration::from_millis(50),
        participant: Duration::from_millis(50),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CoordinatorEvent {
    Decision,
    Prepare,
    Start,
    Serving,
}

#[derive(Clone, Default)]
struct RecordingDeployFactWriter {
    events: Arc<Mutex<Vec<CoordinatorEvent>>>,
}

impl DeployFactWriter for RecordingDeployFactWriter {
    fn write_decision<'a>(
        &'a self,
        fact: DeployDecisionFact,
    ) -> Pin<Box<dyn Future<Output = crate::DeployResult<WrittenDeployFact>> + Send + 'a>> {
        Box::pin(async move {
            self.events
                .lock()
                .expect("recording deploy events")
                .push(CoordinatorEvent::Decision);
            Ok(WrittenDeployFact::inserted(
                deploy_decision_fact_key(&fact.deploy_id)?,
                FactContentHash::new("b3:decision"),
            ))
        })
    }

    fn write_cleanup_done<'a>(
        &'a self,
        fact: crate::DeployCleanupDoneFact,
    ) -> Pin<Box<dyn Future<Output = crate::DeployResult<WrittenDeployFact>> + Send + 'a>> {
        Box::pin(async move {
            Ok(WrittenDeployFact::inserted(
                deploy_cleanup_done_fact_key(&fact.deploy_id)?,
                FactContentHash::new("b3:cleanup"),
            ))
        })
    }
}

#[derive(Clone, Default)]
struct RecordingServingFactWriter {
    events: Arc<Mutex<Vec<CoordinatorEvent>>>,
}

impl ServingFactWriter for RecordingServingFactWriter {
    fn write_serving_commit<'a>(
        &'a self,
        commit: &'a ServingCommitPlan,
    ) -> Pin<Box<dyn Future<Output = mvp_routing::RoutingResult<WrittenServingFact>> + Send + 'a>>
    {
        Box::pin(async move {
            self.events
                .lock()
                .expect("recording serving events")
                .push(CoordinatorEvent::Serving);
            Ok(WrittenServingFact::inserted(
                mvp_routing::serving_commit_fact_key(&commit.serving_commit_id)?,
                FactContentHash::new("b3:serving"),
            ))
        })
    }
}

fn recording_writers() -> (
    Arc<Mutex<Vec<CoordinatorEvent>>>,
    RecordingDeployFactWriter,
    RecordingServingFactWriter,
) {
    let events = Arc::new(Mutex::new(Vec::new()));
    (
        Arc::clone(&events),
        RecordingDeployFactWriter {
            events: Arc::clone(&events),
        },
        RecordingServingFactWriter { events },
    )
}

#[test]
fn drain_before_serving_commit_is_rejected() {
    let mut state = DeployStateMachine::new(DeployId::new("deploy-1"), [PhaseId::new(1)]);

    let error = state.finish_cleanup().expect_err("cleanup requires commit");

    assert!(matches!(error, DeployError::ServingCommitRequired));
}

#[test]
fn phase_commit_requires_readiness() {
    let mut state = DeployStateMachine::new(DeployId::new("deploy-1"), [PhaseId::new(1)]);

    let error = state
        .commit_phase(PhaseId::new(1), PhasePolicy::irreversible())
        .expect_err("phase cannot commit before ready");

    assert!(matches!(error, DeployError::PhaseNotReady { .. }));
}

#[test]
fn cleanup_pending_preserves_serving_commit_success() {
    let mut state = DeployStateMachine::new(DeployId::new("deploy-1"), [PhaseId::new(1)]);
    state.mark_preparing(PhaseId::new(1)).unwrap();
    state.mark_ready(PhaseId::new(1)).unwrap();
    state
        .commit_phase(PhaseId::new(1), PhasePolicy::reversible())
        .unwrap();
    state
        .commit_serving(serving_commit().serving_commit_id)
        .unwrap();

    let result = state
        .cleanup_pending(CleanupStatus::Pending {
            reason: crate::CleanupPendingReason::StopUnavailable {
                node_id: NodeId::new("node-old"),
                cause: CleanupFailureKind::NoResponders,
            },
        })
        .unwrap();

    assert_eq!(result.outcome, DeployOutcome::CleanupPending);
    assert_eq!(
        result.serving_commit_id.as_ref().map(ToString::to_string),
        Some("serving-commit-1".to_string())
    );
}

#[test]
fn irreversible_phase_failure_is_loud() {
    let mut state = DeployStateMachine::new(DeployId::new("deploy-1"), [PhaseId::new(1)]);
    state.mark_preparing(PhaseId::new(1)).unwrap();
    state.mark_ready(PhaseId::new(1)).unwrap();
    state
        .commit_phase(PhaseId::new(1), PhasePolicy::irreversible())
        .unwrap();

    let error = state
        .block_after_irreversible()
        .expect_err("irreversible phase should block");

    assert!(matches!(error, DeployError::BlockedAfterIrreversiblePhase));
    assert_eq!(
        state.result().expect("terminal result").outcome,
        DeployOutcome::DeployBlockedAfterIrreversiblePhase
    );
}

#[tokio::test]
async fn conflicting_serving_commit_fact_rejects_cutover() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let session = authority.grant_in(
        mvp_bus::IslandId::new("prod"),
        mvp_bus::PrincipalId::new("deploy"),
        mvp_bus::Grant::allow_all(),
    );
    let commit = serving_commit();
    write_serving_commit(&bus, &session, &commit)
        .await
        .expect("first serving commit writes");

    let mut conflicting = commit.clone();
    conflicting.hostnames = vec!["other.example.test".to_string()];

    let error = write_serving_commit(&bus, &session, &conflicting)
        .await
        .expect_err("conflicting serving commit should be rejected");

    assert!(matches!(
        error,
        mvp_routing::RoutingError::ServingFactConflict { .. }
    ));
}

#[test]
fn projection_catch_up_allows_unrelated_gateway_revision_suffix() {
    let commit = serving_commit();
    let report = projection_report_for_commit(
        &commit,
        "gateway:gateway-commit-1:route-commit-1:acme:none",
        "dns:dns-commit-1",
    );

    let proof = ProjectionCatchUp::from_report(&commit, &report).expect("catch-up proof");

    assert_eq!(proof.serving_commit_id(), &commit.serving_commit_id);
}

#[tokio::test]
async fn capacity_failure_writes_no_deploy_decision() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let (events, deploy_writer, serving_writer) = recording_writers();
    let coordinator = crate::DeployCoordinator::with_fact_writers(
        bus,
        operator,
        deploy_writer,
        serving_writer,
        test_timeouts(),
    );

    let error = coordinator
        .execute_until_serving_commit(coordinator_manifest())
        .await
        .expect_err("missing capacity should fail before decision");

    assert!(matches!(error, DeployError::PlannedNodeNotVisible { .. }));
    assert!(events.lock().expect("recording events").is_empty());
}

#[tokio::test]
async fn prepare_failure_writes_decision_but_no_serving_commit() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("node"),
        Grant::allow_all(),
    );
    register_capacity(&bus, &node).await;
    let (events, deploy_writer, serving_writer) = recording_writers();
    let coordinator = crate::DeployCoordinator::with_fact_writers(
        bus,
        operator,
        deploy_writer,
        serving_writer,
        test_timeouts(),
    );

    let error = coordinator
        .execute_until_serving_commit(coordinator_manifest())
        .await
        .expect_err("missing prepare responder should fail after decision");

    assert!(matches!(
        error,
        DeployError::Bus(mvp_bus::BusError::NoResponders { .. })
    ));
    assert_eq!(
        events.lock().expect("recording events").as_slice(),
        [CoordinatorEvent::Decision]
    );
}

#[tokio::test]
async fn serving_commit_success_writes_decision_before_participant_mutation() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("node"),
        Grant::allow_all(),
    );
    let (events, deploy_writer, serving_writer) = recording_writers();
    register_capacity(&bus, &node).await;
    register_instance_participants(&bus, &node, Arc::clone(&events)).await;
    let coordinator = crate::DeployCoordinator::with_fact_writers(
        bus,
        operator,
        deploy_writer,
        serving_writer,
        test_timeouts(),
    );

    coordinator
        .execute_until_serving_commit(coordinator_manifest())
        .await
        .expect("deploy reaches serving commit");

    assert_eq!(
        events.lock().expect("recording events").as_slice(),
        [
            CoordinatorEvent::Decision,
            CoordinatorEvent::Prepare,
            CoordinatorEvent::Start,
            CoordinatorEvent::Serving,
        ]
    );
}

#[tokio::test]
async fn cleanup_requests_include_explicit_cleanup_target() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("node"),
        Grant::allow_all(),
    );
    let (events, deploy_writer, serving_writer) = recording_writers();
    let drained = Arc::new(Mutex::new(Vec::new()));
    let stopped = Arc::new(Mutex::new(Vec::new()));
    register_capacity(&bus, &node).await;
    register_instance_participants(&bus, &node, Arc::clone(&events)).await;
    register_cleanup_participants(&bus, &node, Arc::clone(&drained), Arc::clone(&stopped)).await;
    let coordinator = crate::DeployCoordinator::with_fact_writers(
        bus,
        operator,
        deploy_writer,
        serving_writer,
        test_timeouts(),
    );
    let manifest = coordinator_manifest();
    let expected_target = manifest.serving_commit.old_backends_to_drain[0].clone();
    let pending = coordinator
        .execute_until_serving_commit(manifest.clone())
        .await
        .expect("deploy reaches serving commit");
    let projected = pending
        .after_projection(
            ProjectionCatchUp::from_report(
                &manifest.serving_commit,
                &projection_report_for_commit(
                    &manifest.serving_commit,
                    "gateway:gateway-commit-1:route-commit-1",
                    "dns:dns-commit-1",
                ),
            )
            .expect("projection catch-up"),
        )
        .expect("pending accepts projection");

    coordinator
        .finish_cleanup(projected)
        .await
        .expect("cleanup finishes");

    assert_eq!(
        drained.lock().expect("drained targets").as_slice(),
        std::slice::from_ref(&expected_target)
    );
    assert_eq!(
        stopped.lock().expect("stopped targets").as_slice(),
        std::slice::from_ref(&expected_target)
    );
}

#[test]
fn candidate_cleanup_request_round_trips_with_typed_candidate_state() {
    let request = CleanupDeployCandidatesRequest {
        deploy_id: DeployId::new("deploy-cleanup-candidate"),
        candidates: vec![CandidateCleanupTarget::new(
            InstanceId::new("web-1"),
            mvp_projection::ServiceName::new("web"),
            RevisionId::new("rev-web"),
            CandidateCleanupState::Prepared,
        )],
    };

    let bytes = encode(&request, "candidate cleanup request").expect("encode request");
    let payload: mvp_bus::Payload = bytes.into();
    let decoded: CleanupDeployCandidatesRequest =
        decode(&payload, "candidate cleanup request").expect("decode request");

    assert_eq!(decoded, request);
}

#[tokio::test]
async fn prepare_handler_failure_cleans_attempted_candidate() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("node"),
        Grant::allow_all(),
    );
    let (events, deploy_writer, serving_writer) = recording_writers();
    let cleaned = Arc::new(Mutex::new(Vec::new()));
    register_capacity(&bus, &node).await;
    register_prepare_failure_participant(&bus, &node).await;
    register_candidate_cleanup_participant(&bus, &node, Arc::clone(&cleaned)).await;
    let coordinator = crate::DeployCoordinator::with_fact_writers(
        bus,
        operator,
        deploy_writer,
        serving_writer,
        test_timeouts(),
    );

    let error = coordinator
        .execute_until_serving_commit(coordinator_manifest())
        .await
        .expect_err("prepare handler failure should clean attempted candidate");

    let DeployError::PreCommitFailed { source, cleanup } = error else {
        panic!("expected pre-commit cleanup error");
    };
    assert!(matches!(
        *source,
        DeployError::Bus(BusError::HandlerFailed { .. })
    ));
    assert_eq!(cleanup.status, CandidateCleanupStatus::Done);
    assert_eq!(
        cleanup.visible_nodes,
        VisibleNodes::new([NodeId::new("node-new")])
    );
    let [attempted] = cleanup.attempted.as_slice() else {
        panic!("expected one candidate cleanup target");
    };
    assert_eq!(attempted.node_id, NodeId::new("node-new"));
    assert_eq!(
        attempted.candidates[0].state,
        CandidateCleanupState::PrepareAttempted
    );
    assert_eq!(cleaned.lock().expect("candidate cleanup requests").len(), 1);
    assert_eq!(
        events.lock().expect("recording events").as_slice(),
        [CoordinatorEvent::Decision]
    );
}

#[tokio::test]
async fn pre_commit_recovery_cleans_planned_candidates_without_rerunning_prepare() {
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("node"),
        Grant::allow_all(),
    );
    let cleaned = Arc::new(Mutex::new(Vec::new()));
    let manifest = coordinator_manifest();
    BusDeployFactWriter::new(bus.clone(), operator.clone())
        .write_decision(DeployDecisionFact::new(manifest.clone(), visible_nodes()))
        .await
        .expect("write pre-commit decision");
    register_candidate_cleanup_participant(&bus, &node, Arc::clone(&cleaned)).await;
    let source = BusFactFixtureSource::new(raw_bus);
    let coordinator = crate::DeployCoordinator::new(bus, operator.clone(), test_timeouts());
    let DeployRecovery::PreCommitIncomplete(recovery) = coordinator
        .recover_pending_cleanup(&source, operator.island(), &operator, &manifest.deploy_id)
        .expect("recover pre-commit incomplete")
    else {
        panic!("expected pre-commit incomplete recovery");
    };

    let cleanup = coordinator.cleanup_pre_commit_incomplete(&recovery).await;

    assert_eq!(cleanup.status, CandidateCleanupStatus::Done);
    assert_eq!(cleanup.visible_nodes, visible_nodes());
    let [attempted] = cleanup.attempted.as_slice() else {
        panic!("expected one planned candidate cleanup target");
    };
    assert_eq!(
        attempted.candidates[0].state,
        CandidateCleanupState::Planned
    );
    let requests = cleaned.lock().expect("candidate cleanup requests");
    let [request] = requests.as_slice() else {
        panic!("expected one cleanup request");
    };
    assert_eq!(request.deploy_id, manifest.deploy_id);
    assert_eq!(request.candidates[0].state, CandidateCleanupState::Planned);
}

#[tokio::test]
async fn recovery_with_missing_decision_returns_structured_missing_fact() {
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let source = BusFactFixtureSource::new(raw_bus);
    let coordinator = crate::DeployCoordinator::new(bus, operator.clone(), test_timeouts());

    let error = coordinator
        .recover_pending_cleanup(
            &source,
            operator.island(),
            &operator,
            &DeployId::new("deploy-missing"),
        )
        .expect_err("missing decision should fail");

    assert!(matches!(error, DeployError::DeployFactMissing { .. }));
}

#[tokio::test]
async fn recovery_with_decision_but_no_serving_commit_is_pre_commit_incomplete() {
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let manifest = coordinator_manifest();
    BusDeployFactWriter::new(bus.clone(), operator.clone())
        .write_decision(DeployDecisionFact::new(manifest.clone(), visible_nodes()))
        .await
        .expect("write decision");
    let source = BusFactFixtureSource::new(raw_bus);
    let coordinator = crate::DeployCoordinator::new(bus, operator.clone(), test_timeouts());

    let recovery = coordinator
        .recover_pending_cleanup(&source, operator.island(), &operator, &manifest.deploy_id)
        .expect("recover pre-commit incomplete");

    let DeployRecovery::PreCommitIncomplete(status) = recovery else {
        panic!("expected pre-commit incomplete recovery");
    };
    assert_eq!(status.manifest.deploy_id, manifest.deploy_id);
    assert_eq!(status.visible_nodes, visible_nodes());
    assert!(status.superseded_decisions.is_empty());
    assert_eq!(
        status.manifest.serving_commit.serving_commit_id,
        manifest.serving_commit.serving_commit_id
    );
}

#[tokio::test]
async fn pre_commit_recovery_cleans_superseded_decision_candidates() {
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("node"),
        Grant::allow_all(),
    );
    let cleaned = Arc::new(Mutex::new(Vec::new()));
    let deploy_id = DeployId::new("deploy-superseded");
    let mut older = coordinator_manifest();
    older.deploy_id = deploy_id.clone();
    older.serving_commit.epoch = 1;
    older.serving_commit.serving_commit_id = crate::ServingCommitId::new("older-serving");
    older.phases[0].instances[0].instance_id = InstanceId::new("older-web");
    let mut newer = coordinator_manifest();
    newer.deploy_id = deploy_id.clone();
    newer.serving_commit.epoch = 2;
    newer.serving_commit.serving_commit_id = crate::ServingCommitId::new("newer-serving");
    newer.phases[0].instances[0].instance_id = InstanceId::new("newer-web");
    let writer = BusDeployFactWriter::new(bus.clone(), operator.clone());
    writer
        .write_decision(DeployDecisionFact::new(older.clone(), visible_nodes()))
        .await
        .expect("write older decision");
    writer
        .write_decision(DeployDecisionFact::new(newer.clone(), visible_nodes()))
        .await
        .expect_err("newer decision is stored as a conflict candidate");
    register_candidate_cleanup_participant(&bus, &node, Arc::clone(&cleaned)).await;
    let source = BusFactFixtureSource::new(raw_bus);
    let coordinator = crate::DeployCoordinator::new(bus, operator.clone(), test_timeouts());
    let DeployRecovery::PreCommitIncomplete(recovery) = coordinator
        .recover_pending_cleanup(&source, operator.island(), &operator, &deploy_id)
        .expect("recover pre-commit incomplete")
    else {
        panic!("expected pre-commit incomplete recovery");
    };
    assert_eq!(recovery.manifest.deploy_id, deploy_id);
    assert_eq!(recovery.manifest.serving_commit.epoch, 2);
    assert_eq!(recovery.superseded_decisions.len(), 1);

    let cleanup = coordinator.cleanup_pre_commit_incomplete(&recovery).await;

    assert_eq!(cleanup.status, CandidateCleanupStatus::Done);
    let [attempted] = cleanup.attempted.as_slice() else {
        panic!("expected one node cleanup target");
    };
    let instance_ids = attempted
        .candidates
        .iter()
        .map(|candidate| candidate.instance_id.as_str())
        .collect::<Vec<_>>();
    assert!(instance_ids.contains(&"older-web"));
    assert!(instance_ids.contains(&"newer-web"));
}

#[tokio::test]
async fn recovery_after_serving_commit_resumes_cleanup_after_projection() {
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("node"),
        Grant::allow_all(),
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let drained = Arc::new(Mutex::new(Vec::new()));
    let stopped = Arc::new(Mutex::new(Vec::new()));
    register_capacity(&bus, &node).await;
    register_instance_participants(&bus, &node, Arc::clone(&events)).await;
    register_cleanup_participants(&bus, &node, Arc::clone(&drained), Arc::clone(&stopped)).await;
    let manifest = coordinator_manifest();
    let old_backend = manifest.serving_commit.old_backends_to_drain[0].clone();
    crate::DeployCoordinator::new(bus.clone(), operator.clone(), test_timeouts())
        .execute_until_serving_commit(manifest.clone())
        .await
        .expect("initial coordinator reaches serving commit");
    let event_count_after_commit = events.lock().expect("events").len();
    let source = BusFactFixtureSource::new(raw_bus);
    let restarted = crate::DeployCoordinator::new(bus, operator.clone(), test_timeouts());

    let recovery = restarted
        .recover_pending_cleanup(&source, operator.island(), &operator, &manifest.deploy_id)
        .expect("recover pending cleanup");
    let DeployRecovery::Pending(recovered) = recovery else {
        panic!("expected pending cleanup recovery");
    };
    assert_eq!(recovered.manifest(), &manifest);
    assert!(recovered.superseded_decisions().is_empty());
    assert!(drained.lock().expect("drained targets").is_empty());
    assert_eq!(
        events.lock().expect("events").len(),
        event_count_after_commit
    );

    let projected = (*recovered)
        .after_projection(projection_for_manifest(&manifest))
        .expect("projection proof accepted");
    restarted
        .finish_cleanup(projected)
        .await
        .expect("recovered cleanup finishes");

    assert_eq!(
        drained.lock().expect("drained targets").as_slice(),
        std::slice::from_ref(&old_backend)
    );
    assert_eq!(
        stopped.lock().expect("stopped targets").as_slice(),
        std::slice::from_ref(&old_backend)
    );
}

#[tokio::test]
async fn recovery_with_cleanup_done_returns_complete_without_rpc() {
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("node"),
        Grant::allow_all(),
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    register_capacity(&bus, &node).await;
    register_instance_participants(&bus, &node, Arc::clone(&events)).await;
    let manifest = coordinator_manifest();
    crate::DeployCoordinator::new(bus.clone(), operator.clone(), test_timeouts())
        .execute_until_serving_commit(manifest.clone())
        .await
        .expect("initial coordinator reaches serving commit");
    BusDeployFactWriter::new(bus.clone(), operator.clone())
        .write_cleanup_done(crate::DeployCleanupDoneFact::new(&manifest))
        .await
        .expect("write cleanup done");
    let source = BusFactFixtureSource::new(raw_bus);
    let restarted = crate::DeployCoordinator::new(bus, operator.clone(), test_timeouts());

    let recovery = restarted
        .recover_pending_cleanup(&source, operator.island(), &operator, &manifest.deploy_id)
        .expect("recover cleanup done");

    let DeployRecovery::CleanupDone(result) = recovery else {
        panic!("expected cleanup done recovery");
    };
    assert_eq!(result.outcome, DeployOutcome::DeployDone);
    assert_eq!(
        result.visible_nodes,
        VisibleNodes::new([NodeId::new("node-new")])
    );
}

#[tokio::test]
async fn recovered_cleanup_failure_returns_cleanup_pending_status() {
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let operator = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("node"),
        Grant::allow_all(),
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    register_capacity(&bus, &node).await;
    register_instance_participants(&bus, &node, Arc::clone(&events)).await;
    let manifest = coordinator_manifest();
    crate::DeployCoordinator::new(bus.clone(), operator.clone(), test_timeouts())
        .execute_until_serving_commit(manifest.clone())
        .await
        .expect("initial coordinator reaches serving commit");
    let source = BusFactFixtureSource::new(raw_bus);
    let restarted = crate::DeployCoordinator::new(bus, operator.clone(), test_timeouts());
    let DeployRecovery::Pending(recovered) = restarted
        .recover_pending_cleanup(&source, operator.island(), &operator, &manifest.deploy_id)
        .expect("recover pending cleanup")
    else {
        panic!("expected pending cleanup");
    };

    let result = restarted
        .finish_cleanup(
            (*recovered)
                .after_projection(projection_for_manifest(&manifest))
                .expect("projection proof accepted"),
        )
        .await
        .expect("cleanup pending is returned as result");

    assert_eq!(result.outcome, DeployOutcome::CleanupPending);
    assert_eq!(
        result.serving_commit_id,
        Some(manifest.serving_commit.serving_commit_id)
    );
}

#[test]
fn deploy_fact_keys_are_deploy_id_scoped() {
    let deploy_id = DeployId::new("deploy-1");

    assert_eq!(
        deploy_decision_fact_key(&deploy_id)
            .expect("decision key")
            .as_str(),
        "/facts/deploy/deploy-1/decision"
    );
    assert_eq!(
        deploy_cleanup_done_fact_key(&deploy_id)
            .expect("cleanup key")
            .as_str(),
        "/facts/deploy/deploy-1/cleanup/done"
    );
}

#[tokio::test]
async fn duplicate_deploy_decision_is_already_present() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let session = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("deploy"),
        Grant::allow_all(),
    );
    let writer = BusDeployFactWriter::new(bus, session);
    let fact = decision_fact("deploy-1", 1);

    let inserted = writer
        .write_decision(fact.clone())
        .await
        .expect("insert decision");
    let repeated = writer.write_decision(fact).await.expect("repeat decision");

    assert_eq!(inserted.status(), DeployFactWriteStatus::Inserted);
    assert_eq!(repeated.status(), DeployFactWriteStatus::AlreadyPresent);
    assert_eq!(inserted.content_hash(), repeated.content_hash());
}

#[tokio::test]
async fn conflicting_deploy_decision_returns_structured_conflict() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let session = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("deploy"),
        Grant::allow_all(),
    );
    let writer = BusDeployFactWriter::new(bus, session);
    writer
        .write_decision(decision_fact("deploy-1", 1))
        .await
        .expect("insert decision");

    let error = writer
        .write_decision(decision_fact("deploy-1", 2))
        .await
        .expect_err("conflicting decision should fail");

    assert!(matches!(
        error,
        DeployError::DeployFactConflict {
            key,
            principal,
            ..
        } if key.as_str() == "/facts/deploy/deploy-1/decision"
            && principal == PrincipalId::new("deploy")
    ));
}

#[test]
fn deploy_decision_selection_uses_epoch_desc_then_hash_asc() {
    let key = deploy_decision_fact_key(&DeployId::new("deploy-1")).expect("key");
    let low_epoch = DeployDecisionCandidate {
        fact: decision_fact("deploy-1", 1),
        author: PrincipalId::new("deploy-a"),
        content_hash: FactContentHash::new("b3:0002"),
    };
    let high_epoch_high_hash = DeployDecisionCandidate {
        fact: decision_fact("deploy-1", 2),
        author: PrincipalId::new("deploy-b"),
        content_hash: FactContentHash::new("b3:ffff"),
    };
    let high_epoch_low_hash = DeployDecisionCandidate {
        fact: decision_fact("deploy-1", 2),
        author: PrincipalId::new("deploy-c"),
        content_hash: FactContentHash::new("b3:0001"),
    };

    let selection = select_deploy_decision(
        vec![
            low_epoch,
            high_epoch_high_hash.clone(),
            high_epoch_low_hash.clone(),
        ],
        key,
    )
    .expect("select decision");

    assert_eq!(selection.winner, high_epoch_low_hash);
    assert_eq!(selection.superseded[0], high_epoch_high_hash);
}

#[test]
fn malformed_deploy_fact_payload_is_structured() {
    let key = deploy_decision_fact_key(&DeployId::new("deploy-1")).expect("key");
    let error = decode_deploy_decision_fact(&key, &b"{not-json".as_slice().into())
        .expect_err("malformed payload should fail");

    assert!(matches!(error, DeployError::WirePayload { .. }));
}

#[test]
fn wrong_deploy_fact_kind_is_structured() {
    let key = deploy_decision_fact_key(&DeployId::new("deploy-1")).expect("key");
    let cleanup = crate::deploy_cleanup_done_fact_payload(&crate::DeployCleanupDoneFact::new(
        &manifest("deploy-1", 1),
    ))
    .expect("cleanup payload");
    let error =
        decode_deploy_decision_fact(&key, &cleanup).expect_err("wrong fact kind should fail");

    assert!(matches!(
        error,
        DeployError::DeployFactKindMismatch {
            expected_kind: "decision",
            ..
        }
    ));
}

#[test]
fn deploy_decision_reader_selects_conflict_candidate_without_operator_choice() {
    let (raw_bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
    let source = BusFactFixtureSource::new(raw_bus.clone());
    let writer_a = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("deploy-a"),
        Grant::empty()
            .with_fact_write(FactKeyPattern::parse("/facts/deploy/>").expect("pattern"))
            .with_fact_read(FactKeyPattern::parse("/facts/deploy/>").expect("pattern")),
    );
    let writer_b = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("deploy-b"),
        Grant::empty()
            .with_fact_write(FactKeyPattern::parse("/facts/deploy/>").expect("pattern"))
            .with_fact_read(FactKeyPattern::parse("/facts/deploy/>").expect("pattern")),
    );
    let deploy_id = DeployId::new("deploy-1");
    let key = deploy_decision_fact_key(&deploy_id).expect("key");
    raw_bus
        .write_fact_payload(
            &writer_a,
            key.clone(),
            deploy_decision_fact_payload(&decision_fact("deploy-1", 1)).expect("payload"),
        )
        .expect("first write");
    let _ = raw_bus
        .write_fact_payload(
            &writer_b,
            key,
            deploy_decision_fact_payload(&decision_fact("deploy-1", 2)).expect("payload"),
        )
        .expect("second write reports conflict after storing candidate");

    let selection = read_deploy_decision(&source, writer_a.island(), &writer_a, &deploy_id)
        .expect("read conflicted decisions");

    assert_eq!(selection.winner.fact.serving_epoch, 2);
    assert_eq!(selection.superseded.len(), 1);
}

fn projection_for_manifest(manifest: &DeployManifest) -> ProjectionCatchUp {
    ProjectionCatchUp::from_report(
        &manifest.serving_commit,
        &projection_report_for_commit(
            &manifest.serving_commit,
            "gateway:gateway-commit-1:route-commit-1",
            "dns:dns-commit-1",
        ),
    )
    .expect("projection catch-up")
}

async fn register_capacity(bus: &BusActorHandle, session: &BusSession) {
    bus.subscribe(
        session,
        SubjectPattern::parse("node.node-new.capacity").expect("capacity subject"),
        move |ctx| {
            let _request: CapacityRequest = decode(ctx.message.payload(), "capacity request")
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?;
            ctx.reply(
                encode(
                    &crate::CapacityReply {
                        node_id: NodeId::new("node-new"),
                        memory_free_bytes: 1024,
                        can_run_database: false,
                    },
                    "capacity reply",
                )
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?,
            )
        },
    )
    .await
    .expect("register capacity");
}

async fn register_instance_participants(
    bus: &BusActorHandle,
    session: &BusSession,
    events: Arc<Mutex<Vec<CoordinatorEvent>>>,
) {
    let prepare_events = Arc::clone(&events);
    bus.subscribe(
        session,
        SubjectPattern::parse("node.node-new.rpc.prepare_instance").expect("prepare subject"),
        move |ctx| {
            let _request: InstanceCommandRequest =
                decode(ctx.message.payload(), "prepare instance request").map_err(|_| {
                    BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: HandlerFailure::Application,
                    }
                })?;
            prepare_events
                .lock()
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?
                .push(CoordinatorEvent::Prepare);
            ctx.reply(b"prepared".to_vec())
        },
    )
    .await
    .expect("register prepare");

    bus.subscribe(
        session,
        SubjectPattern::parse("node.node-new.rpc.start_instance").expect("start subject"),
        move |ctx| {
            let request: InstanceCommandRequest =
                decode(ctx.message.payload(), "start instance request").map_err(|_| {
                    BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: HandlerFailure::Application,
                    }
                })?;
            events
                .lock()
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?
                .push(CoordinatorEvent::Start);
            ctx.reply(
                encode(
                    &InstanceCommandReply {
                        instance_id: request.instance_id,
                        outcome: InstanceStartOutcome::Ready,
                        backend: None,
                    },
                    "start instance reply",
                )
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?,
            )
        },
    )
    .await
    .expect("register start");
}

async fn register_prepare_failure_participant(bus: &BusActorHandle, session: &BusSession) {
    bus.subscribe(
        session,
        SubjectPattern::parse("node.node-new.rpc.prepare_instance").expect("prepare subject"),
        move |ctx| {
            let _request: InstanceCommandRequest =
                decode(ctx.message.payload(), "prepare instance request").map_err(|_| {
                    BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: HandlerFailure::Application,
                    }
                })?;
            Err(BusError::HandlerFailed {
                subject: ctx.message.subject().to_string(),
                failure: HandlerFailure::Application,
            })
        },
    )
    .await
    .expect("register failing prepare");
}

async fn register_candidate_cleanup_participant(
    bus: &BusActorHandle,
    session: &BusSession,
    cleaned: Arc<Mutex<Vec<CleanupDeployCandidatesRequest>>>,
) {
    bus.subscribe(
        session,
        SubjectPattern::parse("node.node-new.rpc.cleanup_deploy_candidates")
            .expect("candidate cleanup subject"),
        move |ctx| {
            let request: CleanupDeployCandidatesRequest =
                decode(ctx.message.payload(), "candidate cleanup request").map_err(|_| {
                    BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: HandlerFailure::Application,
                    }
                })?;
            cleaned
                .lock()
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?
                .push(request);
            ctx.reply(b"cleaned".to_vec())
        },
    )
    .await
    .expect("register candidate cleanup");
}

async fn register_cleanup_participants(
    bus: &BusActorHandle,
    session: &BusSession,
    drained: Arc<Mutex<Vec<BackendEndpoint>>>,
    stopped: Arc<Mutex<Vec<BackendEndpoint>>>,
) {
    bus.subscribe(
        session,
        SubjectPattern::parse("node.node-old.rpc.drain_instance").expect("drain subject"),
        move |ctx| {
            let request: DrainInstanceRequest =
                decode(ctx.message.payload(), "drain instance request").map_err(|_| {
                    BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: HandlerFailure::Application,
                    }
                })?;
            drained
                .lock()
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?
                .push(request.cleanup_target);
            ctx.reply(b"drained".to_vec())
        },
    )
    .await
    .expect("register drain");

    bus.subscribe(
        session,
        SubjectPattern::parse("node.node-old.rpc.stop_instance").expect("stop subject"),
        move |ctx| {
            let request: StopInstanceRequest =
                decode(ctx.message.payload(), "stop instance request").map_err(|_| {
                    BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: HandlerFailure::Application,
                    }
                })?;
            stopped
                .lock()
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?
                .push(request.cleanup_target);
            ctx.reply(b"stopped".to_vec())
        },
    )
    .await
    .expect("register stop");
}

fn projection_report_for_commit(
    commit: &ServingCommitPlan,
    gateway_revision: &str,
    dns_revision: &str,
) -> ProjectionReport {
    let mut state = ProjectionState::for_island(mvp_bus::IslandId::new("prod"));
    let mut hostnames = commit.hostnames.clone();
    hostnames.sort();
    let mut backends = commit.active_backends.clone();
    backends.sort();
    let mut old_backends_to_drain = commit.old_backends_to_drain.clone();
    old_backends_to_drain.sort();
    let mut dns_records = commit
        .dns_records
        .clone()
        .into_iter()
        .map(DnsRecordProjection::from)
        .collect::<Vec<_>>();
    dns_records.sort();
    state.gateway = Some(GatewayProjection {
        gateway_commit_id: commit.gateway_commit_id.to_string(),
        route_commit_id: commit.route_commit_id.to_string(),
        routes: vec![GatewayRouteProjection {
            route_id: commit.route_id.clone(),
            hostnames,
            backends,
            old_backends_to_drain,
        }],
    });
    state.dns = Some(DnsProjection {
        dns_commit_id: commit.dns_commit_id.to_string(),
        records: dns_records,
    });
    ProjectionReport {
        state,
        sqlite_path: PathBuf::from("projections.sqlite"),
        gateway_snapshot: Some(SnapshotWriteReport {
            path: PathBuf::from("gateway.snapshot"),
            bytes_written: 1,
            revision: gateway_revision.to_string(),
        }),
        dns_snapshot: Some(SnapshotWriteReport {
            path: PathBuf::from("dns.snapshot"),
            bytes_written: 1,
            revision: dns_revision.to_string(),
        }),
        duration: Duration::from_millis(1),
    }
}

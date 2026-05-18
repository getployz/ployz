use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mvp_bus::{BusActorHandle, BusError, BusSession, Grant, HandlerFailure, IslandId, PrincipalId};
use mvp_deploy::{
    CandidateCleanupState, CandidateCleanupStatus, CapacityReply, CleanupDeployCandidatesRequest,
    CleanupFailureKind, DeployCoordinator, DeployError, DeployId, DeployManifest, DeployRecovery,
    DeployTimeouts, DnsCommitId, DrainInstanceRequest, GatewayCommitId,
    InstanceCapacityRequirement, InstanceCommandReply, InstanceCommandRequest, InstanceId,
    InstancePlan, InstanceStartOutcome, NodeCandidateCleanup, PhaseId, PhasePlan, PhasePolicy,
    PhaseReversibility, RevisionId, RouteCommitId, ServingCommitId, ServingCommitPlan,
    StopInstanceRequest,
};
use mvp_identity::NodeId;
use mvp_projection::{BackendEndpoint, BusFactSource, DnsRecordFact, RouteId, ServiceName};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};

#[derive(Debug, Serialize)]
struct DeployCandidateCleanupReport {
    scenario: &'static str,
    visible_nodes_at_decision: usize,
    prepared_candidates: usize,
    started_candidates: usize,
    candidate_cleanup_requests: usize,
    recovery_cleanup_requests: usize,
    cleanup_pending_count: usize,
    prepare_reruns_during_recovery: usize,
    start_reruns_during_recovery: usize,
    old_backend_drain_requests: usize,
    old_backend_stop_requests: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for deploy candidate cleanup: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("deploy-candidate-cleanup-contract");
    reset_dir(&root)?;

    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let prod = IslandId::new("prod");
    let operator = authority.grant_in(
        prod.clone(),
        PrincipalId::new("candidate-operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(prod, PrincipalId::new("candidate-node"), Grant::allow_all());
    let state = Arc::new(ParticipantState::default());
    register_capacity(&bus, &node, &state, "node-started").await?;
    register_capacity(&bus, &node, &state, "node-prepared").await?;
    register_successful_instance(&bus, &node, &state, "node-started").await?;
    register_prepare_only_instance(&bus, &node, &state, "node-prepared").await?;
    register_candidate_cleanup(&bus, &node, &state, "node-started").await?;
    register_candidate_cleanup(&bus, &node, &state, "node-prepared").await?;
    register_old_backend_cleanup_guards(&bus, &node, &state).await?;

    let manifest = candidate_cleanup_manifest("deploy-candidates", 23);
    let coordinator = DeployCoordinator::new(bus.clone(), operator.clone(), test_timeouts());
    let error = coordinator
        .execute_until_serving_commit(manifest.clone())
        .await
        .expect_err("pre-serving deploy failure should return cleanup report");
    let DeployError::PreCommitFailed { source, cleanup } = error else {
        return Err("expected pre-commit failure with candidate cleanup report".to_string());
    };
    if !matches!(*source, DeployError::Bus(BusError::NoResponders { .. })) {
        return Err(format!("unexpected deploy failure source: {source}"));
    }
    assert_eq_named(
        "candidate cleanup status",
        cleanup.status.clone(),
        CandidateCleanupStatus::Done,
    )?;
    assert_eq_named(
        "candidate cleanup visible nodes",
        cleanup.visible_nodes.len(),
        2,
    )?;
    assert_eq_named(
        "old backend drain before serving commit",
        state.old_drain_requests.load(Ordering::SeqCst),
        0,
    )?;
    assert_eq_named(
        "old backend stop before serving commit",
        state.old_stop_requests.load(Ordering::SeqCst),
        0,
    )?;
    let prepared_candidates =
        count_cleanup_state(&cleanup.attempted, CandidateCleanupState::Prepared);
    let started_candidates =
        count_cleanup_state(&cleanup.attempted, CandidateCleanupState::Started);
    assert_eq_named("prepared candidate cleanup count", prepared_candidates, 1)?;
    assert_eq_named("started candidate cleanup count", started_candidates, 1)?;
    assert_candidate_cleanup_payloads(
        "foreground candidate cleanup",
        &state.cleanup_requests_snapshot(),
        &manifest,
        &[
            (
                "node-started",
                "web-started",
                CandidateCleanupState::Started,
            ),
            (
                "node-prepared",
                "web-prepared",
                CandidateCleanupState::Prepared,
            ),
        ],
    )?;
    let foreground_cleanup_count = state.cleanup_request_count();

    let source = BusFactSource::new(raw_bus.clone());
    let recovery = coordinator
        .recover_pending_cleanup(&source, operator.island(), &operator, &manifest.deploy_id)
        .map_err(|error| format!("recover pre-commit candidate cleanup: {error}"))?;
    let DeployRecovery::PreCommitIncomplete(recovery) = recovery else {
        return Err("expected pre-commit incomplete recovery after failed deploy".to_string());
    };
    let prepare_before_recovery = state.prepare_requests.load(Ordering::SeqCst);
    let start_before_recovery = state.start_requests.load(Ordering::SeqCst);
    let recovery_cleanup = coordinator.cleanup_pre_commit_incomplete(&recovery).await;
    assert_eq_named(
        "recovery candidate cleanup status",
        recovery_cleanup.status.clone(),
        CandidateCleanupStatus::Done,
    )?;
    assert_eq_named(
        "recovery cleanup visible nodes",
        recovery_cleanup.visible_nodes.len(),
        2,
    )?;
    assert_eq_named(
        "recovery planned candidate cleanup count",
        count_cleanup_state(&recovery_cleanup.attempted, CandidateCleanupState::Planned),
        2,
    )?;
    assert_eq_named(
        "prepare reruns after recovery",
        state.prepare_requests.load(Ordering::SeqCst) - prepare_before_recovery,
        0,
    )?;
    assert_eq_named(
        "start reruns after recovery",
        state.start_requests.load(Ordering::SeqCst) - start_before_recovery,
        0,
    )?;
    let requests = state.cleanup_requests_snapshot();
    assert_candidate_cleanup_payloads(
        "recovery candidate cleanup",
        &requests[foreground_cleanup_count..],
        &manifest,
        &[
            (
                "node-started",
                "web-started",
                CandidateCleanupState::Planned,
            ),
            (
                "node-prepared",
                "web-prepared",
                CandidateCleanupState::Planned,
            ),
        ],
    )?;

    let cleanup_pending_count = cleanup_unavailable_variant().await?;
    let report = DeployCandidateCleanupReport {
        scenario: "deploy-candidate-cleanup-contract",
        visible_nodes_at_decision: cleanup.visible_nodes.len(),
        prepared_candidates,
        started_candidates,
        candidate_cleanup_requests: state.cleanup_request_count(),
        recovery_cleanup_requests: state.recovery_cleanup_requests(),
        cleanup_pending_count,
        prepare_reruns_during_recovery: state.prepare_requests.load(Ordering::SeqCst)
            - prepare_before_recovery,
        start_reruns_during_recovery: state.start_requests.load(Ordering::SeqCst)
            - start_before_recovery,
        old_backend_drain_requests: state.old_drain_requests.load(Ordering::SeqCst),
        old_backend_stop_requests: state.old_stop_requests.load(Ordering::SeqCst),
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(
        &root.join("deploy-candidate-cleanup-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS deploy-candidate-cleanup-contract");
    Ok(())
}

#[derive(Default)]
struct ParticipantState {
    prepare_requests: AtomicUsize,
    start_requests: AtomicUsize,
    old_drain_requests: AtomicUsize,
    old_stop_requests: AtomicUsize,
    cleanup_requests: Mutex<Vec<ObservedCandidateCleanup>>,
}

impl ParticipantState {
    fn cleanup_request_count(&self) -> usize {
        self.cleanup_requests
            .lock()
            .expect("cleanup requests")
            .len()
    }

    fn cleanup_requests_snapshot(&self) -> Vec<ObservedCandidateCleanup> {
        self.cleanup_requests
            .lock()
            .expect("cleanup requests")
            .clone()
    }

    fn recovery_cleanup_requests(&self) -> usize {
        self.cleanup_requests
            .lock()
            .expect("cleanup requests")
            .iter()
            .filter(|request| {
                request
                    .request
                    .candidates
                    .iter()
                    .all(|candidate| candidate.state == CandidateCleanupState::Planned)
            })
            .count()
    }
}

#[derive(Clone)]
struct ObservedCandidateCleanup {
    node_id: NodeId,
    request: CleanupDeployCandidatesRequest,
}

async fn cleanup_unavailable_variant() -> Result<usize, String> {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let prod = IslandId::new("prod");
    let operator = authority.grant_in(
        prod.clone(),
        PrincipalId::new("candidate-pending-operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        prod,
        PrincipalId::new("candidate-pending-node"),
        Grant::allow_all(),
    );
    let state = Arc::new(ParticipantState::default());
    register_capacity(&bus, &node, &state, "node-started").await?;
    register_capacity(&bus, &node, &state, "node-prepared").await?;
    register_successful_instance(&bus, &node, &state, "node-started").await?;
    register_prepare_only_instance(&bus, &node, &state, "node-prepared").await?;
    register_candidate_cleanup(&bus, &node, &state, "node-started").await?;

    let coordinator = DeployCoordinator::new(bus, operator, test_timeouts());
    let error = coordinator
        .execute_until_serving_commit(candidate_cleanup_manifest("deploy-candidates-pending", 24))
        .await
        .expect_err("missing cleanup responder should return pending cleanup");
    let DeployError::PreCommitFailed { cleanup, .. } = error else {
        return Err("expected pre-commit cleanup pending error".to_string());
    };
    let CandidateCleanupStatus::Pending { failures } = cleanup.status.clone() else {
        return Err("expected pending cleanup status".to_string());
    };
    let [failure] = failures.as_slice() else {
        return Err("expected one candidate cleanup failure".to_string());
    };
    assert_eq_named(
        "pending cleanup node",
        failure.node_id.clone(),
        NodeId::new("node-prepared"),
    )?;
    assert_eq_named(
        "pending cleanup cause",
        failure.cause.clone(),
        CleanupFailureKind::NoResponders,
    )?;
    assert_failure_targets_attempted_candidates(&failures, &cleanup.attempted)?;
    Ok(failures.len())
}

async fn register_capacity(
    bus: &BusActorHandle,
    session: &BusSession,
    _state: &Arc<ParticipantState>,
    node_id: &'static str,
) -> Result<(), String> {
    bus.subscribe(
        session,
        pattern(&format!("node.{node_id}.capacity"))?,
        move |ctx| {
            let _request: mvp_deploy::CapacityRequest =
                serde_json::from_slice(ctx.message.payload().as_bytes())
                    .map_err(|_| handler_failure(ctx.message.subject().to_string()))?;
            ctx.reply(
                serde_json::to_vec(&CapacityReply {
                    node_id: NodeId::new(node_id),
                    memory_free_bytes: 1024,
                    can_run_database: false,
                })
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?,
            )
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("register capacity {node_id}: {error}"))
}

async fn register_successful_instance(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
    node_id: &'static str,
) -> Result<(), String> {
    register_prepare(bus, session, state, node_id).await?;
    let state = Arc::clone(state);
    bus.subscribe(
        session,
        pattern(&format!("node.{node_id}.rpc.start_instance"))?,
        move |ctx| {
            let request: InstanceCommandRequest =
                serde_json::from_slice(ctx.message.payload().as_bytes())
                    .map_err(|_| handler_failure(ctx.message.subject().to_string()))?;
            state.start_requests.fetch_add(1, Ordering::SeqCst);
            ctx.reply(
                serde_json::to_vec(&InstanceCommandReply {
                    instance_id: request.instance_id,
                    outcome: InstanceStartOutcome::Ready,
                })
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?,
            )
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("register start {node_id}: {error}"))
}

async fn register_prepare_only_instance(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
    node_id: &'static str,
) -> Result<(), String> {
    register_prepare(bus, session, state, node_id).await
}

async fn register_prepare(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
    node_id: &'static str,
) -> Result<(), String> {
    let state = Arc::clone(state);
    bus.subscribe(
        session,
        pattern(&format!("node.{node_id}.rpc.prepare_instance"))?,
        move |ctx| {
            let _request: InstanceCommandRequest =
                serde_json::from_slice(ctx.message.payload().as_bytes())
                    .map_err(|_| handler_failure(ctx.message.subject().to_string()))?;
            state.prepare_requests.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"prepared".to_vec())
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("register prepare {node_id}: {error}"))
}

async fn register_candidate_cleanup(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
    node_id: &'static str,
) -> Result<(), String> {
    let state = Arc::clone(state);
    bus.subscribe(
        session,
        pattern(&format!("node.{node_id}.rpc.cleanup_deploy_candidates"))?,
        move |ctx| {
            let request: CleanupDeployCandidatesRequest =
                serde_json::from_slice(ctx.message.payload().as_bytes())
                    .map_err(|_| handler_failure(ctx.message.subject().to_string()))?;
            state
                .cleanup_requests
                .lock()
                .map_err(|_| BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: HandlerFailure::Application,
                })?
                .push(ObservedCandidateCleanup {
                    node_id: NodeId::new(node_id),
                    request,
                });
            ctx.reply(b"cleaned".to_vec())
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("register candidate cleanup {node_id}: {error}"))
}

async fn register_old_backend_cleanup_guards(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
) -> Result<(), String> {
    let drain_state = Arc::clone(state);
    bus.subscribe(
        session,
        pattern("node.node-old.rpc.drain_instance")?,
        move |ctx| {
            let _request: DrainInstanceRequest =
                serde_json::from_slice(ctx.message.payload().as_bytes())
                    .map_err(|_| handler_failure(ctx.message.subject().to_string()))?;
            drain_state
                .old_drain_requests
                .fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"drained".to_vec())
        },
    )
    .await
    .map_err(|error| format!("register old drain: {error}"))?;

    let stop_state = Arc::clone(state);
    bus.subscribe(
        session,
        pattern("node.node-old.rpc.stop_instance")?,
        move |ctx| {
            let _request: StopInstanceRequest =
                serde_json::from_slice(ctx.message.payload().as_bytes())
                    .map_err(|_| handler_failure(ctx.message.subject().to_string()))?;
            stop_state.old_stop_requests.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"stopped".to_vec())
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("register old stop: {error}"))
}

fn handler_failure(subject: impl Into<String>) -> BusError {
    BusError::HandlerFailed {
        subject: subject.into(),
        failure: HandlerFailure::Application,
    }
}

fn assert_candidate_cleanup_payloads(
    label: &'static str,
    requests: &[ObservedCandidateCleanup],
    manifest: &DeployManifest,
    expected: &[(&'static str, &'static str, CandidateCleanupState)],
) -> Result<(), String> {
    assert_eq_named(
        &format!("{label} request count"),
        requests.len(),
        expected.len(),
    )?;
    for (node_id, instance_id, state) in expected {
        let observed = requests
            .iter()
            .find(|observed| {
                observed.request.candidates.iter().any(|candidate| {
                    candidate.instance_id == InstanceId::new(*instance_id)
                        && candidate.state == *state
                })
            })
            .ok_or_else(|| format!("{label} missing cleanup request for {instance_id}"))?;
        let request = &observed.request;
        assert_eq_named(
            &format!("{label} deploy id for {instance_id}"),
            request.deploy_id.clone(),
            manifest.deploy_id.clone(),
        )?;
        assert_eq_named(
            &format!("{label} candidate count for {instance_id}"),
            request.candidates.len(),
            1,
        )?;
        let [candidate] = request.candidates.as_slice() else {
            return Err(format!("{label} expected one candidate for {instance_id}"));
        };
        let planned = find_instance(manifest, instance_id)
            .ok_or_else(|| format!("{label} instance {instance_id} not in manifest"))?;
        assert_eq_named(
            &format!("{label} subject node for {instance_id}"),
            observed.node_id.clone(),
            NodeId::new(*node_id),
        )?;
        assert_eq_named(
            &format!("{label} planned node for {instance_id}"),
            planned.node_id.clone(),
            NodeId::new(*node_id),
        )?;
        assert_eq_named(
            &format!("{label} service for {instance_id}"),
            candidate.service.clone(),
            planned.service.clone(),
        )?;
        assert_eq_named(
            &format!("{label} revision for {instance_id}"),
            candidate.revision.clone(),
            planned.revision.clone(),
        )?;
    }
    Ok(())
}

fn find_instance<'a>(manifest: &'a DeployManifest, instance_id: &str) -> Option<&'a InstancePlan> {
    manifest
        .phases
        .iter()
        .flat_map(|phase| &phase.instances)
        .find(|instance| instance.instance_id == InstanceId::new(instance_id))
}

fn assert_failure_targets_attempted_candidates(
    failures: &[mvp_deploy::CandidateCleanupFailure],
    attempted: &[NodeCandidateCleanup],
) -> Result<(), String> {
    for failure in failures {
        let attempted_node = attempted
            .iter()
            .find(|node| node.node_id == failure.node_id)
            .ok_or_else(|| {
                format!(
                    "pending cleanup failure for {} had no attempted candidate node",
                    failure.node_id
                )
            })?;
        assert_eq_named(
            "pending cleanup failure candidates",
            failure.candidates.clone(),
            attempted_node.candidates.clone(),
        )?;
    }
    Ok(())
}

fn count_cleanup_state(
    attempted: &[mvp_deploy::NodeCandidateCleanup],
    state: CandidateCleanupState,
) -> usize {
    attempted
        .iter()
        .flat_map(|node| &node.candidates)
        .filter(|candidate| candidate.state == state)
        .count()
}

fn candidate_cleanup_manifest(deploy_id: &str, epoch: u64) -> DeployManifest {
    DeployManifest::new(
        DeployId::new(deploy_id),
        vec![
            PhasePlan::new(
                PhaseId::new(1),
                vec![InstancePlan::new(
                    InstanceId::new("web-started"),
                    NodeId::new("node-started"),
                    ServiceName::new("web"),
                    RevisionId::new("rev-started"),
                    InstanceCapacityRequirement::General,
                )],
                PhasePolicy::reversible(),
            ),
            PhasePlan::new(
                PhaseId::new(2),
                vec![InstancePlan::new(
                    InstanceId::new("web-prepared"),
                    NodeId::new("node-prepared"),
                    ServiceName::new("web"),
                    RevisionId::new("rev-prepared"),
                    InstanceCapacityRequirement::General,
                )],
                PhasePolicy::serving(PhaseReversibility::Reversible),
            ),
        ],
        ServingCommitPlan {
            serving_commit_id: ServingCommitId::new(format!("{deploy_id}-serving")),
            route_commit_id: RouteCommitId::new(format!("{deploy_id}-route")),
            gateway_commit_id: GatewayCommitId::new(format!("{deploy_id}-gateway")),
            dns_commit_id: DnsCommitId::new(format!("{deploy_id}-dns")),
            route_id: RouteId::new("web"),
            hostnames: vec!["candidate.example.test".to_string()],
            active_backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-prepared"),
                address: "fd00::33:8080".to_string(),
            }],
            old_backends_to_drain: vec![BackendEndpoint {
                node_id: NodeId::new("node-old"),
                address: "fd00::30:8080".to_string(),
            }],
            dns_records: vec![DnsRecordFact {
                name: "candidate.example.test".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::33".to_string(),
                ttl_seconds: 30,
            }],
            epoch,
        },
    )
}

fn test_timeouts() -> DeployTimeouts {
    DeployTimeouts {
        capacity: Duration::from_millis(50),
        participant: Duration::from_millis(50),
    }
}

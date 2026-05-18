use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mvp_bus::{
    BusActorHandle, BusError, BusSession, FactContentHash, FactKeyPattern, FactPayload, Grant,
    HandlerFailure, IslandId, PrincipalId,
};
use mvp_deploy::{
    CapacityReply, CleanupFailureKind, CleanupPendingReason, CleanupStatus, DeployCleanupDoneFact,
    DeployCoordinator, DeployDecisionFact, DeployError, DeployFactWriter, DeployId, DeployManifest,
    DeployOutcome, DeployRecovery, DeployResult, DeployTimeouts, DnsCommitId, DrainInstanceRequest,
    DrainStatus, GatewayCommitId, InstanceCapacityRequirement, InstanceCommandReply,
    InstanceCommandRequest, InstanceId, InstancePlan, InstanceStartOutcome, PhaseId, PhasePlan,
    PhasePolicy, PhaseReversibility, ProjectionCatchUp, RevisionId, RouteCommitId, ServingCommitId,
    ServingCommitPlan, ServingFactWriter, StopInstanceRequest, WrittenDeployFact,
    WrittenServingFact, deploy_cleanup_done_fact_key, deploy_cleanup_done_fact_payload,
    deploy_decision_fact_key, deploy_decision_fact_payload,
};
use mvp_identity::NodeId;
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactOperation, PandaFactStore, PandaFactWriteOutcome,
};
use mvp_projection::{
    BackendEndpoint, DnsRecordFact, FactCandidate, FactSource, FactSourceError, FactSourceResult,
    RouteId, ServiceName, load_dns_snapshot, load_gateway_snapshot,
};
use mvp_routing::{serving_commit_fact_key, serving_commit_fact_payload};
use mvp_serving::{ServingActorHandle, ServingSnapshotPaths};
use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
struct DeployRestartRecoveryReport {
    scenario: &'static str,
    visible_nodes_at_decision: usize,
    decision_fact_write_ms: u64,
    serving_fact_write_ms: u64,
    cleanup_done_fact_write_ms: u64,
    serving_commit_to_simulated_kill_ms: u128,
    coordinator_outage_ms: u128,
    recovery_read_ms: u128,
    projection_catch_up_ms: u128,
    resumed_drain_ms: u128,
    resumed_stop_ms: u128,
    data_plane_requests_served_during_outage: usize,
    capacity_requests: usize,
    prepare_requests: usize,
    start_requests: usize,
    drain_requests: usize,
    stop_requests: usize,
    cleanup_pending_after_restart: bool,
    cleanup_done_recovered: bool,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for deploy restart recovery: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("deploy-restart-recovery-contract");
    reset_dir(&root)?;

    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let prod = IslandId::new("prod");
    let operator = authority.grant_in(
        prod.clone(),
        PrincipalId::new("restart-operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        prod.clone(),
        PrincipalId::new("restart-node"),
        Grant::allow_all(),
    );
    let projection = authority.grant_in(
        prod.clone(),
        PrincipalId::new("restart-projection"),
        Grant::allow_all(),
    );

    let facts = SharedPandaFacts::new(PandaFactStore::new(Arc::new(raw_bus.clone())));
    let operator_author = Arc::new(PandaFactAuthor::new(operator.principal().clone()));
    let timings = Arc::new(FactWriteTimings::default());
    let participant_state = Arc::new(ParticipantState::default());
    register_capacity(&bus, &node, &participant_state, "node-db", true).await?;
    register_capacity(&bus, &node, &participant_state, "node-web", false).await?;
    register_capacity(&bus, &node, &participant_state, "node-queue", false).await?;
    register_instance_participants(&bus, &node, &participant_state).await?;
    register_cleanup_participants(&bus, &node, &participant_state).await?;

    let manifest = deploy_manifest(
        "deploy-restart",
        "route-commit-restart",
        "gateway-restart",
        "dns-restart",
        18,
    );
    let coordinator = coordinator_with_panda_facts(
        bus.clone(),
        operator.clone(),
        facts.clone(),
        Arc::clone(&operator_author),
        Arc::clone(&timings),
    );

    let _pending = coordinator
        .execute_until_serving_commit(manifest.clone())
        .await
        .map_err(|error| format!("execute deploy until serving commit: {error}"))?;
    let serving_commit_returned_at = Instant::now();
    assert_eq_named(
        "drain before projection",
        participant_state.drain_requests.load(Ordering::SeqCst),
        0,
    )?;
    assert_eq_named(
        "stop before projection",
        participant_state.stop_requests.load(Ordering::SeqCst),
        0,
    )?;

    drop(coordinator);
    let serving_commit_to_simulated_kill_ms = serving_commit_returned_at.elapsed().as_millis();

    let projection_started = Instant::now();
    let actor = projection_actor(Arc::new(facts.clone()), projection.clone(), &root)?;
    let projected = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project p2panda deploy facts: {error}"))?;
    let projection_catch_up_ms = projection_started.elapsed().as_millis();
    let proof = ProjectionCatchUp::from_report(&manifest.serving_commit, &projected)
        .map_err(|error| format!("build projection catch-up proof: {error}"))?;
    assert_projected_serving_state(&root, &manifest, &projected)?;

    let serving = ServingActorHandle::spawn(
        prod.clone(),
        ServingSnapshotPaths::new(root.join("gateway.snapshot"), root.join("dns.snapshot")),
        Duration::from_secs(60),
    )
    .map_err(|error| format!("spawn serving actor: {error}"))?;

    let outage_started = Instant::now();
    let data_plane_requests_served_during_outage =
        assert_serving_answers_without_coordinator(&serving).await?;
    let coordinator_outage_ms = outage_started.elapsed().as_millis();
    let exported_recovery_operations = facts.export_operations().await;
    let recovered_facts = import_shared_panda_facts(
        raw_bus.clone(),
        &prod,
        &operator,
        operator_author.as_ref(),
        &exported_recovery_operations,
    )
    .await?;

    let recovery_coordinator = coordinator_with_panda_facts(
        bus.clone(),
        operator.clone(),
        recovered_facts.clone(),
        Arc::clone(&operator_author),
        Arc::clone(&timings),
    );
    let recovery_read_started = Instant::now();
    let recovered = match recovery_coordinator
        .recover_pending_cleanup(&recovered_facts, &prod, &projection, &manifest.deploy_id)
        .map_err(|error| format!("recover pending cleanup: {error}"))?
    {
        DeployRecovery::Pending(recovered) => recovered,
        other => return Err(format!("expected pending cleanup recovery, got {other:?}")),
    };
    let recovery_read_ms = recovery_read_started.elapsed().as_millis();
    assert_eq_named(
        "recovered manifest deploy id",
        recovered.manifest().deploy_id.clone(),
        manifest.deploy_id.clone(),
    )?;
    assert_eq_named(
        "superseded decisions after recovery",
        recovered.superseded_decisions().len(),
        0,
    )?;
    assert_no_precommit_rerun(&participant_state)?;
    assert_eq_named(
        "recovered drain before proof use",
        participant_state.drain_requests.load(Ordering::SeqCst),
        0,
    )?;
    assert_eq_named(
        "recovered stop before proof use",
        participant_state.stop_requests.load(Ordering::SeqCst),
        0,
    )?;

    let projected_pending = recovered
        .after_projection(proof.clone())
        .map_err(|error| format!("accept recovered projection proof: {error}"))?;
    let resumed_drain_started = Instant::now();
    let cleanup_pending = recovery_coordinator
        .finish_cleanup(projected_pending)
        .await
        .map_err(|error| format!("finish recovered cleanup to pending: {error}"))?;
    let resumed_drain_ms = resumed_drain_started.elapsed().as_millis();
    assert_eq_named(
        "cleanup pending outcome after restart",
        cleanup_pending.outcome.clone(),
        DeployOutcome::CleanupPending,
    )?;
    assert_eq_named(
        "cleanup pending serving commit",
        cleanup_pending
            .serving_commit_id
            .as_ref()
            .map(ToString::to_string),
        Some("route-commit-restart".to_string()),
    )?;
    assert_eq_named(
        "cleanup pending status after restart",
        cleanup_pending.cleanup_status.clone(),
        CleanupStatus::Pending {
            reason: CleanupPendingReason::StopUnavailable {
                node_id: NodeId::new("node-old"),
                cause: CleanupFailureKind::NoResponders,
            },
        },
    )?;

    register_stop_participant(&bus, &node, &participant_state).await?;
    let final_coordinator = coordinator_with_panda_facts(
        bus,
        operator,
        recovered_facts.clone(),
        Arc::clone(&operator_author),
        Arc::clone(&timings),
    );
    let recovered = match final_coordinator
        .recover_pending_cleanup(&recovered_facts, &prod, &projection, &manifest.deploy_id)
        .map_err(|error| format!("recover pending cleanup after stop registers: {error}"))?
    {
        DeployRecovery::Pending(recovered) => recovered,
        other => {
            return Err(format!(
                "expected pending cleanup before final stop, got {other:?}"
            ));
        }
    };
    let final_pending = recovered
        .after_projection(proof)
        .map_err(|error| format!("accept final projection proof: {error}"))?;
    let resumed_stop_started = Instant::now();
    let result = final_coordinator
        .finish_cleanup(final_pending)
        .await
        .map_err(|error| format!("finish recovered cleanup: {error}"))?;
    let resumed_stop_ms = resumed_stop_started.elapsed().as_millis();
    assert_eq_named(
        "final deploy outcome",
        result.outcome,
        DeployOutcome::DeployDone,
    )?;
    assert_eq_named(
        "final drain status",
        result.drain_status,
        DrainStatus::Completed,
    )?;
    assert_visible_nodes(&result.visible_nodes)?;
    assert_no_precommit_rerun(&participant_state)?;
    participant_state.assert_cleanup_targets(&manifest.serving_commit.old_backends_to_drain)?;

    let cleanup_done_recovered = matches!(
        final_coordinator
            .recover_pending_cleanup(&recovered_facts, &prod, &projection, &manifest.deploy_id)
            .map_err(|error| format!("recover cleanup done fact: {error}"))?,
        DeployRecovery::CleanupDone(_)
    );
    if !cleanup_done_recovered {
        return Err("cleanup-done fact did not make recovery idempotent".to_string());
    }

    let report = DeployRestartRecoveryReport {
        scenario: "deploy-restart-recovery-contract",
        visible_nodes_at_decision: result.visible_nodes.len(),
        decision_fact_write_ms: timings.decision_ms.load(Ordering::SeqCst),
        serving_fact_write_ms: timings.serving_ms.load(Ordering::SeqCst),
        cleanup_done_fact_write_ms: timings.cleanup_done_ms.load(Ordering::SeqCst),
        serving_commit_to_simulated_kill_ms,
        coordinator_outage_ms,
        recovery_read_ms,
        projection_catch_up_ms,
        resumed_drain_ms,
        resumed_stop_ms,
        data_plane_requests_served_during_outage,
        capacity_requests: participant_state.capacity_requests.load(Ordering::SeqCst),
        prepare_requests: participant_state.prepare_requests.load(Ordering::SeqCst),
        start_requests: participant_state.start_requests.load(Ordering::SeqCst),
        drain_requests: participant_state.drain_requests.load(Ordering::SeqCst),
        stop_requests: participant_state.stop_requests.load(Ordering::SeqCst),
        cleanup_pending_after_restart: cleanup_pending.outcome == DeployOutcome::CleanupPending,
        cleanup_done_recovered,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(
        &root.join("deploy-restart-recovery-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS deploy-restart-recovery-contract");
    Ok(())
}

#[derive(Clone)]
struct SharedPandaFacts {
    store: Arc<AsyncMutex<PandaFactStore>>,
}

impl SharedPandaFacts {
    fn new(store: PandaFactStore) -> Self {
        Self {
            store: Arc::new(AsyncMutex::new(store)),
        }
    }

    async fn write(
        &self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: mvp_bus::FactKey,
        payload: FactPayload,
    ) -> Result<PandaFactWriteOutcome, DeployError> {
        self.store
            .lock()
            .await
            .write_fact_payload(session, author, key, payload)
            .await
            .map_err(|error| DeployError::FactSource(error.into()))
    }

    async fn write_timed(
        &self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: mvp_bus::FactKey,
        payload: FactPayload,
        elapsed_ms: &AtomicU64,
    ) -> Result<PandaFactWriteOutcome, DeployError> {
        let started = Instant::now();
        let outcome = self.write(session, author, key, payload).await?;
        elapsed_ms.store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
        Ok(outcome)
    }

    async fn export_operations(&self) -> Vec<PandaFactOperation> {
        self.store
            .lock()
            .await
            .export_operations()
            .cloned()
            .collect()
    }

    fn unavailable() -> FactSourceError {
        FactSourceError::Unavailable {
            name: "p2panda fact store is locked by a writer".to_string(),
        }
    }
}

async fn import_shared_panda_facts(
    authorizer: mvp_bus::harness::InMemoryBus,
    island: &IslandId,
    session: &BusSession,
    author: &PandaFactAuthor,
    operations: &[PandaFactOperation],
) -> Result<SharedPandaFacts, String> {
    let mut imported = PandaFactStore::new(Arc::new(authorizer));
    imported
        .trust_author_key(island, author.principal().clone(), author.author_key())
        .map_err(|error| format!("trust p2panda author key for restart recovery: {error}"))?;
    for operation in operations {
        imported
            .import_operation(session, operation)
            .await
            .map_err(|error| format!("import p2panda restart recovery operation: {error}"))?;
    }
    Ok(SharedPandaFacts::new(imported))
}

impl FactSource for SharedPandaFacts {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        self.store
            .try_lock()
            .map_err(|_| Self::unavailable())?
            .list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        self.store
            .try_lock()
            .map_err(|_| Self::unavailable())?
            .read_payloads(island, candidates, session)
    }
}

#[derive(Default)]
struct FactWriteTimings {
    decision_ms: AtomicU64,
    serving_ms: AtomicU64,
    cleanup_done_ms: AtomicU64,
}

struct PandaDeployFactWriter {
    facts: SharedPandaFacts,
    session: BusSession,
    author: Arc<PandaFactAuthor>,
    timings: Arc<FactWriteTimings>,
}

impl DeployFactWriter for PandaDeployFactWriter {
    fn write_decision<'a>(
        &'a self,
        fact: DeployDecisionFact,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenDeployFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = deploy_decision_fact_key(&fact.deploy_id)?;
            let payload = deploy_decision_fact_payload(&fact)?;
            let outcome = self
                .facts
                .write_timed(
                    &self.session,
                    self.author.as_ref(),
                    key.clone(),
                    payload,
                    &self.timings.decision_ms,
                )
                .await?;
            deploy_fact_outcome(key, outcome)
        })
    }

    fn write_cleanup_done<'a>(
        &'a self,
        fact: DeployCleanupDoneFact,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenDeployFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = deploy_cleanup_done_fact_key(&fact.deploy_id)?;
            let payload = deploy_cleanup_done_fact_payload(&fact)?;
            let outcome = self
                .facts
                .write_timed(
                    &self.session,
                    self.author.as_ref(),
                    key.clone(),
                    payload,
                    &self.timings.cleanup_done_ms,
                )
                .await?;
            deploy_fact_outcome(key, outcome)
        })
    }
}

struct PandaServingFactWriter {
    facts: SharedPandaFacts,
    session: BusSession,
    author: Arc<PandaFactAuthor>,
    timings: Arc<FactWriteTimings>,
}

impl ServingFactWriter for PandaServingFactWriter {
    fn write_serving_commit<'a>(
        &'a self,
        commit: &'a ServingCommitPlan,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenServingFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = serving_commit_fact_key(&commit.serving_commit_id)?;
            let payload = serving_commit_fact_payload(commit)?;
            let outcome = self
                .facts
                .write_timed(
                    &self.session,
                    self.author.as_ref(),
                    key.clone(),
                    payload,
                    &self.timings.serving_ms,
                )
                .await?;
            serving_fact_outcome(key, outcome)
        })
    }
}

fn coordinator_with_panda_facts(
    bus: BusActorHandle,
    session: BusSession,
    facts: SharedPandaFacts,
    author: Arc<PandaFactAuthor>,
    timings: Arc<FactWriteTimings>,
) -> DeployCoordinator<PandaDeployFactWriter, PandaServingFactWriter> {
    DeployCoordinator::with_fact_writers(
        bus,
        session.clone(),
        PandaDeployFactWriter {
            facts: facts.clone(),
            author: Arc::clone(&author),
            session: session.clone(),
            timings: Arc::clone(&timings),
        },
        PandaServingFactWriter {
            facts,
            author,
            session,
            timings,
        },
        test_timeouts(),
    )
}

fn deploy_fact_outcome(
    key: mvp_bus::FactKey,
    outcome: PandaFactWriteOutcome,
) -> DeployResult<WrittenDeployFact> {
    match outcome {
        PandaFactWriteOutcome::Inserted(metadata) => Ok(WrittenDeployFact::inserted(
            key,
            metadata.content_hash().clone(),
        )),
        PandaFactWriteOutcome::AlreadyPresent(metadata) => Ok(WrittenDeployFact::already_present(
            key,
            metadata.content_hash().clone(),
        )),
        PandaFactWriteOutcome::Conflict(metadata) => Err(DeployError::DeployFactConflict {
            key,
            principal: metadata.author().clone(),
            content_hash: metadata.content_hash().clone(),
        }),
    }
}

fn serving_fact_outcome(
    key: mvp_bus::FactKey,
    outcome: PandaFactWriteOutcome,
) -> DeployResult<WrittenServingFact> {
    match outcome {
        PandaFactWriteOutcome::Inserted(metadata) => Ok(WrittenServingFact::inserted(
            key,
            metadata.content_hash().clone(),
        )),
        PandaFactWriteOutcome::AlreadyPresent(metadata) => Ok(WrittenServingFact::already_present(
            key,
            metadata.content_hash().clone(),
        )),
        PandaFactWriteOutcome::Conflict(_) => Err(DeployError::ServingFactConflict { key }),
    }
}

#[derive(Default)]
struct ParticipantState {
    capacity_requests: AtomicUsize,
    prepare_requests: AtomicUsize,
    start_requests: AtomicUsize,
    drain_requests: AtomicUsize,
    stop_requests: AtomicUsize,
    drain_targets: Mutex<Vec<BackendEndpoint>>,
    stop_targets: Mutex<Vec<BackendEndpoint>>,
}

impl ParticipantState {
    fn push_drain_target(&self, target: BackendEndpoint) -> Result<(), BusError> {
        self.drain_targets
            .lock()
            .map_err(|_| handler_failure("drain target log"))?
            .push(target);
        Ok(())
    }

    fn push_stop_target(&self, target: BackendEndpoint) -> Result<(), BusError> {
        self.stop_targets
            .lock()
            .map_err(|_| handler_failure("stop target log"))?
            .push(target);
        Ok(())
    }

    fn assert_cleanup_targets(&self, expected: &[BackendEndpoint]) -> Result<(), String> {
        let [expected_target] = expected else {
            return Err(format!("expected one cleanup target, got {expected:?}"));
        };
        let drain_targets = self
            .drain_targets
            .lock()
            .map_err(|_| "drain target log mutex poisoned".to_string())?
            .clone();
        let stop_targets = self
            .stop_targets
            .lock()
            .map_err(|_| "stop target log mutex poisoned".to_string())?
            .clone();
        if drain_targets != vec![expected_target.clone(), expected_target.clone()] {
            return Err(format!("unexpected drain targets: {drain_targets:?}"));
        }
        if stop_targets != vec![expected_target.clone()] {
            return Err(format!("unexpected stop targets: {stop_targets:?}"));
        }
        Ok(())
    }
}

async fn register_capacity(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
    node_id: &'static str,
    can_run_database: bool,
) -> Result<(), String> {
    let state_for_capacity = Arc::clone(state);
    bus.subscribe(
        session,
        pattern(&format!("node.{node_id}.capacity"))?,
        move |ctx| {
            state_for_capacity
                .capacity_requests
                .fetch_add(1, Ordering::SeqCst);
            let reply = CapacityReply {
                node_id: NodeId::new(node_id),
                memory_free_bytes: 1024 * 1024 * 1024,
                can_run_database,
            };
            ctx.reply(
                serde_json::to_vec(&reply)
                    .map_err(|_| handler_failure(ctx.message.subject().to_string()))?,
            )
        },
    )
    .await
    .map_err(|error| format!("register capacity {node_id}: {error}"))?;
    Ok(())
}

async fn register_instance_participants(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
) -> Result<(), String> {
    for node_id in ["node-db", "node-web", "node-queue"] {
        let state_for_prepare = Arc::clone(state);
        bus.subscribe(
            session,
            pattern(&format!("node.{node_id}.rpc.prepare_instance"))?,
            move |ctx| {
                state_for_prepare
                    .prepare_requests
                    .fetch_add(1, Ordering::SeqCst);
                ctx.reply(b"prepared".to_vec())
            },
        )
        .await
        .map_err(|error| format!("register prepare {node_id}: {error}"))?;

        let state_for_start = Arc::clone(state);
        bus.subscribe(
            session,
            pattern(&format!("node.{node_id}.rpc.start_instance"))?,
            move |ctx| {
                state_for_start
                    .start_requests
                    .fetch_add(1, Ordering::SeqCst);
                let request: InstanceCommandRequest =
                    serde_json::from_slice(ctx.message.payload().as_bytes())
                        .map_err(|_| handler_failure(ctx.message.subject().to_string()))?;
                let reply = InstanceCommandReply {
                    instance_id: request.instance_id,
                    outcome: InstanceStartOutcome::Ready,
                };
                ctx.reply(
                    serde_json::to_vec(&reply)
                        .map_err(|_| handler_failure(ctx.message.subject().to_string()))?,
                )
            },
        )
        .await
        .map_err(|error| format!("register start {node_id}: {error}"))?;
    }
    Ok(())
}

async fn register_cleanup_participants(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
) -> Result<(), String> {
    let state_for_drain = Arc::clone(state);
    bus.subscribe(
        session,
        pattern("node.node-old.rpc.drain_instance")?,
        move |ctx| {
            state_for_drain
                .drain_requests
                .fetch_add(1, Ordering::SeqCst);
            let request: DrainInstanceRequest =
                serde_json::from_slice(ctx.message.payload().as_bytes())
                    .map_err(|_| handler_failure(ctx.message.subject().to_string()))?;
            state_for_drain.push_drain_target(request.cleanup_target)?;
            ctx.reply(b"draining".to_vec())
        },
    )
    .await
    .map_err(|error| format!("register old drain: {error}"))?;
    Ok(())
}

async fn register_stop_participant(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
) -> Result<(), String> {
    let state_for_stop = Arc::clone(state);
    bus.subscribe(
        session,
        pattern("node.node-old.rpc.stop_instance")?,
        move |ctx| {
            state_for_stop.stop_requests.fetch_add(1, Ordering::SeqCst);
            let request: StopInstanceRequest =
                serde_json::from_slice(ctx.message.payload().as_bytes())
                    .map_err(|_| handler_failure(ctx.message.subject().to_string()))?;
            state_for_stop.push_stop_target(request.cleanup_target)?;
            ctx.reply(b"stopped".to_vec())
        },
    )
    .await
    .map_err(|error| format!("register old stop: {error}"))?;
    Ok(())
}

fn deploy_manifest(
    deploy_id: &str,
    route_commit_id: &str,
    gateway_commit_id: &str,
    dns_commit_id: &str,
    epoch: u64,
) -> DeployManifest {
    DeployManifest::new(
        DeployId::new(deploy_id),
        vec![
            PhasePlan::new(
                PhaseId::new(1),
                vec![InstancePlan::new(
                    InstanceId::new("db-1"),
                    NodeId::new("node-db"),
                    ServiceName::new("db"),
                    RevisionId::new("rev-db"),
                    InstanceCapacityRequirement::Database,
                )],
                PhasePolicy::irreversible(),
            ),
            PhasePlan::new(
                PhaseId::new(2),
                vec![
                    InstancePlan::new(
                        InstanceId::new("web-1"),
                        NodeId::new("node-web"),
                        ServiceName::new("web"),
                        RevisionId::new("rev-web"),
                        InstanceCapacityRequirement::General,
                    ),
                    InstancePlan::new(
                        InstanceId::new("queue-1"),
                        NodeId::new("node-queue"),
                        ServiceName::new("queue"),
                        RevisionId::new("rev-queue"),
                        InstanceCapacityRequirement::General,
                    ),
                ],
                PhasePolicy::serving(PhaseReversibility::Reversible),
            ),
        ],
        ServingCommitPlan {
            serving_commit_id: ServingCommitId::new(route_commit_id),
            route_commit_id: RouteCommitId::new(route_commit_id),
            gateway_commit_id: GatewayCommitId::new(gateway_commit_id),
            dns_commit_id: DnsCommitId::new(dns_commit_id),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["web.example.test".to_string()],
            active_backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-web"),
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
            epoch,
        },
    )
}

fn assert_projected_serving_state(
    root: &std::path::Path,
    manifest: &DeployManifest,
    projected: &mvp_projection::ProjectionReport,
) -> Result<(), String> {
    let gateway = projected
        .state
        .gateway
        .as_ref()
        .ok_or_else(|| "gateway projection missing after restart serving commit".to_string())?;
    let [route] = gateway.routes.as_slice() else {
        return Err("gateway projection should contain exactly one route".to_string());
    };
    assert_eq_named("active backend count", route.backends.len(), 1)?;
    assert_eq_named(
        "old backend drain metadata count",
        route.old_backends_to_drain.len(),
        1,
    )?;
    let dns = projected
        .state
        .dns
        .as_ref()
        .ok_or_else(|| "dns projection missing after restart serving commit".to_string())?;
    let [dns_record] = dns.records.as_slice() else {
        return Err("dns projection should contain exactly one record".to_string());
    };
    assert_eq_named("projected dns value", dns_record.value.as_str(), "fd00::2")?;
    assert_eq_named(
        "loaded gateway snapshot routes",
        load_gateway_snapshot(root.join("gateway.snapshot"), &IslandId::new("prod"))
            .map_err(|error| format!("load gateway snapshot: {error}"))?
            .routes
            .len(),
        1,
    )?;
    assert_eq_named(
        "loaded dns snapshot records",
        load_dns_snapshot(root.join("dns.snapshot"), &IslandId::new("prod"))
            .map_err(|error| format!("load dns snapshot: {error}"))?
            .records
            .len(),
        1,
    )?;
    assert_eq_named(
        "projected serving commit id",
        manifest.serving_commit.serving_commit_id.to_string(),
        "route-commit-restart".to_string(),
    )
}

async fn assert_serving_answers_without_coordinator(
    serving: &ServingActorHandle,
) -> Result<usize, String> {
    let route = serving
        .gateway_route_for_host("web.example.test")
        .await
        .map_err(|error| format!("read gateway route while coordinator absent: {error}"))?
        .ok_or_else(|| "serving actor lost gateway route while coordinator absent".to_string())?;
    assert_eq_named("serving route active backends", route.backends.len(), 1)?;
    assert_eq_named(
        "serving route old backends",
        route.old_backends_to_drain.len(),
        1,
    )?;
    let records = serving
        .dns_records("web.example.test", "AAAA")
        .await
        .map_err(|error| format!("read dns records while coordinator absent: {error}"))?;
    assert_eq_named("serving dns record count", records.len(), 1)?;
    Ok(2)
}

fn assert_no_precommit_rerun(state: &ParticipantState) -> Result<(), String> {
    assert_eq_named(
        "capacity requests after recovery",
        state.capacity_requests.load(Ordering::SeqCst),
        3,
    )?;
    assert_eq_named(
        "prepare requests after recovery",
        state.prepare_requests.load(Ordering::SeqCst),
        3,
    )?;
    assert_eq_named(
        "start requests after recovery",
        state.start_requests.load(Ordering::SeqCst),
        3,
    )
}

fn assert_visible_nodes(visible_nodes: &mvp_identity::VisibleNodes) -> Result<(), String> {
    let actual = visible_nodes.iter().cloned().collect::<Vec<_>>();
    assert_eq_named(
        "recovered visible nodes",
        actual,
        vec![
            NodeId::new("node-db"),
            NodeId::new("node-queue"),
            NodeId::new("node-web"),
        ],
    )
}

fn handler_failure(subject: impl Into<String>) -> BusError {
    BusError::HandlerFailed {
        subject: subject.into(),
        failure: HandlerFailure::Application,
    }
}

fn test_timeouts() -> DeployTimeouts {
    DeployTimeouts {
        capacity: Duration::from_secs(1),
        participant: Duration::from_secs(1),
    }
}

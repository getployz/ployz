use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mvp_bus::{
    BusActorHandle, BusError, BusSession, FactKey, FactPayload, Grant, HandlerFailure, IslandId,
    PrincipalId,
};
use mvp_deploy::{
    CapacityReply, CleanupFailureKind, CleanupPendingReason, CleanupStatus, DeployCleanupDoneFact,
    DeployCoordinator, DeployDecisionFact, DeployFactWriter, DeployId, DeployManifest,
    DeployOutcome, DeployRecovery, DeployResult, DeployTimeouts, DnsCommitId, DrainInstanceRequest,
    DrainStatus, GatewayCommitId, InstanceCapacityRequirement, InstanceCommandReply,
    InstanceCommandRequest, InstanceId, InstancePlan, InstanceStartOutcome, PhaseId, PhasePlan,
    PhasePolicy, PhaseReversibility, ProjectionCatchUp, RevisionId, RouteCommitId, ServingCommitId,
    ServingCommitPlan, StopInstanceRequest, WrittenDeployFact, deploy_decision_fact_key,
    deploy_decision_fact_payload,
};
use mvp_deploy_p2panda::PandaDeployFactWriter;
use mvp_identity::{NodeId, VisibleNodes};
use mvp_p2panda_authz::ReplicaImportAccess;
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactError, PandaFactOperation, PandaFactStore, SharedPandaFactStore,
};
use mvp_projection::{
    BackendEndpoint, DnsRecordFact, RouteId, ServiceName, load_dns_snapshot, load_gateway_snapshot,
};
use mvp_routing::{
    RoutingResult, ServingFactWriter, WrittenServingFact, serving_commit_fact_key,
    serving_commit_fact_payload,
};
use mvp_routing_p2panda::PandaServingFactWriter;
use mvp_serving::{ServingActorHandle, ServingSnapshotPaths};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::{fact_pattern, pattern};
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::p2panda_projection_fixture::{
    P2pandaMembershipFixture, create_p2panda_membership_fixture,
};
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
    let recovery_replica = authority.grant_in(
        prod.clone(),
        PrincipalId::new("restart-recovery-replica"),
        Grant::empty(),
    );
    let non_replica_importer = authority.grant_in(
        prod.clone(),
        PrincipalId::new("restart-non-replica-importer"),
        deploy_and_serving_grant()?,
    );
    let replica_write_probe = authority.grant_in(
        prod.clone(),
        PrincipalId::new("restart-replica-write-probe"),
        deploy_and_serving_grant()?,
    );
    let deploy_key_denied = authority.grant_in(
        prod.clone(),
        PrincipalId::new("restart-serving-only-writer"),
        Grant::empty().with_fact_write(fact_pattern("/facts/serving/>")?),
    );
    let serving_key_denied = authority.grant_in(
        prod.clone(),
        PrincipalId::new("restart-deploy-only-writer"),
        Grant::empty().with_fact_write(fact_pattern("/facts/deploy/>")?),
    );
    let writer_only_importer = authority.grant_in(
        prod.clone(),
        PrincipalId::new("restart-writer-only-importer"),
        deploy_and_serving_grant()?,
    );

    let operator_author = Arc::new(PandaFactAuthor::new(operator.principal().clone()));
    let recovery_replica_author =
        Arc::new(PandaFactAuthor::new(recovery_replica.principal().clone()));
    let replica_write_author = Arc::new(PandaFactAuthor::new(
        replica_write_probe.principal().clone(),
    ));
    let deploy_key_denied_author =
        Arc::new(PandaFactAuthor::new(deploy_key_denied.principal().clone()));
    let serving_key_denied_author =
        Arc::new(PandaFactAuthor::new(serving_key_denied.principal().clone()));
    let writer_only_importer_author = Arc::new(PandaFactAuthor::new(
        writer_only_importer.principal().clone(),
    ));
    let membership = create_p2panda_membership_fixture(
        &root.join("p2panda-membership"),
        &prod,
        &[
            operator_author.as_ref(),
            deploy_key_denied_author.as_ref(),
            serving_key_denied_author.as_ref(),
            writer_only_importer_author.as_ref(),
        ],
        &[
            (recovery_replica_author.as_ref(), ReplicaImportAccess::Read),
            (replica_write_author.as_ref(), ReplicaImportAccess::Read),
        ],
    )
    .await?;
    let facts = open_membership_deploy_store(Arc::new(raw_bus.clone()), &membership, &prod).await?;
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
    let recovered_facts = import_panda_deploy_facts(
        raw_bus.clone(),
        &prod,
        &membership,
        &recovery_replica,
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
        bus.clone(),
        operator.clone(),
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
    assert_replica_importer_cannot_write_recovery_facts(
        &facts,
        &replica_write_probe,
        replica_write_author.as_ref(),
        &manifest,
    )
    .await?;
    assert_non_replica_cannot_import(
        raw_bus.clone(),
        &prod,
        &membership,
        NonReplicaImportProbe::GrantedNonMember(&non_replica_importer),
        &exported_recovery_operations,
    )
    .await?;
    assert_non_replica_cannot_import(
        raw_bus.clone(),
        &prod,
        &membership,
        NonReplicaImportProbe::WriterOnlyMember(&writer_only_importer),
        &exported_recovery_operations,
    )
    .await?;
    assert_recovery_import_rejects_author_without_fact_grant(
        raw_bus.clone(),
        &prod,
        &membership,
        &recovery_replica,
        deploy_key_denied_author.as_ref(),
        SourceFactKind::DeployDecision,
        &manifest,
    )
    .await?;
    assert_recovery_import_rejects_author_without_fact_grant(
        raw_bus.clone(),
        &prod,
        &membership,
        &recovery_replica,
        serving_key_denied_author.as_ref(),
        SourceFactKind::ServingCommit,
        &manifest,
    )
    .await?;
    assert_recovery_import_rejects_foreign_island_operation(
        &root,
        raw_bus.clone(),
        &prod,
        &membership,
        &recovery_replica,
        &manifest,
    )
    .await?;

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

async fn import_panda_deploy_facts(
    authorizer: mvp_bus::harness::InMemoryBus,
    island: &IslandId,
    membership: &P2pandaMembershipFixture,
    replica_session: &BusSession,
    operations: &[PandaFactOperation],
) -> Result<SharedPandaFactStore, String> {
    let imported = open_membership_deploy_store(Arc::new(authorizer), membership, island).await?;
    import_recovery_operations(&imported, replica_session, operations)
        .await
        .map_err(|error| format!("import p2panda restart recovery operation: {error}"))?;
    Ok(imported)
}

async fn import_recovery_operations(
    facts: &SharedPandaFactStore,
    replica_session: &BusSession,
    operations: &[PandaFactOperation],
) -> Result<(), PandaFactError> {
    for operation in operations {
        facts
            .import_replica_operation(replica_session, operation)
            .await?;
    }
    Ok(())
}

fn deploy_and_serving_grant() -> Result<Grant, String> {
    Ok(Grant::empty()
        .with_fact_write(fact_pattern("/facts/deploy/>")?)
        .with_fact_write(fact_pattern("/facts/serving/>")?))
}

async fn open_membership_deploy_store(
    bus: Arc<mvp_bus::harness::InMemoryBus>,
    membership: &P2pandaMembershipFixture,
    island: &IslandId,
) -> Result<SharedPandaFactStore, String> {
    let facts = SharedPandaFactStore::new(PandaFactStore::new(bus));
    facts
        .install_authority_snapshot(membership.authority_snapshot(island).await?)
        .await;
    Ok(facts)
}

async fn assert_replica_importer_cannot_write_recovery_facts(
    facts: &SharedPandaFactStore,
    replica_session: &BusSession,
    replica_author: &PandaFactAuthor,
    manifest: &DeployManifest,
) -> Result<(), String> {
    for kind in [
        SourceFactKind::DeployDecision,
        SourceFactKind::ServingCommit,
    ] {
        let (key, payload) = kind.key_and_payload(manifest)?;
        let result = facts
            .write_fact_payload(replica_session, replica_author, key, payload)
            .await;
        match result {
            Err(PandaFactError::UntrustedAuthorKey { .. }) => {}
            other => {
                return Err(format!(
                    "expected replica importer {} write to fail membership writer check, got {other:?}",
                    kind.label(),
                ));
            }
        }
    }
    Ok(())
}

async fn assert_non_replica_cannot_import(
    authorizer: mvp_bus::harness::InMemoryBus,
    island: &IslandId,
    membership: &P2pandaMembershipFixture,
    probe: NonReplicaImportProbe<'_>,
    operations: &[PandaFactOperation],
) -> Result<(), String> {
    let [operation, ..] = operations else {
        return Err("deploy recovery exported no operation for non-replica probe".to_string());
    };
    let target = open_membership_deploy_store(Arc::new(authorizer), membership, island).await?;
    let result =
        import_recovery_operations(&target, probe.session(), std::slice::from_ref(operation)).await;
    match result {
        Err(PandaFactError::UnauthorizedReplicaImport { .. }) => Ok(()),
        other => Err(format!(
            "expected {} import to fail replica membership check, got {other:?}",
            probe.label()
        )),
    }
}

async fn assert_recovery_import_rejects_author_without_fact_grant(
    authorizer: mvp_bus::harness::InMemoryBus,
    island: &IslandId,
    membership: &P2pandaMembershipFixture,
    replica_session: &BusSession,
    author: &PandaFactAuthor,
    kind: SourceFactKind,
    manifest: &DeployManifest,
) -> Result<(), String> {
    let operation = write_source_operation(island, membership, author, kind, manifest).await?;
    let target = open_membership_deploy_store(Arc::new(authorizer), membership, island).await?;
    let result = target
        .import_replica_operation(replica_session, &operation)
        .await;
    let (expected_key, _) = kind.key_and_payload(manifest)?;
    match result {
        Err(PandaFactError::UnauthorizedWrite {
            island: denied_island,
            principal,
            key,
        }) if denied_island == *island
            && principal == *author.principal()
            && key == expected_key =>
        {
            Ok(())
        }
        other => Err(format!(
            "expected {} fact-key denial during recovery import, got {other:?}",
            kind.label(),
        )),
    }
}

async fn assert_recovery_import_rejects_foreign_island_operation(
    root: &std::path::Path,
    authorizer: mvp_bus::harness::InMemoryBus,
    local_island: &IslandId,
    local_membership: &P2pandaMembershipFixture,
    replica_session: &BusSession,
    manifest: &DeployManifest,
) -> Result<(), String> {
    let foreign_island = IslandId::new("foreign");
    let foreign_author = PandaFactAuthor::new(PrincipalId::new("foreign-restart-writer"));
    let foreign_membership = create_p2panda_membership_fixture(
        &root.join("foreign-deploy-membership"),
        &foreign_island,
        &[&foreign_author],
        &[],
    )
    .await?;
    let operation = write_source_operation(
        &foreign_island,
        &foreign_membership,
        &foreign_author,
        SourceFactKind::DeployDecision,
        manifest,
    )
    .await?;
    let target =
        open_membership_deploy_store(Arc::new(authorizer), local_membership, local_island).await?;
    let result = target
        .import_replica_operation(replica_session, &operation)
        .await;
    match result {
        Err(PandaFactError::ImportIslandMismatch { session, operation })
            if session == *local_island && operation == foreign_island =>
        {
            Ok(())
        }
        other => Err(format!(
            "expected foreign-island recovery import rejection, got {other:?}"
        )),
    }
}

#[derive(Clone, Copy)]
enum SourceFactKind {
    DeployDecision,
    ServingCommit,
}

enum NonReplicaImportProbe<'a> {
    GrantedNonMember(&'a BusSession),
    WriterOnlyMember(&'a BusSession),
}

impl NonReplicaImportProbe<'_> {
    fn session(&self) -> &BusSession {
        match self {
            Self::GrantedNonMember(session) | Self::WriterOnlyMember(session) => session,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::GrantedNonMember(_) => "granted non-member",
            Self::WriterOnlyMember(_) => "writer-only member",
        }
    }
}

impl SourceFactKind {
    fn label(self) -> &'static str {
        match self {
            Self::DeployDecision => "deploy",
            Self::ServingCommit => "serving",
        }
    }

    fn source_grant(self) -> Result<Grant, String> {
        let pattern = match self {
            Self::DeployDecision => "/facts/deploy/>",
            Self::ServingCommit => "/facts/serving/>",
        };
        Ok(Grant::empty().with_fact_write(fact_pattern(pattern)?))
    }

    fn key_and_payload(self, manifest: &DeployManifest) -> Result<(FactKey, FactPayload), String> {
        match self {
            Self::DeployDecision => {
                let fact = DeployDecisionFact::new(
                    manifest.clone(),
                    VisibleNodes::new([NodeId::new("node-db"), NodeId::new("node-web")]),
                );
                Ok((
                    deploy_decision_fact_key(&fact.deploy_id)
                        .map_err(|error| format!("build source deploy decision key: {error}"))?,
                    deploy_decision_fact_payload(&fact).map_err(|error| {
                        format!("build source deploy decision payload: {error}")
                    })?,
                ))
            }
            Self::ServingCommit => Ok((
                serving_commit_fact_key(&manifest.serving_commit.serving_commit_id)
                    .map_err(|error| format!("build source serving key: {error}"))?,
                serving_commit_fact_payload(&manifest.serving_commit)
                    .map_err(|error| format!("build source serving payload: {error}"))?,
            )),
        }
    }
}

async fn write_source_operation(
    island: &IslandId,
    membership: &P2pandaMembershipFixture,
    author: &PandaFactAuthor,
    kind: SourceFactKind,
    manifest: &DeployManifest,
) -> Result<PandaFactOperation, String> {
    let (bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
    let session = authority.grant_in(
        island.clone(),
        author.principal().clone(),
        kind.source_grant()?,
    );
    let source = open_membership_deploy_store(Arc::new(bus), membership, island).await?;
    let (key, payload) = kind.key_and_payload(manifest)?;
    let write = source
        .write_fact_payload_with_operation(&session, author, key, payload)
        .await
        .map_err(|error| format!("write source deploy recovery operation: {error}"))?;
    write
        .operation()
        .cloned()
        .ok_or_else(|| "source deploy recovery operation was already present".to_string())
}

#[derive(Default)]
struct FactWriteTimings {
    decision_ms: AtomicU64,
    serving_ms: AtomicU64,
    cleanup_done_ms: AtomicU64,
}

struct TimedFactWriter<W> {
    inner: W,
    timings: Arc<FactWriteTimings>,
}

impl<W> DeployFactWriter for TimedFactWriter<W>
where
    W: DeployFactWriter,
{
    fn write_decision<'a>(
        &'a self,
        fact: mvp_deploy::DeployDecisionFact,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenDeployFact>> + Send + 'a>> {
        Box::pin(async move {
            let started = Instant::now();
            let written = self.inner.write_decision(fact).await?;
            self.timings
                .decision_ms
                .store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
            Ok(written)
        })
    }

    fn write_cleanup_done<'a>(
        &'a self,
        fact: DeployCleanupDoneFact,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenDeployFact>> + Send + 'a>> {
        Box::pin(async move {
            let started = Instant::now();
            let written = self.inner.write_cleanup_done(fact).await?;
            self.timings
                .cleanup_done_ms
                .store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
            Ok(written)
        })
    }
}

impl<W> ServingFactWriter for TimedFactWriter<W>
where
    W: ServingFactWriter,
{
    fn write_serving_commit<'a>(
        &'a self,
        commit: &'a ServingCommitPlan,
    ) -> Pin<Box<dyn Future<Output = RoutingResult<WrittenServingFact>> + Send + 'a>> {
        Box::pin(async move {
            let started = Instant::now();
            let written = self.inner.write_serving_commit(commit).await?;
            self.timings
                .serving_ms
                .store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
            Ok(written)
        })
    }
}

fn coordinator_with_panda_facts(
    bus: BusActorHandle,
    session: BusSession,
    facts: SharedPandaFactStore,
    author: Arc<PandaFactAuthor>,
    timings: Arc<FactWriteTimings>,
) -> DeployCoordinator<
    TimedFactWriter<PandaDeployFactWriter>,
    TimedFactWriter<PandaServingFactWriter>,
> {
    DeployCoordinator::with_fact_writers(
        bus,
        session.clone(),
        TimedFactWriter {
            inner: PandaDeployFactWriter::new(facts.clone(), session.clone(), Arc::clone(&author)),
            timings: Arc::clone(&timings),
        },
        TimedFactWriter {
            inner: PandaServingFactWriter::new(facts, session, author),
            timings,
        },
        test_timeouts(),
    )
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
                    backend: None,
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

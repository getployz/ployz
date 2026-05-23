use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use mvp_bus::{
    BusActorHandle, BusError, BusSession, FactAuthorizer, FactContentHash, FactKey, FactKeyPattern,
    FactPayload, Grant, HandlerFailure, IslandId, PrincipalId,
};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_lease::{LeaseClaimed, LeaseFact, LeaseHolder, LeaseTimestamp};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactOperation, PandaFactStore, PandaFactWriteOutcome,
    SharedPandaFactStore,
};
use mvp_projection::{FactCandidate, FactSource, FactSourceResult};
use mvp_volume::{
    BusVolumeParticipantClient, VolumeError, VolumeFactWriteOutcome, VolumeFactWriter, VolumeId,
    VolumeNamespace, VolumeOwnershipEvidence, VolumeOwnershipFact, VolumeOwnershipLeaseEvidence,
    VolumeReceiveReply, VolumeReceiveRequest, VolumeResult, VolumeSnapshotGuid, VolumeSnapshotId,
    VolumeSnapshotReply, VolumeSnapshotRequest, VolumeSourceCleanup, VolumeTransferCommand,
    VolumeTransferExecutor, VolumeTransferId, VolumeTransferTimeouts, current_volume_ownership,
    decode_payload, encode_payload, volume_ownership_fact_key,
};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};

#[derive(Debug, Serialize)]
struct VolumeTransferReport {
    scenario: &'static str,
    visible_nodes_at_decision: usize,
    lease_writes: usize,
    snapshot_requests: usize,
    receive_requests: usize,
    ownership_commit_writes: usize,
    lease_write_ms: u128,
    ownership_commit_write_ms: u128,
    superseded_ownership_candidate_count: usize,
    recovery_read_ms: u128,
    active_conflict_rejections: usize,
    stale_mutation_rejections: usize,
    stale_ownership_rejections: usize,
    expired_lease_rejections: usize,
    preflight_rejections: usize,
    forged_receive_rejections: usize,
    recovered_owner: String,
    source_cleanup: &'static str,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for volume transfer: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("volume-transfer-contract");
    reset_dir(&root)?;

    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let island = IslandId::new("prod");
    let operator = authority.grant_in(
        island.clone(),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let node_session = authority.grant_in(
        island.clone(),
        PrincipalId::new("node-agent"),
        Grant::allow_all(),
    );
    let author = Arc::new(PandaFactAuthor::new(operator.principal().clone()));
    let fact_authorizer: Arc<dyn FactAuthorizer> = Arc::new(raw_bus.clone());
    let store =
        PandaVolumeFactStore::new(PandaFactStore::new(Arc::clone(&fact_authorizer)), author);
    let namespace = namespace("prod")?;
    let volume = volume("db")?;
    seed_initial_owner(&store, &operator, &namespace, &volume).await?;

    let participant_state = Arc::new(ParticipantState::default());
    register_participants(&bus, &node_session, Arc::clone(&participant_state)).await?;
    let executor = volume_transfer_executor(&operator, &bus);

    let mut success_store = store.clone();
    let result = executor
        .transfer(
            &mut success_store,
            command(
                namespace.clone(),
                volume.clone(),
                "transfer-1",
                "node-b",
                10,
            )?,
        )
        .await
        .map_err(|error| format!("execute volume transfer: {error}"))?;
    assert_eq_named("new owner", result.new_owner.as_str(), "node-b")?;
    assert_eq_named("visible nodes", result.visible_nodes.len(), 2)?;
    assert_eq_named(
        "visible node ids",
        visible_node_ids(&result.visible_nodes),
        vec!["node-a".to_string(), "node-b".to_string()],
    )?;
    assert_eq_named("snapshot count", participant_state.snapshot_count(), 1)?;
    assert_eq_named("receive count", participant_state.receive_count(), 1)?;
    let success_snapshot_requests = participant_state.snapshot_count();
    let success_receive_requests = participant_state.receive_count();

    let recovery_started = Instant::now();
    let exported = store.export_operations().await;
    let recovered_store = import_panda_volume_facts(
        &island,
        &operator,
        store.author(),
        &exported,
        Arc::clone(&fact_authorizer),
    )
    .await?;
    let recovered_read_model =
        current_volume_ownership(&recovered_store, &operator, &namespace, &volume)
            .map_err(|error| format!("recover volume ownership: {error}"))?;
    let recovered = recovered_read_model
        .current
        .ok_or_else(|| "recovered volume ownership missing".to_string())?;
    let recovery_read_ms = recovery_started.elapsed().as_millis();
    assert_eq_named("recovered owner", recovered.fact.owner.as_str(), "node-b")?;
    assert_eq_named(
        "recovered visible node ids",
        visible_node_ids(&recovered.fact.visible_nodes),
        vec!["node-a".to_string(), "node-b".to_string()],
    )?;
    assert_eq_named(
        "recovery did not rerun snapshot",
        participant_state.snapshot_count(),
        1,
    )?;
    assert_eq_named(
        "recovery did not rerun receive",
        participant_state.receive_count(),
        1,
    )?;

    prove_active_conflict_rejects_before_rpc(
        &operator,
        &bus,
        &namespace,
        &volume,
        &participant_state,
        Arc::clone(&fact_authorizer),
    )
    .await?;
    prove_stale_holder_rejects_before_commit(
        &operator,
        &bus,
        &namespace,
        &volume,
        &participant_state,
        Arc::clone(&fact_authorizer),
    )
    .await?;
    prove_stale_ownership_rejects_before_commit(
        &operator,
        &bus,
        &namespace,
        &volume,
        &participant_state,
        Arc::clone(&fact_authorizer),
    )
    .await?;
    prove_expired_lease_rejects_before_commit(
        &operator,
        &bus,
        &namespace,
        &volume,
        &participant_state,
        Arc::clone(&fact_authorizer),
    )
    .await?;
    prove_ownership_preflight_rejects_before_side_effect(
        &bus,
        &namespace,
        &volume,
        &participant_state,
    )
    .await?;
    prove_forged_receive_rejects(
        &operator,
        &bus,
        &namespace,
        &volume,
        Arc::clone(&fact_authorizer),
    )
    .await?;
    prove_pre_commit_drop_has_no_new_owner(
        &operator,
        &bus,
        &namespace,
        &volume,
        &participant_state,
        Arc::clone(&fact_authorizer),
    )
    .await?;

    let report = VolumeTransferReport {
        scenario: "volume-transfer-contract",
        visible_nodes_at_decision: result.visible_nodes.len(),
        lease_writes: store.lease_writes(),
        snapshot_requests: success_snapshot_requests,
        receive_requests: success_receive_requests,
        ownership_commit_writes: store.ownership_writes(),
        lease_write_ms: store.lease_write_duration().as_millis(),
        ownership_commit_write_ms: store.ownership_write_duration().as_millis(),
        superseded_ownership_candidate_count: recovered_read_model.superseded.len(),
        recovery_read_ms,
        active_conflict_rejections: 1,
        stale_mutation_rejections: 1,
        stale_ownership_rejections: 1,
        expired_lease_rejections: 1,
        preflight_rejections: 1,
        forged_receive_rejections: 1,
        recovered_owner: recovered.fact.owner.to_string(),
        source_cleanup: source_cleanup_label(result.source_cleanup),
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(&root.join("volume-transfer-contract-metrics.json"), &report)?;
    println!("{json}");
    eprintln!("PASS volume-transfer-contract");
    Ok(())
}

async fn seed_initial_owner(
    store: &PandaVolumeFactStore,
    session: &BusSession,
    namespace: &VolumeNamespace,
    volume: &VolumeId,
) -> Result<(), String> {
    let fact = VolumeOwnershipFact {
        namespace: namespace.clone(),
        volume: volume.clone(),
        epoch: 1,
        owner: NodeId::new("node-a"),
        evidence: VolumeOwnershipEvidence::Initialized {
            initialized_by: LeaseHolder::new("operator"),
        },
        visible_nodes: VisibleNodes::new([NodeId::new("node-a")]),
    };
    store
        .write_volume_ownership(session, fact)
        .await
        .map(|_| ())
        .map_err(|error| format!("seed initial volume owner: {error}"))
}

async fn register_participants(
    bus: &BusActorHandle,
    session: &BusSession,
    state: Arc<ParticipantState>,
) -> Result<(), String> {
    let snapshot_state = Arc::clone(&state);
    bus.subscribe(
        session,
        pattern("node.node-a.rpc.volume.snapshot")?,
        move |ctx| {
            snapshot_state.snapshots.fetch_add(1, Ordering::SeqCst);
            let request: VolumeSnapshotRequest =
                decode_payload(ctx.message.payload(), "snapshot request").map_err(handler_error)?;
            let reply = VolumeSnapshotReply {
                source: request.source,
                transfer_id: request.transfer_id,
                namespace: request.namespace,
                volume: request.volume,
                snapshot_id: snapshot_id("snap-transfer-1").map_err(handler_error)?,
                snapshot_guid: snapshot_guid("guid-transfer-1").map_err(handler_error)?,
                bytes: 4096,
            };
            ctx.reply(encode_payload(&reply).map_err(handler_error)?)
        },
    )
    .await
    .map_err(|error| format!("register snapshot participant: {error}"))?;

    let receive_state = Arc::clone(&state);
    bus.subscribe(
        session,
        pattern("node.node-b.rpc.volume.receive")?,
        move |ctx| {
            receive_state.receives.fetch_add(1, Ordering::SeqCst);
            let request: VolumeReceiveRequest =
                decode_payload(ctx.message.payload(), "receive request").map_err(handler_error)?;
            let reply = VolumeReceiveReply {
                source: request.source,
                target: request.target,
                namespace: request.namespace,
                volume: request.volume,
                transfer_id: request.transfer_id,
                snapshot_id: request.snapshot_id,
                snapshot_guid: request.snapshot_guid,
                bytes: request.bytes,
            };
            ctx.reply(encode_payload(&reply).map_err(handler_error)?)
        },
    )
    .await
    .map_err(|error| format!("register receive participant: {error}"))?;
    Ok(())
}

fn volume_transfer_executor(
    operator: &BusSession,
    bus: &BusActorHandle,
) -> VolumeTransferExecutor<BusVolumeParticipantClient> {
    VolumeTransferExecutor::new(
        operator.clone(),
        BusVolumeParticipantClient::new(bus.clone()),
        VisibleNodes::new([NodeId::new("node-a"), NodeId::new("node-b")]),
        VolumeTransferTimeouts::new(std::time::Duration::from_secs(2)),
    )
}

async fn prove_active_conflict_rejects_before_rpc(
    operator: &BusSession,
    bus: &BusActorHandle,
    namespace: &VolumeNamespace,
    volume: &VolumeId,
    participant_state: &ParticipantState,
    fact_authorizer: Arc<dyn FactAuthorizer>,
) -> Result<(), String> {
    let before = participant_state.snapshot_count();
    let author = Arc::new(PandaFactAuthor::new(operator.principal().clone()));
    let store = PandaVolumeFactStore::new(PandaFactStore::new(fact_authorizer), author);
    seed_initial_owner(&store, operator, namespace, volume).await?;
    store
        .write_lease_claim(operator, namespace, volume, "other", 1, (10, 100))
        .await?;
    let executor = volume_transfer_executor(operator, bus);
    let mut store_for_command = store.clone();
    let result = executor
        .transfer(
            &mut store_for_command,
            command(namespace.clone(), volume.clone(), "conflict", "node-b", 20)?,
        )
        .await;
    if !matches!(result, Err(VolumeError::LeaseConflict { .. })) {
        return Err(format!("expected active lease conflict, got {result:?}"));
    }
    assert_eq_named(
        "conflict no snapshot",
        participant_state.snapshot_count(),
        before,
    )?;
    Ok(())
}

async fn prove_stale_holder_rejects_before_commit(
    operator: &BusSession,
    bus: &BusActorHandle,
    namespace: &VolumeNamespace,
    volume: &VolumeId,
    participant_state: &ParticipantState,
    fact_authorizer: Arc<dyn FactAuthorizer>,
) -> Result<(), String> {
    let author = Arc::new(PandaFactAuthor::new(operator.principal().clone()));
    let store = PandaVolumeFactStore::new(PandaFactStore::new(fact_authorizer), author);
    seed_initial_owner(&store, operator, namespace, volume).await?;
    let stale_injecting =
        StaleLeaseInjectingStore::new(store.clone(), namespace.clone(), volume.clone());
    let executor = volume_transfer_executor(operator, bus);
    let before_ownership_writes = store.ownership_writes();
    let before_snapshot = participant_state.snapshot_count();
    let mut store_for_command = stale_injecting;
    let result = executor
        .transfer(
            &mut store_for_command,
            command(namespace.clone(), volume.clone(), "stale", "node-b", 30)?,
        )
        .await;
    if !matches!(result, Err(VolumeError::StaleLease { .. })) {
        return Err(format!("expected stale lease, got {result:?}"));
    }
    assert_eq_named(
        "stale no ownership commit",
        store.ownership_writes(),
        before_ownership_writes,
    )?;
    if participant_state.snapshot_count() <= before_snapshot {
        return Err("stale proof did not reach participant RPC before lease race".to_string());
    }
    Ok(())
}

async fn prove_stale_ownership_rejects_before_commit(
    operator: &BusSession,
    bus: &BusActorHandle,
    namespace: &VolumeNamespace,
    volume: &VolumeId,
    participant_state: &ParticipantState,
    fact_authorizer: Arc<dyn FactAuthorizer>,
) -> Result<(), String> {
    let author = Arc::new(PandaFactAuthor::new(operator.principal().clone()));
    let store = PandaVolumeFactStore::new(PandaFactStore::new(fact_authorizer), author);
    seed_initial_owner(&store, operator, namespace, volume).await?;
    let raced = VolumeOwnershipFact {
        namespace: namespace.clone(),
        volume: volume.clone(),
        epoch: 2,
        owner: NodeId::new("node-c"),
        evidence: VolumeOwnershipEvidence::transferred(
            NodeId::new("node-a"),
            transfer("raced")?,
            snapshot_id("snap-raced")?,
            snapshot_guid("guid-raced")?,
            2048,
            VolumeOwnershipLeaseEvidence::new(
                LeaseHolder::new("operator"),
                mvp_lease::LeaseEpoch::from_u64(1).map_err(|error| error.to_string())?,
                lease_hash(namespace, volume, "operator", 1, 30, 120)?,
            ),
        ),
        visible_nodes: VisibleNodes::new([NodeId::new("node-a"), NodeId::new("node-c")]),
    };
    let stale_injecting = StaleOwnershipInjectingStore::new(store.clone(), raced);
    let executor = volume_transfer_executor(operator, bus);
    let before_snapshot = participant_state.snapshot_count();
    let mut store_for_command = stale_injecting;
    let result = executor
        .transfer(
            &mut store_for_command,
            command(
                namespace.clone(),
                volume.clone(),
                "stale-owner",
                "node-b",
                35,
            )?,
        )
        .await;
    if !matches!(result, Err(VolumeError::StaleOwnership { .. })) {
        return Err(format!("expected stale ownership, got {result:?}"));
    }
    assert_eq_named(
        "stale ownership no command commit",
        store.ownership_writes(),
        2,
    )?;
    if participant_state.snapshot_count() <= before_snapshot {
        return Err("stale ownership proof did not reach participant RPC".to_string());
    }
    Ok(())
}

async fn prove_expired_lease_rejects_before_commit(
    operator: &BusSession,
    bus: &BusActorHandle,
    namespace: &VolumeNamespace,
    volume: &VolumeId,
    participant_state: &ParticipantState,
    fact_authorizer: Arc<dyn FactAuthorizer>,
) -> Result<(), String> {
    let author = Arc::new(PandaFactAuthor::new(operator.principal().clone()));
    let store = PandaVolumeFactStore::new(PandaFactStore::new(fact_authorizer), author);
    seed_initial_owner(&store, operator, namespace, volume).await?;
    let executor = volume_transfer_executor(operator, bus);
    let before_ownership_writes = store.ownership_writes();
    let before_snapshot = participant_state.snapshot_count();
    let mut store_for_command = store.clone();
    let result = executor
        .transfer(
            &mut store_for_command,
            command_with_window(
                namespace.clone(),
                volume.clone(),
                "expired",
                "node-b",
                60,
                151,
                150,
            )?,
        )
        .await;
    if !matches!(result, Err(VolumeError::StaleLease { .. })) {
        return Err(format!("expected expired lease, got {result:?}"));
    }
    assert_eq_named(
        "expired no ownership commit",
        store.ownership_writes(),
        before_ownership_writes,
    )?;
    if participant_state.snapshot_count() <= before_snapshot {
        return Err("expired lease proof did not reach participant RPC".to_string());
    }
    Ok(())
}

async fn prove_ownership_preflight_rejects_before_side_effect(
    bus: &BusActorHandle,
    namespace: &VolumeNamespace,
    volume: &VolumeId,
    participant_state: &ParticipantState,
) -> Result<(), String> {
    let (raw_bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
    let island = IslandId::new("prod");
    let seeder = authority.grant_in(
        island.clone(),
        PrincipalId::new("volume-seeder"),
        Grant::allow_all(),
    );
    let limited = authority.grant_in(
        island,
        PrincipalId::new("volume-limited"),
        Grant::empty()
            .with_fact_read(
                FactKeyPattern::parse("/facts/volume/>").map_err(|error| error.to_string())?,
            )
            .with_fact_read(
                FactKeyPattern::parse("/facts/lease/>").map_err(|error| error.to_string())?,
            )
            .with_fact_write(
                FactKeyPattern::parse("/facts/lease/>").map_err(|error| error.to_string())?,
            ),
    );
    let author = Arc::new(PandaFactAuthor::new(seeder.principal().clone()));
    let store = PandaVolumeFactStore::new(PandaFactStore::new(Arc::new(raw_bus)), author);
    seed_initial_owner(&store, &seeder, namespace, volume).await?;
    let executor = volume_transfer_executor(&limited, bus);
    let before_snapshot = participant_state.snapshot_count();
    let mut store_for_command = store.clone();
    let result = executor
        .transfer(
            &mut store_for_command,
            command(namespace.clone(), volume.clone(), "preflight", "node-b", 70)?,
        )
        .await;
    if !matches!(result, Err(VolumeError::UnauthorizedFactWrite { .. })) {
        return Err(format!(
            "expected ownership preflight rejection, got {result:?}"
        ));
    }
    assert_eq_named("preflight no lease write", store.lease_writes(), 0)?;
    assert_eq_named(
        "preflight no snapshot",
        participant_state.snapshot_count(),
        before_snapshot,
    )?;
    Ok(())
}

async fn prove_forged_receive_rejects(
    operator: &BusSession,
    bus: &BusActorHandle,
    namespace: &VolumeNamespace,
    volume: &VolumeId,
    fact_authorizer: Arc<dyn FactAuthorizer>,
) -> Result<(), String> {
    let author = Arc::new(PandaFactAuthor::new(operator.principal().clone()));
    let store = PandaVolumeFactStore::new(PandaFactStore::new(fact_authorizer), author);
    seed_initial_owner(&store, operator, namespace, volume).await?;
    let executor = VolumeTransferExecutor::new(
        operator.clone(),
        ForgedReceiveClient::new(BusVolumeParticipantClient::new(bus.clone())),
        VisibleNodes::new([NodeId::new("node-a"), NodeId::new("node-b")]),
        VolumeTransferTimeouts::new(std::time::Duration::from_secs(2)),
    );
    let mut store_for_command = store.clone();
    let result = executor
        .transfer(
            &mut store_for_command,
            command(namespace.clone(), volume.clone(), "forged", "node-b", 40)?,
        )
        .await;
    if !matches!(result, Err(VolumeError::ReceivedSnapshotMismatch { .. })) {
        return Err(format!("expected forged receive rejection, got {result:?}"));
    }
    assert_eq_named("forged no ownership commit", store.ownership_writes(), 1)?;
    Ok(())
}

async fn prove_pre_commit_drop_has_no_new_owner(
    operator: &BusSession,
    bus: &BusActorHandle,
    namespace: &VolumeNamespace,
    volume: &VolumeId,
    participant_state: &ParticipantState,
    fact_authorizer: Arc<dyn FactAuthorizer>,
) -> Result<(), String> {
    let author = Arc::new(PandaFactAuthor::new(operator.principal().clone()));
    let store = PandaVolumeFactStore::new(PandaFactStore::new(fact_authorizer), author);
    seed_initial_owner(&store, operator, namespace, volume).await?;
    let before_snapshot = participant_state.snapshot_count();
    let before_receive = participant_state.receive_count();
    let executor = volume_transfer_executor(operator, bus);
    let mut command_store = FailOwnershipWriteStore::new(store.clone());
    let result = executor
        .transfer(
            &mut command_store,
            command(
                namespace.clone(),
                volume.clone(),
                "drop-before-commit",
                "node-b",
                80,
            )?,
        )
        .await;
    if !matches!(result, Err(VolumeError::FactWrite { .. })) {
        return Err(format!("expected ownership write failure, got {result:?}"));
    }
    if participant_state.snapshot_count() <= before_snapshot
        || participant_state.receive_count() <= before_receive
    {
        return Err("pre-commit drop proof did not reach participant RPCs".to_string());
    }
    let current = current_volume_ownership(&store, operator, namespace, volume)
        .map_err(|error| format!("read pre-commit owner: {error}"))?
        .current
        .ok_or_else(|| "pre-commit owner missing".to_string())?;
    if current.fact.owner != NodeId::new("node-a") {
        return Err(format!(
            "pre-commit drop changed owner to {}",
            current.fact.owner
        ));
    }
    Ok(())
}

async fn import_panda_volume_facts(
    island: &IslandId,
    session: &BusSession,
    author: Arc<PandaFactAuthor>,
    operations: &[PandaFactOperation],
    fact_authorizer: Arc<dyn FactAuthorizer>,
) -> Result<PandaVolumeFactStore, String> {
    let imported = SharedPandaFactStore::new(PandaFactStore::new(fact_authorizer));
    imported
        .trust_author_key(island, author.principal().clone(), author.author_key())
        .await
        .map_err(|error| format!("trust p2panda volume author: {error}"))?;
    for operation in operations {
        imported
            .import_operation(session, operation)
            .await
            .map_err(|error| format!("import volume p2panda operation: {error}"))?;
    }
    Ok(PandaVolumeFactStore::from_shared(imported, author))
}

#[derive(Clone)]
struct PandaVolumeFactStore {
    inner: SharedPandaFactStore,
    author: Arc<PandaFactAuthor>,
    lease_writes: Arc<AtomicUsize>,
    ownership_writes: Arc<AtomicUsize>,
    lease_write_nanos: Arc<AtomicU64>,
    ownership_write_nanos: Arc<AtomicU64>,
}

impl PandaVolumeFactStore {
    fn new(store: PandaFactStore, author: Arc<PandaFactAuthor>) -> Self {
        Self::from_shared(SharedPandaFactStore::new(store), author)
    }

    fn from_shared(inner: SharedPandaFactStore, author: Arc<PandaFactAuthor>) -> Self {
        Self {
            inner,
            author,
            lease_writes: Arc::new(AtomicUsize::new(0)),
            ownership_writes: Arc::new(AtomicUsize::new(0)),
            lease_write_nanos: Arc::new(AtomicU64::new(0)),
            ownership_write_nanos: Arc::new(AtomicU64::new(0)),
        }
    }

    fn author(&self) -> Arc<PandaFactAuthor> {
        Arc::clone(&self.author)
    }

    fn lease_writes(&self) -> usize {
        self.lease_writes.load(Ordering::SeqCst)
    }

    fn ownership_writes(&self) -> usize {
        self.ownership_writes.load(Ordering::SeqCst)
    }

    fn lease_write_duration(&self) -> Duration {
        Duration::from_nanos(self.lease_write_nanos.load(Ordering::SeqCst))
    }

    fn ownership_write_duration(&self) -> Duration {
        Duration::from_nanos(self.ownership_write_nanos.load(Ordering::SeqCst))
    }

    async fn export_operations(&self) -> Vec<PandaFactOperation> {
        self.inner.export_operations().await
    }

    async fn write_volume_ownership(
        &self,
        session: &BusSession,
        fact: VolumeOwnershipFact,
    ) -> VolumeResult<VolumeFactWriteOutcome> {
        let key = volume_ownership_fact_key(&fact.namespace, &fact.volume, fact.epoch)?;
        let payload = fact.to_payload()?;
        self.write_panda(session, key, payload).await
    }

    async fn write_lease_claim(
        &self,
        session: &BusSession,
        namespace: &VolumeNamespace,
        volume: &VolumeId,
        holder: &str,
        epoch: u64,
        window: (u64, u64),
    ) -> Result<VolumeFactWriteOutcome, String> {
        let epoch = mvp_lease::LeaseEpoch::from_u64(epoch).map_err(|error| error.to_string())?;
        let claim = LeaseClaimed::new(
            volume.lease_resource(namespace),
            LeaseHolder::new(holder),
            epoch,
            LeaseTimestamp::from_secs(window.0),
            LeaseTimestamp::from_secs(window.1),
        );
        let key = FactKey::parse(format!("/facts/lease/{}/claimed/{epoch}", claim.resource))
            .map_err(|error| error.to_string())?;
        let payload = mvp_projection::ProjectionFactPayload::LeaseClaimed(claim)
            .to_fact_bytes()
            .map(FactPayload::from)
            .map_err(|error| error.to_string())?;
        self.write_panda(session, key, payload)
            .await
            .map_err(|error| error.to_string())
    }

    async fn write_panda(
        &self,
        session: &BusSession,
        key: FactKey,
        payload: FactPayload,
    ) -> VolumeResult<VolumeFactWriteOutcome> {
        let is_lease = key.as_str().starts_with("/facts/lease/");
        let is_ownership = key.as_str().starts_with("/facts/volume/");
        let started = Instant::now();
        let outcome = self
            .inner
            .write_fact_payload(session, self.author.as_ref(), key, payload)
            .await
            .map_err(|error| VolumeError::FactWrite {
                message: error.to_string(),
            })?;
        let write_outcome = match outcome {
            PandaFactWriteOutcome::Inserted(_) => VolumeFactWriteOutcome::Inserted,
            PandaFactWriteOutcome::AlreadyPresent(_) => VolumeFactWriteOutcome::AlreadyPresent,
            PandaFactWriteOutcome::Conflict(_) => VolumeFactWriteOutcome::Conflict,
        };
        if write_outcome != VolumeFactWriteOutcome::Conflict && is_lease {
            self.lease_writes.fetch_add(1, Ordering::SeqCst);
            self.lease_write_nanos
                .fetch_add(duration_nanos(started.elapsed()), Ordering::SeqCst);
        }
        if write_outcome != VolumeFactWriteOutcome::Conflict && is_ownership {
            self.ownership_writes.fetch_add(1, Ordering::SeqCst);
            self.ownership_write_nanos
                .fetch_add(duration_nanos(started.elapsed()), Ordering::SeqCst);
        }
        Ok(write_outcome)
    }
}

impl FactSource for PandaVolumeFactStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        self.inner.list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        self.inner.read_payloads(island, candidates, session)
    }
}

impl VolumeFactWriter for PandaVolumeFactStore {
    fn preflight(&self, session: &BusSession, key: &FactKey) -> VolumeResult<()> {
        if self
            .inner
            .try_can_write_fact(session, key)
            .map_err(VolumeError::FactSource)?
        {
            return Ok(());
        }
        Err(VolumeError::UnauthorizedFactWrite {
            principal: session.principal().clone(),
            key: key.clone(),
        })
    }

    fn write_fact<'a>(
        &'a mut self,
        session: &'a BusSession,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeFactWriteOutcome>> + Send + 'a>> {
        Box::pin(async move { self.write_panda(session, key, payload).await })
    }
}

struct FailOwnershipWriteStore {
    inner: PandaVolumeFactStore,
}

impl FailOwnershipWriteStore {
    fn new(inner: PandaVolumeFactStore) -> Self {
        Self { inner }
    }
}

impl FactSource for FailOwnershipWriteStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        self.inner.list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        self.inner.read_payloads(island, candidates, session)
    }
}

impl VolumeFactWriter for FailOwnershipWriteStore {
    fn preflight(&self, session: &BusSession, key: &FactKey) -> VolumeResult<()> {
        self.inner.preflight(session, key)
    }

    fn write_fact<'a>(
        &'a mut self,
        session: &'a BusSession,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeFactWriteOutcome>> + Send + 'a>> {
        Box::pin(async move {
            if key.as_str().starts_with("/facts/volume/") {
                return Err(VolumeError::FactWrite {
                    message: "simulated coordinator drop before ownership commit".to_string(),
                });
            }
            self.inner.write_panda(session, key, payload).await
        })
    }
}

struct StaleLeaseInjectingStore {
    inner: PandaVolumeFactStore,
    namespace: VolumeNamespace,
    volume: VolumeId,
    lease_writes_seen: usize,
}

impl StaleLeaseInjectingStore {
    fn new(inner: PandaVolumeFactStore, namespace: VolumeNamespace, volume: VolumeId) -> Self {
        Self {
            inner,
            namespace,
            volume,
            lease_writes_seen: 0,
        }
    }
}

impl FactSource for StaleLeaseInjectingStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        self.inner.list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        self.inner.read_payloads(island, candidates, session)
    }
}

impl VolumeFactWriter for StaleLeaseInjectingStore {
    fn preflight(&self, session: &BusSession, key: &FactKey) -> VolumeResult<()> {
        self.inner.preflight(session, key)
    }

    fn write_fact<'a>(
        &'a mut self,
        session: &'a BusSession,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeFactWriteOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let outcome = self
                .inner
                .write_panda(session, key.clone(), payload)
                .await?;
            if key.as_str().starts_with("/facts/lease/") && self.lease_writes_seen == 0 {
                self.lease_writes_seen += 1;
                self.inner
                    .write_lease_claim(
                        session,
                        &self.namespace,
                        &self.volume,
                        "newer",
                        2,
                        (31, 140),
                    )
                    .await
                    .map_err(|error| VolumeError::FactWrite { message: error })?;
            }
            Ok(outcome)
        })
    }
}

struct StaleOwnershipInjectingStore {
    inner: PandaVolumeFactStore,
    raced: Option<VolumeOwnershipFact>,
}

impl StaleOwnershipInjectingStore {
    fn new(inner: PandaVolumeFactStore, raced: VolumeOwnershipFact) -> Self {
        Self {
            inner,
            raced: Some(raced),
        }
    }
}

impl FactSource for StaleOwnershipInjectingStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        self.inner.list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        self.inner.read_payloads(island, candidates, session)
    }
}

impl VolumeFactWriter for StaleOwnershipInjectingStore {
    fn preflight(&self, session: &BusSession, key: &FactKey) -> VolumeResult<()> {
        self.inner.preflight(session, key)
    }

    fn write_fact<'a>(
        &'a mut self,
        session: &'a BusSession,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeFactWriteOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let outcome = self
                .inner
                .write_panda(session, key.clone(), payload)
                .await?;
            if key.as_str().starts_with("/facts/lease/")
                && let Some(raced) = self.raced.take()
            {
                self.inner.write_volume_ownership(session, raced).await?;
            }
            Ok(outcome)
        })
    }
}

#[derive(Clone)]
struct ForgedReceiveClient {
    inner: BusVolumeParticipantClient,
}

impl ForgedReceiveClient {
    fn new(inner: BusVolumeParticipantClient) -> Self {
        Self { inner }
    }
}

impl mvp_volume::VolumeParticipantClient for ForgedReceiveClient {
    fn snapshot<'a>(
        &'a self,
        session: &'a BusSession,
        request: VolumeSnapshotRequest,
        timeout: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeSnapshotReply>> + Send + 'a>> {
        self.inner.snapshot(session, request, timeout)
    }

    fn receive<'a>(
        &'a self,
        session: &'a BusSession,
        request: VolumeReceiveRequest,
        timeout: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeReceiveReply>> + Send + 'a>> {
        Box::pin(async move {
            let mut reply = self.inner.receive(session, request, timeout).await?;
            reply.snapshot_guid = VolumeSnapshotGuid::parse("guid-forged").map_err(|error| {
                VolumeError::Serialization {
                    message: error.to_string(),
                }
            })?;
            Ok(reply)
        })
    }
}

#[derive(Default)]
struct ParticipantState {
    snapshots: AtomicUsize,
    receives: AtomicUsize,
}

impl ParticipantState {
    fn snapshot_count(&self) -> usize {
        self.snapshots.load(Ordering::SeqCst)
    }

    fn receive_count(&self) -> usize {
        self.receives.load(Ordering::SeqCst)
    }
}

fn command(
    namespace: VolumeNamespace,
    volume: VolumeId,
    transfer_id: &str,
    target: &str,
    observed_at: u64,
) -> Result<VolumeTransferCommand, String> {
    command_with_window(
        namespace,
        volume,
        transfer_id,
        target,
        observed_at,
        observed_at + 1,
        observed_at + 90,
    )
}

fn command_with_window(
    namespace: VolumeNamespace,
    volume: VolumeId,
    transfer_id: &str,
    target: &str,
    observed_at: u64,
    commit_observed_at: u64,
    expires_at: u64,
) -> Result<VolumeTransferCommand, String> {
    Ok(VolumeTransferCommand::new(
        namespace,
        volume,
        transfer(transfer_id)?,
        NodeId::new(target),
        LeaseTimestamp::from_secs(observed_at),
        LeaseTimestamp::from_secs(commit_observed_at),
        LeaseTimestamp::from_secs(expires_at),
    ))
}

fn lease_hash(
    namespace: &VolumeNamespace,
    volume: &VolumeId,
    holder: &str,
    epoch: u64,
    acquired_at: u64,
    expires_at: u64,
) -> Result<mvp_lease::LeaseContentHash, String> {
    let claim = LeaseClaimed::new(
        volume.lease_resource(namespace),
        LeaseHolder::new(holder),
        mvp_lease::LeaseEpoch::from_u64(epoch).map_err(|error| error.to_string())?,
        LeaseTimestamp::from_secs(acquired_at),
        LeaseTimestamp::from_secs(expires_at),
    );
    Ok(LeaseFact::Claimed(claim).content_hash())
}

fn namespace(value: &str) -> Result<VolumeNamespace, String> {
    VolumeNamespace::parse(value).map_err(|error| error.to_string())
}

fn volume(value: &str) -> Result<VolumeId, String> {
    VolumeId::parse(value).map_err(|error| error.to_string())
}

fn transfer(value: &str) -> Result<VolumeTransferId, String> {
    VolumeTransferId::parse(value).map_err(|error| error.to_string())
}

fn snapshot_id(value: &str) -> Result<VolumeSnapshotId, String> {
    VolumeSnapshotId::parse(value).map_err(|error| error.to_string())
}

fn snapshot_guid(value: &str) -> Result<VolumeSnapshotGuid, String> {
    VolumeSnapshotGuid::parse(value).map_err(|error| error.to_string())
}

fn handler_error(_error: impl std::fmt::Display) -> BusError {
    BusError::HandlerFailed {
        subject: "volume-transfer-contract".to_string(),
        failure: HandlerFailure::Application,
    }
}

fn source_cleanup_label(cleanup: VolumeSourceCleanup) -> &'static str {
    match cleanup {
        VolumeSourceCleanup::Deferred => "deferred",
    }
}

fn visible_node_ids(nodes: &VisibleNodes) -> Vec<String> {
    nodes.iter().map(|node| node.as_str().to_string()).collect()
}

fn duration_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

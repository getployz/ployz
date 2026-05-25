use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use ployz::composition::{self, PeerRuntime};
use ployz::machine::{
    IrohEndpointId, MachineAddOutcome, MachineAddRequest, MachineEpoch, MachineId,
    MachineMembershipPort, MachineMembershipService, MachineNetworkIdentity, MachineStatus,
    OverlayIp, WireGuardPublicKey,
};
use ployz::operation::{
    AuthorityEpoch, IdempotencyKey, MutationContext, OperationId, PrincipalId, ScopeId,
};

#[tokio::test]
async fn local_corrosion_agent_applies_membership_schema_through_store_api() {
    let (_agent, store) = start_empty_corrosion_agent(Vec::new()).await;

    apply_membership_schema(&store).await;
    assert_machines_table_exists(&store).await;
}

#[tokio::test]
async fn real_corrosion_store_upserts_and_reads_machine_row() {
    let (_agent, store) = start_empty_corrosion_agent(Vec::new()).await;
    apply_membership_schema(&store).await;

    let row = machine_row("node-a", "endpoint-node-a", "fd00::1");
    let upsert = polis::upsert_machine_statement(&row).expect("upsert");
    let receipt = store
        .execute_transaction(&[upsert], timeout())
        .await
        .expect("insert");
    let query = polis::MachineRowQuery::by_machine_id(row.machine_id()).expect("query");
    let rows = store
        .query(query.statement(), timeout())
        .await
        .expect("query rows");
    let decoded = query.decode_optional(&rows).expect("decode").expect("row");

    assert_eq!(receipt.rows_affected(), 1);
    assert_eq!(decoded, row);
}

#[tokio::test]
async fn two_nodes_join_and_observe_corrosion_membership_rows() {
    let (agent_a, store_a) = start_corrosion_agent(Vec::new()).await;
    let (_agent_b, store_b) = start_corrosion_agent(vec![agent_a.gossip_addr()]).await;

    let peer_a = PeerRuntime::start(
        &temp_identity_path("node-a"),
        polis::PeerProbeDeadline::new(Duration::from_secs(5)),
    )
    .await
    .expect("peer a");
    let peer_b = PeerRuntime::start(
        &temp_identity_path("node-b"),
        polis::PeerProbeDeadline::new(Duration::from_secs(5)),
    )
    .await
    .expect("peer b");
    let context = context();
    let request = request("node-b", 1, "fd00::2", peer_b.endpoint_id().as_str());
    let island = polis::IslandId::parse("prod").expect("island");

    {
        let probe_a_to_b = composition::iroh_peer_rpc_probe(peer_a.endpoint(), peer_b.ticket())
            .expect("probe a to b");
        let membership_a = composition::corrosion_machine_membership(
            store_a.clone(),
            probe_a_to_b,
            island.clone(),
        )
        .await
        .expect("membership a");
        let service_a = MachineMembershipService::new(membership_a);

        let outcome = service_a
            .add_machine(&context, request.clone())
            .await
            .expect("add machine");

        assert!(
            matches!(outcome, MachineAddOutcome::Joined(joined) if joined.machine.as_str() == "node-b")
        );
    }

    wait_for_machine_row(&store_b, "node-b").await;

    {
        let probe_b_to_a = composition::iroh_peer_rpc_probe(peer_b.endpoint(), peer_a.ticket())
            .expect("probe b to a");
        let membership_b = composition::corrosion_machine_membership(store_b, probe_b_to_a, island)
            .await
            .expect("membership b");
        let observed = membership_b
            .observe(&context, &MachineId::parse("node-b").expect("machine"))
            .await
            .expect("observe from b");

        assert!(
            matches!(observed, MachineStatus::Joined(joined) if joined.network.iroh_endpoint_id.as_str() == peer_b.endpoint_id().as_str())
        );
    }

    shutdown_peer(peer_b, "node b").await;
    shutdown_peer(peer_a, "node a").await;
}

#[tokio::test]
async fn restarted_node_keeps_endpoint_id_and_observes_membership_row() {
    let (_agent, store) = start_corrosion_agent(Vec::new()).await;
    let coordinator = PeerRuntime::start(
        &temp_identity_path("restart-coordinator"),
        polis::PeerProbeDeadline::new(Duration::from_secs(5)),
    )
    .await
    .expect("coordinator");
    let node_identity_path = temp_identity_path("restart-node");
    let node_v1 = PeerRuntime::start(
        &node_identity_path,
        polis::PeerProbeDeadline::new(Duration::from_secs(5)),
    )
    .await
    .expect("node v1");
    let endpoint_id = node_v1.endpoint_id();
    let context = context();
    let island = polis::IslandId::parse("prod").expect("island");
    let request = request("node-restart", 1, "fd00::3", endpoint_id.as_str());

    {
        let probe = composition::iroh_peer_rpc_probe(coordinator.endpoint(), node_v1.ticket())
            .expect("probe coordinator to node v1");
        let membership =
            composition::corrosion_machine_membership(store.clone(), probe, island.clone())
                .await
                .expect("membership");
        let service = MachineMembershipService::new(membership);

        let outcome = service
            .add_machine(&context, request)
            .await
            .expect("add machine");

        assert!(
            matches!(outcome, MachineAddOutcome::Joined(joined) if joined.machine.as_str() == "node-restart")
        );
    }

    node_v1
        .shutdown(polis::PeerProbeDeadline::new(Duration::from_secs(5)))
        .await
        .expect("shutdown node v1");
    let node_v2 = PeerRuntime::start(
        &node_identity_path,
        polis::PeerProbeDeadline::new(Duration::from_secs(5)),
    )
    .await
    .expect("node v2");

    assert_eq!(node_v2.endpoint_id(), endpoint_id);

    {
        let probe = composition::iroh_peer_rpc_probe(node_v2.endpoint(), coordinator.ticket())
            .expect("probe node v2 to coordinator");
        let membership = composition::corrosion_machine_membership(store, probe, island)
            .await
            .expect("membership");
        let observed = membership
            .observe(
                &context,
                &MachineId::parse("node-restart").expect("machine"),
            )
            .await
            .expect("observe restarted node");

        assert!(
            matches!(observed, MachineStatus::Joined(joined) if joined.network.iroh_endpoint_id.as_str() == endpoint_id.as_str())
        );
    }

    shutdown_peer(node_v2, "node v2").await;
    shutdown_peer(coordinator, "coordinator").await;
    let _ = std::fs::remove_file(node_identity_path);
}

async fn shutdown_peer(peer: PeerRuntime, label: &str) {
    if let Err(error) = peer
        .shutdown(polis::PeerProbeDeadline::new(Duration::from_secs(15)))
        .await
    {
        eprintln!("peer cleanup timed out for {label}: {error:?}");
    }
}

async fn start_corrosion_agent(
    bootstrap: Vec<std::net::SocketAddr>,
) -> (polis::LocalCorrosionAgent, polis::CorrosionStore) {
    let config = polis::CorrosionAgentConfig::isolated()
        .expect("corrosion config")
        .with_bootstrap(bootstrap)
        .with_schema_file("membership.sql", corrosion_startup_membership_schema());
    start_agent(config).await
}

async fn start_empty_corrosion_agent(
    bootstrap: Vec<std::net::SocketAddr>,
) -> (polis::LocalCorrosionAgent, polis::CorrosionStore) {
    let config = polis::CorrosionAgentConfig::isolated()
        .expect("corrosion config")
        .with_bootstrap(bootstrap);
    start_agent(config).await
}

async fn start_agent(
    config: polis::CorrosionAgentConfig,
) -> (polis::LocalCorrosionAgent, polis::CorrosionStore) {
    let agent = polis::LocalCorrosionAgent::start(config)
        .await
        .expect("corrosion agent");
    let store = agent.store().expect("corrosion store");
    (agent, store)
}

async fn apply_membership_schema(store: &polis::CorrosionStore) {
    let schema = polis::membership_schema_statements().expect("schema");
    store
        .apply_schema(&schema, timeout())
        .await
        .expect("apply schema");
}

async fn assert_machines_table_exists(store: &polis::CorrosionStore) {
    let query = polis::StoreStatement::new(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'machines'",
    )
    .expect("query");
    let rows = store.query(&query, timeout()).await.expect("rows");

    assert_eq!(
        rows.rows().first().and_then(|row| row.text("name").ok()),
        Some("machines")
    );
}

async fn wait_for_machine_row(store: &polis::CorrosionStore, machine_id: &str) {
    let machine = polis::StoreMachineId::parse(machine_id).expect("machine");
    let query = polis::MachineRowQuery::by_machine_id(&machine).expect("query");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows = store
            .query(query.statement(), timeout())
            .await
            .expect("query machine row");
        if query
            .decode_optional(&rows)
            .expect("decode machine row")
            .is_some()
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "machine row {machine_id} was not visible before deadline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn request(
    machine: &str,
    epoch: u64,
    overlay_ip: &str,
    iroh_endpoint_id: &str,
) -> MachineAddRequest {
    MachineAddRequest {
        machine: MachineId::parse(machine).expect("machine"),
        epoch: MachineEpoch::new(epoch).expect("epoch"),
        network: MachineNetworkIdentity::new(
            OverlayIp::parse(overlay_ip).expect("overlay"),
            IrohEndpointId::parse(iroh_endpoint_id).expect("endpoint"),
            WireGuardPublicKey::parse(format!("wg-{machine}")).expect("wireguard"),
        ),
    }
}

fn context() -> MutationContext {
    MutationContext::test_authorized(
        OperationId::parse("machine-add-substrate").expect("operation"),
        IdempotencyKey::parse("machine-add-substrate").expect("idempotency"),
        PrincipalId::parse("operator").expect("principal"),
        ScopeId::parse("cluster").expect("scope"),
        AuthorityEpoch::new(1),
        None,
        SystemTime::now() + Duration::from_secs(30),
    )
}

fn timeout() -> polis::StoreTimeout {
    polis::StoreTimeout::seconds(2).expect("timeout")
}

fn machine_row(machine: &str, endpoint_id: &str, overlay_ip: &str) -> polis::MachineRow {
    polis::MachineRow::new(
        polis::StoreMachineId::parse(machine).expect("machine"),
        polis::IslandId::parse("prod").expect("island"),
        polis::IrohEndpointId::parse(endpoint_id).expect("endpoint"),
        polis::WireGuardPublicKey::parse(format!("wg-{machine}")).expect("wireguard"),
        polis::OverlayIp::parse(overlay_ip).expect("overlay"),
        polis::MembershipLifecycle::Active,
        polis::RowEpoch::new(1).expect("epoch"),
        100,
    )
}

fn corrosion_startup_membership_schema() -> &'static str {
    // Corrosion v1.0 file-backed startup schemas require defaults on non-null
    // columns for forward compatibility. Keep those defaults out of the
    // canonical Polis schema and use them only for the replicated startup
    // topology exercised here; product writes still provide every column.
    "CREATE TABLE IF NOT EXISTS machines (
    machine_id TEXT NOT NULL CHECK(length(trim(machine_id)) > 0),
    island_id TEXT NOT NULL DEFAULT 'unknown-island' CHECK(length(trim(island_id)) > 0),
    iroh_endpoint_id TEXT NOT NULL DEFAULT 'unknown-endpoint' CHECK(length(trim(iroh_endpoint_id)) > 0),
    wireguard_public_key TEXT NOT NULL DEFAULT 'unknown-wireguard' CHECK(length(trim(wireguard_public_key)) > 0),
    overlay_ip TEXT NOT NULL DEFAULT '0.0.0.0' CHECK(length(trim(overlay_ip)) > 0),
    lifecycle TEXT NOT NULL DEFAULT 'active' CHECK(lifecycle IN ('active', 'removing', 'tombstoned', 'conflicted', 'deleted')),
    epoch INTEGER NOT NULL DEFAULT 1 CHECK(epoch > 0),
    updated_at INTEGER NOT NULL DEFAULT 0 CHECK(updated_at >= 0),
    PRIMARY KEY(machine_id)
);

CREATE INDEX IF NOT EXISTS machines_lifecycle_idx
    ON machines(lifecycle);"
}

fn temp_identity_path(label: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "ployz-substrate-spine-{label}-{}-{id}.key",
        std::process::id()
    ))
}

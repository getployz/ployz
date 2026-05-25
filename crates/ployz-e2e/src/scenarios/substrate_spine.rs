use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use ployz::composition;
use ployz::machine::{
    IrohEndpointId, MachineAddOutcome, MachineAddRequest, MachineEpoch, MachineId,
    MachineMembershipPort, MachineMembershipService, MachineNetworkIdentity, MachineStatus,
    OverlayIp, WireGuardPublicKey,
};
use ployz::operation::{
    AuthorityEpoch, IdempotencyKey, MutationContext, OperationId, PrincipalId, ScopeId,
};
use polis::PeerRuntime;

#[tokio::test]
async fn local_corrosion_agent_applies_membership_schema_through_store_api() {
    let (_agent, store) = start_empty_corrosion_agent(Vec::new()).await;

    apply_membership_schema(&store).await;
    verify_membership_schema(&store).await;
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
    let query = polis::MachineRowQuery::by_island_machine_id(row.island_id(), row.machine_id())
        .expect("query");
    let rows = store
        .query(query.statement(), timeout())
        .await
        .expect("query rows");
    let decoded = query.decode_optional(&rows).expect("decode").expect("row");

    assert_eq!(receipt.rows_affected(), 1);
    assert_eq!(decoded, row);
}

#[tokio::test]
async fn corrosion_membership_treats_same_machine_id_in_other_island_as_absent() {
    let (_agent, store) = start_empty_corrosion_agent(Vec::new()).await;
    apply_membership_schema(&store).await;

    let endpoint = "endpoint-node-a";
    let first_island = polis::IslandId::parse("prod").expect("first island");
    let second_island = polis::IslandId::parse("staging").expect("second island");
    let probe = || {
        polis::FakePeerProbe::new()
            .reachable(polis::IrohEndpointId::parse(endpoint).expect("endpoint"))
    };
    let service_a = MachineMembershipService::new(composition::corrosion_machine_membership(
        store.clone(),
        probe(),
        first_island.clone(),
    ));
    let service_b = MachineMembershipService::new(composition::corrosion_machine_membership(
        store.clone(),
        probe(),
        second_island.clone(),
    ));
    let request = request("node-a", 1, "fd00::1", endpoint);

    let first = service_a
        .add_machine(&context(), request.clone())
        .await
        .expect("first island add");
    let second = service_b
        .add_machine(&context(), request)
        .await
        .expect("second island add");

    assert!(
        matches!(first, MachineAddOutcome::Joined(joined) if joined.machine.as_str() == "node-a")
    );
    assert!(
        matches!(second, MachineAddOutcome::Joined(joined) if joined.machine.as_str() == "node-a")
    );
    assert_machine_row_exists(&store, &first_island, "node-a").await;
    assert_machine_row_exists(&store, &second_island, "node-a").await;
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
        );
        let service_a = MachineMembershipService::new(membership_a);

        let outcome = service_a
            .add_machine(&context, request.clone())
            .await
            .expect("add machine");

        assert!(
            matches!(outcome, MachineAddOutcome::Joined(joined) if joined.machine.as_str() == "node-b")
        );
    }

    wait_for_machine_row(&store_b, &island, "node-b").await;

    {
        let probe_b_to_a = composition::iroh_peer_rpc_probe(peer_b.endpoint(), peer_a.ticket())
            .expect("probe b to a");
        let membership_b = composition::corrosion_machine_membership(store_b, probe_b_to_a, island);
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
            composition::corrosion_machine_membership(store.clone(), probe, island.clone());
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
        let membership = composition::corrosion_machine_membership(store, probe, island);
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
    let config = polis::CorrosionAgentFixtureConfig::isolated()
        .expect("corrosion config")
        .with_bootstrap(bootstrap)
        .with_schema_file("membership.sql", polis::membership_replication_schema_sql());
    let (agent, store) = start_agent(config).await;
    verify_membership_replication_schema(&store).await;
    (agent, store)
}

async fn start_empty_corrosion_agent(
    bootstrap: Vec<std::net::SocketAddr>,
) -> (polis::LocalCorrosionAgent, polis::CorrosionStore) {
    let config = polis::CorrosionAgentFixtureConfig::isolated()
        .expect("corrosion config")
        .with_bootstrap(bootstrap);
    start_agent(config).await
}

async fn start_agent(
    config: polis::CorrosionAgentFixtureConfig,
) -> (polis::LocalCorrosionAgent, polis::CorrosionStore) {
    let agent = config.start().await.expect("corrosion agent");
    let store = agent.store().expect("corrosion store");
    (agent, store)
}

async fn apply_membership_schema(store: &polis::CorrosionStore) {
    let schema = polis::membership_schema_statements().expect("schema");
    store
        .execute_transaction(&schema, timeout())
        .await
        .expect("apply schema");
}

async fn verify_membership_schema(store: &polis::CorrosionStore) {
    polis::verify_membership_schema(store, timeout())
        .await
        .expect("membership schema");
}

async fn verify_membership_replication_schema(store: &polis::CorrosionStore) {
    polis::verify_membership_replication_schema(store, timeout())
        .await
        .expect("membership replication schema");
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

async fn assert_machine_row_exists(
    store: &polis::CorrosionStore,
    island_id: &polis::IslandId,
    machine_id: &str,
) {
    let machine = polis::StoreMachineId::parse(machine_id).expect("machine");
    let query = polis::MachineRowQuery::by_island_machine_id(island_id, &machine).expect("query");
    let rows = store
        .query(query.statement(), timeout())
        .await
        .expect("query machine row");
    let row = query
        .decode_optional(&rows)
        .expect("decode machine row")
        .expect("machine row");
    assert_eq!(row.island_id(), island_id);
    assert_eq!(row.machine_id(), &machine);
}

async fn wait_for_machine_row(
    store: &polis::CorrosionStore,
    island_id: &polis::IslandId,
    machine_id: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let machine = polis::StoreMachineId::parse(machine_id).expect("machine");
        let query =
            polis::MachineRowQuery::by_island_machine_id(island_id, &machine).expect("query");
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
    )
}

fn temp_identity_path(label: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "ployz-substrate-spine-{label}-{}-{id}.key",
        std::process::id()
    ))
}

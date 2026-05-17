use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_iroh::{IrohDocsFactSource, IrohFactDoc, IrohFactNode};
use mvp_mesh::{
    InviteId, InviteSecret, IrohEndpointId, JoinCommand, JoinRequest, MachineInvite,
    TombstoneCommand, VisibleNodes, WireGuardAppliedSnapshot, WireGuardPublicKey,
    WireGuardSnapshotPaths, plan_full_mesh, write_applied_snapshot,
};
use mvp_projection::{FactSource, NodeId, ProjectionState, reduce_facts};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::fact_pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::process_role_harness::{
    CoordinatorRequest, MeshRoleClientError, MeshRoleFailureKind as HarnessMeshFailureKind,
    MeshRoleRequest, MeshRoleSuccess, cleanup_orphaned_children as cleanup_process_children,
    request_coordinator, request_mesh_role, spawn_process_role, spawn_process_role_owned,
    wait_for_coordinator, wait_for_mesh_data_plane,
};

const SYNC_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize)]
struct MembershipWireGuardReport {
    scenario: &'static str,
    joined_nodes: usize,
    total_planned_peers: usize,
    post_tombstone_nodes: usize,
    post_tombstone_node0_peers: usize,
    expired_invite_failed: bool,
    tombstoned_rejoin_failed: bool,
    coordinator_mutation_unavailable: bool,
    traffic_before_coordinator_death: bool,
    traffic_after_coordinator_death: bool,
    traffic_after_data_plane_restart: bool,
    tombstoned_peer_rejected: bool,
    join_duration_ms: u128,
    docs_convergence_ms: u128,
    peer_plan_duration_ms: u128,
    data_plane_outage_success_count: usize,
    elapsed_ms: u128,
}

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    cleanup_process_children(&scenario_dir("membership-wireguard-contract"))
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create membership-wireguard runtime: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("membership-wireguard-contract");
    reset_dir(&root)?;

    let island = IslandId::new("prod");
    let (bus, authority) = InMemoryBus::new_with_authority();
    let projection_session = authority.grant_in(
        island.clone(),
        PrincipalId::new("projection"),
        Grant::empty().with_fact_read(fact_pattern("/facts/node/>")?),
    );
    let writer_session = authority.grant_in(
        island.clone(),
        PrincipalId::new("node-writer"),
        Grant::empty().with_fact_write(fact_pattern("/facts/node/>")?),
    );

    let node_a = IrohFactNode::memory()
        .await
        .map_err(|error| format!("spawn iroh bootstrap node: {error}"))?;
    let doc_a = node_a
        .create_fact_doc(island.clone())
        .await
        .map_err(|error| format!("create membership doc: {error}"))?;
    let writer = node_a
        .create_author(writer_session.principal().clone())
        .await
        .map_err(|error| format!("create membership writer: {error}"))?;

    let expired_invite_failed = JoinCommand::admit(join_request(
        invite(island.clone(), 100),
        NodeId::new("expired-node"),
        1,
        100,
        Vec::new(),
        BTreeSet::new(),
    ))
    .is_err();

    let join_started = Instant::now();
    let mut joined_keys = Vec::new();
    for index in 0..10 {
        let node_id = NodeId::new(format!("node-{index}"));
        let result = JoinCommand::admit(join_request(
            invite(island.clone(), 60_000),
            node_id.clone(),
            1,
            10,
            (0..index)
                .map(|visible| NodeId::new(format!("node-{visible}")))
                .collect(),
            BTreeSet::new(),
        ))
        .map_err(|error| format!("admit join for {}: {error}", node_id.as_str()))?;
        doc_a
            .write_fact_payload(&writer, result.fact_key.clone(), result.fact_payload, &bus)
            .await
            .map_err(|error| format!("write joined fact for {}: {error}", node_id.as_str()))?;
        joined_keys.push(result.fact_key);
    }
    let join_duration_ms = join_started.elapsed().as_millis();

    let node_b = IrohFactNode::memory()
        .await
        .map_err(|error| format!("spawn iroh observer node: {error}"))?;
    let doc_b = node_b
        .import_fact_doc(
            doc_a
                .share()
                .await
                .map_err(|error| format!("share membership doc: {error}"))?,
        )
        .await
        .map_err(|error| format!("import membership doc: {error}"))?;
    let source = IrohDocsFactSource::new(node_b.local_view(), Arc::new(bus.clone()));

    let sync_started = Instant::now();
    for key in &joined_keys {
        doc_b
            .wait_for_key(key, SYNC_TIMEOUT)
            .await
            .map_err(|error| format!("wait for joined key {key}: {error}"))?;
    }
    let docs_convergence_ms = sync_started.elapsed().as_millis();

    let state = reduce_membership(&doc_b, &source, &island, &projection_session).await?;
    assert_eq_named("joined nodes", state.nodes.len(), 10)?;

    let plan_started = Instant::now();
    let plans = plan_all_nodes(&state, 1)?;
    let peer_plan_duration_ms = plan_started.elapsed().as_millis();
    let total_planned_peers: usize = plans.values().map(|plan| plan.peers.len()).sum();
    assert_eq_named("ten node full mesh peer count", total_planned_peers, 90)?;

    let node0_plan = plans
        .get(&NodeId::new("node-0"))
        .ok_or_else(|| "missing node-0 plan".to_string())?
        .clone();
    let node1_plan = plans
        .get(&NodeId::new("node-1"))
        .ok_or_else(|| "missing node-1 plan".to_string())?
        .clone();
    let node0_snapshot = WireGuardAppliedSnapshot::from_plan(island.clone(), node0_plan.clone());
    let node1_snapshot = WireGuardAppliedSnapshot::from_plan(island.clone(), node1_plan);
    let node0_snapshot_path = root.join("node-0.wireguard.snapshot");
    let node1_snapshot_path = root.join("node-1.wireguard.snapshot");
    write_applied_snapshot(
        &WireGuardSnapshotPaths {
            applied: node0_snapshot_path.clone(),
        },
        &node0_snapshot,
    )
    .map_err(|error| format!("write node-0 snapshot: {error}"))?;
    write_applied_snapshot(
        &WireGuardSnapshotPaths {
            applied: node1_snapshot_path.clone(),
        },
        &node1_snapshot,
    )
    .map_err(|error| format!("write node-1 snapshot: {error}"))?;

    let coordinator_socket = root.join("coordinator.sock");
    let mut coordinator = spawn_process_role(
        "membership-coordinator",
        "local-coordinator",
        &root,
        &coordinator_socket,
        &[],
    )?;
    wait_for_coordinator(&coordinator_socket).await?;

    let node0_socket = root.join("node-0-mesh.sock");
    let node1_socket = root.join("node-1-mesh.sock");
    let mut node0_role = spawn_mesh_role(
        &root,
        "mesh-node-0",
        &node0_socket,
        &node0_snapshot_path,
        loopback_any_port()?,
        &island,
    )?;
    let mut node1_role = spawn_mesh_role(
        &root,
        "mesh-node-1",
        &node1_socket,
        &node1_snapshot_path,
        loopback_any_port()?,
        &island,
    )?;
    let _node0_addr = wait_for_mesh_data_plane(&node0_socket).await?.listen;
    let node1_addr = wait_for_mesh_data_plane(&node1_socket).await?.listen;

    let traffic_before_coordinator_death =
        send_between(&node0_socket, "node-1", node1_addr, b"before").await?;
    coordinator.kill_and_wait().await?;
    let coordinator_mutation_unavailable = matches!(
        request_coordinator(&coordinator_socket, &CoordinatorRequest::Status).await,
        Err(crate::process_role_harness::CoordinatorClientError::Transport(_))
    );
    let traffic_after_coordinator_death =
        send_between(&node0_socket, "node-1", node1_addr, b"after").await?;

    let tombstone = TombstoneCommand {
        island: island.clone(),
        node_id: NodeId::new("node-9"),
        epoch: 2,
        reason: "force-remove".to_string(),
        visible_nodes: VisibleNodes::new((0..9).map(|index| NodeId::new(format!("node-{index}")))),
    }
    .force_remove()
    .map_err(|error| format!("build tombstone command: {error}"))?;
    doc_a
        .write_fact_payload(
            &writer,
            tombstone.fact_key.clone(),
            tombstone.fact_payload,
            &bus,
        )
        .await
        .map_err(|error| format!("write tombstone fact: {error}"))?;
    doc_b
        .wait_for_key(&tombstone.fact_key, SYNC_TIMEOUT)
        .await
        .map_err(|error| format!("wait for tombstone sync: {error}"))?;
    let removed_state = reduce_membership(&doc_b, &source, &island, &projection_session).await?;
    assert_eq_named("post tombstone nodes", removed_state.nodes.len(), 9)?;
    let removed_node0_plan = plan_full_mesh(&removed_state, &NodeId::new("node-0"), 2)
        .map_err(|error| format!("plan node-0 after tombstone: {error}"))?;
    assert_eq_named(
        "node-0 post tombstone peers",
        removed_node0_plan.peers.len(),
        8,
    )?;
    write_applied_snapshot(
        &WireGuardSnapshotPaths {
            applied: node0_snapshot_path.clone(),
        },
        &WireGuardAppliedSnapshot::from_plan(island.clone(), removed_node0_plan.clone()),
    )
    .map_err(|error| format!("write post-tombstone node-0 snapshot: {error}"))?;
    match request_mesh_role(&node0_socket, &MeshRoleRequest::Reload).await {
        Ok(MeshRoleSuccess::Reloaded(status)) => {
            assert_eq_named("reloaded peer count", status.peer_count, 8)?;
        }
        other => return Err(format!("unexpected mesh reload response: {other:?}")),
    }
    let tombstoned_peer_rejected = matches!(
        request_mesh_role(
            &node0_socket,
            &MeshRoleRequest::Send {
                target_node_id: NodeId::new("node-9"),
                target_addr: node1_addr,
                payload: b"blocked".to_vec(),
            },
        )
        .await,
        Err(MeshRoleClientError::Failure(error))
            if error.kind == HarnessMeshFailureKind::PeerNotApplied
    );

    let tombstoned_rejoin_failed = JoinCommand::admit(join_request(
        invite(island.clone(), 60_000),
        NodeId::new("node-9"),
        3,
        20,
        Vec::new(),
        BTreeSet::from([NodeId::new("node-9")]),
    ))
    .is_err();

    node0_role.kill_and_wait().await?;
    let mut restarted_node0 = spawn_mesh_role(
        &root,
        "mesh-node-0-restarted",
        &node0_socket,
        &node0_snapshot_path,
        loopback_any_port()?,
        &island,
    )?;
    let _restarted_node0_addr = wait_for_mesh_data_plane(&node0_socket).await?.listen;
    let traffic_after_data_plane_restart =
        send_between(&node0_socket, "node-1", node1_addr, b"restart").await?;

    restarted_node0.kill_and_wait().await?;
    node1_role.kill_and_wait().await?;
    node_a
        .shutdown()
        .await
        .map_err(|error| format!("shutdown iroh bootstrap node: {error}"))?;
    node_b
        .shutdown()
        .await
        .map_err(|error| format!("shutdown iroh observer node: {error}"))?;
    cleanup_process_children(&root)?;

    let report = MembershipWireGuardReport {
        scenario: "membership-wireguard-contract",
        joined_nodes: state.nodes.len(),
        total_planned_peers,
        post_tombstone_nodes: removed_state.nodes.len(),
        post_tombstone_node0_peers: removed_node0_plan.peers.len(),
        expired_invite_failed,
        tombstoned_rejoin_failed,
        coordinator_mutation_unavailable,
        traffic_before_coordinator_death,
        traffic_after_coordinator_death,
        traffic_after_data_plane_restart,
        tombstoned_peer_rejected,
        join_duration_ms,
        docs_convergence_ms,
        peer_plan_duration_ms,
        data_plane_outage_success_count: [
            traffic_before_coordinator_death,
            traffic_after_coordinator_death,
            traffic_after_data_plane_restart,
        ]
        .into_iter()
        .filter(|success| *success)
        .count(),
        elapsed_ms: started.elapsed().as_millis(),
    };
    assert!(report.expired_invite_failed);
    assert!(report.tombstoned_rejoin_failed);
    assert!(report.coordinator_mutation_unavailable);
    assert!(report.tombstoned_peer_rejected);
    assert_eq_named(
        "data-plane outage successes",
        report.data_plane_outage_success_count,
        3,
    )?;

    let json = write_json(
        &root.join("membership-wireguard-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS membership-wireguard-contract");
    Ok(())
}

async fn reduce_membership(
    doc: &IrohFactDoc,
    source: &IrohDocsFactSource,
    island: &IslandId,
    session: &mvp_bus::BusSession,
) -> Result<ProjectionState, String> {
    doc.refresh_local_view()
        .await
        .map_err(|error| format!("refresh membership facts: {error}"))?;
    let pattern = fact_pattern("/facts/node/>")?;
    let candidates = source
        .list_candidates(island, &pattern, session)
        .map_err(|error| format!("list membership candidates: {error}"))?;
    let payloads = source
        .read_payloads(island, &candidates, session)
        .map_err(|error| format!("read membership payloads: {error}"))?;
    Ok(reduce_facts(island, &candidates, &payloads))
}

fn plan_all_nodes(
    state: &ProjectionState,
    revision: u64,
) -> Result<BTreeMap<NodeId, mvp_mesh::WireGuardPeerPlan>, String> {
    let mut plans = BTreeMap::new();
    for node_id in state.nodes.keys() {
        let plan = plan_full_mesh(state, node_id, revision)
            .map_err(|error| format!("plan full mesh for {}: {error}", node_id.as_str()))?;
        plans.insert(node_id.clone(), plan);
    }
    Ok(plans)
}

fn join_request(
    invite: MachineInvite,
    node_id: NodeId,
    epoch: u64,
    now_ms: u64,
    visible_nodes: Vec<NodeId>,
    tombstoned_nodes: BTreeSet<NodeId>,
) -> JoinRequest {
    let node = node_id.as_str().to_string();
    JoinRequest {
        invite,
        presented_secret: InviteSecret::new("secret"),
        node_id,
        epoch,
        iroh_endpoint_id: IrohEndpointId::new(format!("iroh-{node}")),
        wg_public_key: WireGuardPublicKey::new(format!("wg-{node}")),
        now_ms,
        visible_nodes: VisibleNodes::new(visible_nodes),
        tombstoned_nodes,
    }
}

fn invite(island: IslandId, expires_at_ms: u64) -> MachineInvite {
    MachineInvite {
        island,
        bootstrap_endpoint: IrohEndpointId::new("iroh-bootstrap"),
        invite_id: InviteId::new("invite-1"),
        secret: InviteSecret::new("secret"),
        expires_at_ms,
    }
}

fn spawn_mesh_role(
    root: &std::path::Path,
    name: &'static str,
    socket: &std::path::Path,
    snapshot: &std::path::Path,
    listen: SocketAddr,
    island: &IslandId,
) -> Result<crate::process_role_harness::RunningChild, String> {
    spawn_process_role_owned(
        name,
        "mesh-data-plane",
        root,
        socket,
        &[
            "--snapshot".to_string(),
            snapshot.display().to_string(),
            "--listen".to_string(),
            listen.to_string(),
            "--island".to_string(),
            island.as_str().to_string(),
        ],
    )
}

async fn send_between(
    source_socket: &std::path::Path,
    target_node_id: &str,
    target_addr: SocketAddr,
    payload: &[u8],
) -> Result<bool, String> {
    match request_mesh_role(
        source_socket,
        &MeshRoleRequest::Send {
            target_node_id: NodeId::new(target_node_id),
            target_addr,
            payload: payload.to_vec(),
        },
    )
    .await
    {
        Ok(MeshRoleSuccess::Sent { payload: returned }) => Ok(returned == payload),
        other => Err(format!("unexpected mesh send response: {other:?}")),
    }
}

fn loopback_any_port() -> Result<SocketAddr, String> {
    "127.0.0.1:0"
        .parse()
        .map_err(|error| format!("parse loopback any-port addr: {error}"))
}

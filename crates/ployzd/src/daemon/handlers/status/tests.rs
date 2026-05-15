use super::control_plane::{component_health_status, storage_replica_intent_status};
use super::edge_sync::{SyncMetric, metric_status, parse_sync_metric, parse_sync_metric_u64};
use super::nats::{
    nats_asset_is_healthy, nats_asset_probe_error, nats_current_replicas, nats_max_lag,
    nats_offline_replicas,
};
use crate::daemon::{ActiveMesh, DaemonState, RetainedSubnet};
use crate::mesh_state::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
use async_nats::jetstream::stream::{ClusterInfo, PeerInfo};
use ipnet::Ipv4Net;
use ployz_api::{
    ControlPlaneHealthState, DaemonPayload, EdgeSyncHealthState, NatsAssetHealthState,
    NatsAssetReplicaStatus, NatsAssetStatus,
};
use ployz_model::{
    AuthorityId, AuthorityNodeRole, ControlPlaneDataBucket, ControlPlaneLossImpact, MachineId,
    MachineLifecycle, MachineMembership, MachineTopology, NetworkLifecycle, NetworkName, OverlayIp,
    PublicKey, StorageParticipation, StorageReplicaPolicy,
};
use ployz_nats::{NatsScope, NatsStore};
use ployz_orchestrator::Mesh;
use ployz_orchestrator::mesh::driver::WireguardDriver;
use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
use ployz_runtime_api::Identity;
use ployz_store_api::{MachineMembershipStore, StoreDriver};
use ployz_store_memory::{MemoryService, MemoryStore, StoreDriverMemoryExt as _};
use ployz_supervision::ComponentHealth;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

fn peer(name: &str, current: bool, offline: bool, lag: Option<u64>) -> PeerInfo {
    PeerInfo {
        name: name.to_string(),
        current,
        active: Duration::from_secs(1),
        offline,
        lag,
    }
}

fn cluster(leader: Option<&str>, replicas: Vec<PeerInfo>) -> ClusterInfo {
    ClusterInfo {
        name: None,
        raft_group: None,
        leader: leader.map(str::to_string),
        leader_since: None,
        replicas,
        ..ClusterInfo::default()
    }
}

fn replica_status(replicas: usize, cluster: Option<&ClusterInfo>) -> NatsAssetReplicaStatus {
    NatsAssetReplicaStatus {
        replicas,
        current_replicas: nats_current_replicas(replicas, cluster),
        offline_replicas: nats_offline_replicas(replicas, cluster),
        max_lag: nats_max_lag(cluster),
        leader: cluster.and_then(|cluster| cluster.leader.clone()),
    }
}

#[tokio::test]
async fn status_derives_local_authority_from_authoritative_self_record() {
    let mut state = make_status_state(true).await;
    state
        .active
        .as_mut()
        .expect("active mesh")
        .config
        .storage_participation = StorageParticipation::Candidate;

    let response = state.handle_status().await;
    let Some(DaemonPayload::Status(payload)) = response.payload() else {
        panic!("expected status payload");
    };
    let authority = payload.local_authority.expect("local authority posture");

    assert_eq!(
        authority.role(),
        AuthorityNodeRole::AuthorityStorage {
            authority_id: AuthorityId::default_authority(),
        }
    );
    assert_eq!(
        authority.data_bucket(),
        ControlPlaneDataBucket::StoredIntent
    );
    assert_eq!(
        authority.loss_impact(),
        ControlPlaneLossImpact::StoredTruthLost
    );
    teardown_status_state(&mut state).await;
}

#[tokio::test]
async fn status_omits_local_authority_when_self_record_is_missing() {
    let mut state = make_status_state(false).await;

    let response = state.handle_status().await;
    let Some(DaemonPayload::Status(payload)) = response.payload() else {
        panic!("expected status payload");
    };

    assert_eq!(payload.local_machine_lifecycle, None);
    assert_eq!(payload.local_authority, None);
    teardown_status_state(&mut state).await;
}

#[test]
fn parses_sidecar_sync_metric_by_stream() {
    let metrics = r#"
# HELP ployz_gateway_store_sync_healthy Whether ployz-gateway store subscriptions are current.
# TYPE ployz_gateway_store_sync_healthy gauge
ployz_gateway_store_sync_healthy{stream="routing"} 1
ployz_gateway_store_sync_healthy{stream="certificates"} 0
ployz_gateway_store_sync_state_since_unix_seconds{stream="certificates"} 1777646000
ployz_gateway_store_sync_failures_total{stream="certificates"} 3
"#;

    assert_eq!(
        parse_sync_metric(metrics, "ployz_gateway_store_sync_healthy", "routing"),
        SyncMetric::Present(true)
    );
    assert_eq!(
        parse_sync_metric(metrics, "ployz_gateway_store_sync_healthy", "certificates"),
        SyncMetric::Present(false)
    );
    assert_eq!(
        parse_sync_metric(
            metrics,
            "ployz_gateway_store_sync_healthy",
            "acme_challenges"
        ),
        SyncMetric::Missing
    );
    assert_eq!(
        parse_sync_metric_u64(
            metrics,
            "ployz_gateway_store_sync_state_since_unix_seconds",
            "certificates"
        ),
        Some(1_777_646_000)
    );
    assert_eq!(
        parse_sync_metric_u64(
            metrics,
            "ployz_gateway_store_sync_failures_total",
            "certificates"
        ),
        Some(3)
    );
}

#[test]
fn sidecar_sync_status_reports_invalid_health_metric_as_unknown() {
    let metrics = String::from(
        r#"
ployz_gateway_store_sync_healthy{stream="routing"} 2
ployz_gateway_store_sync_failures_total{stream="routing"} 4
"#,
    );

    let status = metric_status(
        "gateway",
        "routing",
        "ployz_gateway_store_sync_healthy",
        "ployz_gateway_store_sync_state_since_unix_seconds",
        "ployz_gateway_store_sync_failures_total",
        Ok(&metrics),
    );

    match status.state {
        EdgeSyncHealthState::Unknown {
            error,
            failures_total,
        } => {
            assert!(error.contains("invalid boolean value 2"));
            assert_eq!(failures_total, Some(4));
        }
        other => panic!("expected unknown sidecar sync state, got {other:?}"),
    }
}

#[test]
fn sidecar_sync_status_reports_missing_health_metric_as_unknown() {
    let metrics = String::from(
        r#"
ployz_gateway_store_sync_failures_total{stream="routing"} 4
"#,
    );

    let status = metric_status(
        "gateway",
        "routing",
        "ployz_gateway_store_sync_healthy",
        "ployz_gateway_store_sync_state_since_unix_seconds",
        "ployz_gateway_store_sync_failures_total",
        Ok(&metrics),
    );

    assert_eq!(status.service, "gateway");
    assert_eq!(status.stream, "routing");
    match status.state {
        EdgeSyncHealthState::Unknown {
            error,
            failures_total,
        } => {
            assert!(error.contains("was absent"));
            assert_eq!(failures_total, Some(4));
        }
        other => panic!("expected unknown sidecar sync state, got {other:?}"),
    }
}

#[test]
fn sidecar_sync_status_reports_unhealthy_metric_with_staleness_and_failures() {
    let metrics = String::from(
        r#"
ployz_gateway_store_sync_healthy{stream="certificates"} 0
ployz_gateway_store_sync_state_since_unix_seconds{stream="certificates"} 1777646000
ployz_gateway_store_sync_failures_total{stream="certificates"} 7
"#,
    );

    let status = metric_status(
        "gateway",
        "certificates",
        "ployz_gateway_store_sync_healthy",
        "ployz_gateway_store_sync_state_since_unix_seconds",
        "ployz_gateway_store_sync_failures_total",
        Ok(&metrics),
    );

    assert_eq!(status.service, "gateway");
    assert_eq!(status.stream, "certificates");
    match status.state {
        EdgeSyncHealthState::Stale {
            stale_since_unix_secs,
            failures_total,
        } => {
            assert_eq!(stale_since_unix_secs, 1_777_646_000);
            assert_eq!(failures_total, 7);
        }
        other => panic!("expected stale sidecar sync state, got {other:?}"),
    }
}

#[test]
fn sidecar_sync_status_reports_unreadable_metrics_as_unknown() {
    let error = String::from("connection refused");

    let status = metric_status(
        "dns",
        "routing",
        "ployz_dns_store_sync_healthy",
        "ployz_dns_store_sync_state_since_unix_seconds",
        "ployz_dns_store_sync_failures_total",
        Err(&error),
    );

    assert_eq!(status.service, "dns");
    assert_eq!(status.stream, "routing");
    match status.state {
        EdgeSyncHealthState::Unknown {
            error,
            failures_total,
        } => {
            assert_eq!(error, "connection refused");
            assert_eq!(failures_total, None);
        }
        other => panic!("expected unknown sidecar sync state, got {other:?}"),
    }
}

#[test]
fn component_health_status_preserves_stale_failure_details() {
    let first = ComponentHealth::stale(1_777_646_000, None, "subscription closed");
    let health = ComponentHealth::stale(1_777_646_100, Some(&first), "ack failed");

    let status = component_health_status("node_rpc_listener", &health);

    assert_eq!(status.component, "node_rpc_listener");
    match status.state {
        ControlPlaneHealthState::Stale {
            stale_since_unix_secs,
            consecutive_failures,
            error,
        } => {
            assert_eq!(stale_since_unix_secs, 1_777_646_000);
            assert_eq!(consecutive_failures, 2);
            assert_eq!(error, "ack failed");
        }
        other => panic!("expected stale component health, got {other:?}"),
    }
}

#[test]
fn nats_asset_probe_error_preserves_classified_asset_context() {
    let scope = NatsScope::local_default();
    let manifest = NatsStore::asset_manifest_for_scope(&scope);
    let statuses = nats_asset_probe_error(&scope, &manifest, String::from("connect failed"));

    assert_eq!(statuses.len(), manifest.len());
    let status = statuses
        .iter()
        .find(|status| status.name == "routing_events_auth-default")
        .expect("routing events status is present");
    assert_eq!(status.kind, "stream");
    assert_eq!(status.data_bucket, ControlPlaneDataBucket::Projection);
    assert_eq!(
        status.loss_impact,
        ControlPlaneLossImpact::NoStoredTruthLost
    );
    assert_eq!(status.installation.as_deref(), Some("local"));
    assert_eq!(status.authority.as_deref(), Some("auth-default"));
    assert_eq!(status.domain.as_deref(), Some("dom-auth-default"));
    assert_eq!(status.scope.as_deref(), Some("authority_local"));
    match &status.state {
        NatsAssetHealthState::Unknown { error } => assert_eq!(error, "connect failed"),
        other => panic!("expected unknown NATS asset state, got {other:?}"),
    }

    let status = statuses
        .iter()
        .find(|status| status.name == "machines_local")
        .expect("machines status is present");
    assert_eq!(status.data_bucket, ControlPlaneDataBucket::StoredIntent);
    assert_eq!(status.loss_impact, ControlPlaneLossImpact::StoredTruthLost);
    assert_eq!(status.authority, None);
    assert_eq!(status.domain.as_deref(), Some("dom-local-root"));
    assert_eq!(status.scope.as_deref(), Some("installation_root"));
}

#[test]
fn nats_asset_health_treats_single_replica_without_cluster_as_current() {
    assert!(nats_asset_is_healthy(&replica_status(1, None)));
    assert_eq!(nats_current_replicas(1, None), 1);
    assert_eq!(nats_offline_replicas(1, None), 0);
    assert_eq!(nats_max_lag(None), 0);
}

#[test]
fn nats_asset_health_reports_current_cluster_replicas() {
    let cluster = cluster(
        Some("nats-a"),
        vec![
            peer("nats-b", true, false, Some(0)),
            peer("nats-c", true, false, Some(0)),
        ],
    );

    assert!(nats_asset_is_healthy(&replica_status(3, Some(&cluster))));
    assert_eq!(nats_current_replicas(3, Some(&cluster)), 3);
    assert_eq!(nats_offline_replicas(3, Some(&cluster)), 0);
    assert_eq!(nats_max_lag(Some(&cluster)), 0);
}

#[test]
fn nats_asset_health_reports_lagging_or_offline_replicas() {
    let cluster = cluster(
        Some("nats-a"),
        vec![
            peer("nats-b", false, false, Some(12)),
            peer("nats-c", false, true, Some(44)),
        ],
    );

    assert!(!nats_asset_is_healthy(&replica_status(3, Some(&cluster))));
    assert_eq!(nats_current_replicas(3, Some(&cluster)), 1);
    assert_eq!(nats_offline_replicas(3, Some(&cluster)), 1);
    assert_eq!(nats_max_lag(Some(&cluster)), 44);
}

#[test]
fn nats_asset_health_reports_missing_cluster_for_replicated_asset() {
    assert!(!nats_asset_is_healthy(&replica_status(3, None)));
    assert_eq!(nats_current_replicas(3, None), 0);
    assert_eq!(nats_offline_replicas(3, None), 3);
    assert_eq!(nats_max_lag(None), 0);
}

#[test]
fn storage_replica_intent_reports_mismatched_observed_replicas() {
    let status = storage_replica_intent_status(
        StorageReplicaPolicy::R3,
        &[NatsAssetStatus {
            name: "cp_instances_auth-default".into(),
            kind: "kv".into(),
            data_bucket: ControlPlaneDataBucket::StoredIntent,
            loss_impact: ControlPlaneLossImpact::StoredTruthLost,
            installation: None,
            authority: Some("auth-default".into()),
            domain: Some("dom-auth-default".into()),
            scope: Some("authority_local".into()),
            state: NatsAssetHealthState::Healthy(NatsAssetReplicaStatus {
                replicas: 1,
                current_replicas: 1,
                offline_replicas: 0,
                max_lag: 0,
                leader: Some("nats-a".into()),
            }),
        }],
    )
    .expect("replica intent status");

    assert_eq!(status.component, "storage_replica_intent");
    let ControlPlaneHealthState::Stale { error, .. } = status.state else {
        panic!("expected stale replica intent");
    };
    assert!(error.contains("expected 3"));
}

#[test]
fn storage_replica_intent_is_healthy_when_assets_match_desired_replicas() {
    let status = storage_replica_intent_status(
        StorageReplicaPolicy::R3,
        &[NatsAssetStatus {
            name: "cp_instances_auth-default".into(),
            kind: "kv".into(),
            data_bucket: ControlPlaneDataBucket::StoredIntent,
            loss_impact: ControlPlaneLossImpact::StoredTruthLost,
            installation: None,
            authority: Some("auth-default".into()),
            domain: Some("dom-auth-default".into()),
            scope: Some("authority_local".into()),
            state: NatsAssetHealthState::Healthy(NatsAssetReplicaStatus {
                replicas: 3,
                current_replicas: 3,
                offline_replicas: 0,
                max_lag: 0,
                leader: Some("nats-a".into()),
            }),
        }],
    )
    .expect("replica intent status");

    assert_eq!(status.component, "storage_replica_intent");
    assert!(matches!(status.state, ControlPlaneHealthState::Healthy));
}

#[test]
fn storage_replica_intent_stays_stale_when_asset_observation_is_stale() {
    let status = storage_replica_intent_status(
        StorageReplicaPolicy::R3,
        &[NatsAssetStatus {
            name: "cp_instances_auth-default".into(),
            kind: "kv".into(),
            data_bucket: ControlPlaneDataBucket::StoredIntent,
            loss_impact: ControlPlaneLossImpact::StoredTruthLost,
            installation: None,
            authority: Some("auth-default".into()),
            domain: Some("dom-auth-default".into()),
            scope: Some("authority_local".into()),
            state: NatsAssetHealthState::Stale(NatsAssetReplicaStatus {
                replicas: 3,
                current_replicas: 3,
                offline_replicas: 0,
                max_lag: 0,
                leader: Some("nats-a".into()),
            }),
        }],
    )
    .expect("replica intent status");

    assert_eq!(status.component, "storage_replica_intent");
    let ControlPlaneHealthState::Stale { error, .. } = status.state else {
        panic!("expected stale replica intent");
    };
    assert!(error.contains("observation is stale"));
}

async fn make_status_state(start_mesh: bool) -> DaemonState {
    let identity = Identity::generate(MachineId::new("founder"), [1; 32]);
    let founder_subnet: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
    let data_dir = unique_temp_dir("ployz-status-state");
    let config = NetworkConfig::new(
        NetworkName("alpha".into()),
        &identity.public_key,
        DEFAULT_CLUSTER_CIDR,
        founder_subnet,
    );
    config
        .save(&NetworkConfig::path(&data_dir, "alpha"))
        .expect("save config");

    let store = Arc::new(MemoryStore::new());
    let service = Arc::new(MemoryService::new());
    let network = Arc::new(MemoryWireGuard::new());
    store
        .upsert_self_machine(&test_machine_record(
            "founder",
            founder_subnet,
            MachineLifecycle::Standby,
            identity.public_key.clone(),
        ))
        .await
        .expect("upsert founder");

    let mut mesh = Mesh::new(
        WireguardDriver::memory_with(network),
        StoreDriver::memory_with(store, service),
        None,
        identity.machine_id.clone(),
        51820,
    );
    if start_mesh {
        mesh.up().await.expect("mesh up");
    }

    let mut state = DaemonState::new_for_tests(
        &data_dir,
        identity,
        DEFAULT_CLUSTER_CIDR.into(),
        24,
        4319,
        "127.0.0.1:0".into(),
        None,
        1,
    );
    let mut config = config;
    config.lifecycle = if start_mesh {
        NetworkLifecycle::Running
    } else {
        NetworkLifecycle::Stopped
    };
    let retained_subnet = RetainedSubnet::from_running_config(config.subnet);
    state.active = Some(ActiveMesh {
        config,
        retained_subnet,
        mesh,
        nats_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        image_receiver: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        image_receiver_bind_addr: None,
        gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        certificate_renewal: None,
        bootstrap_peer_seed: None,
    });
    state
}

async fn teardown_status_state(state: &mut DaemonState) {
    let Some(active) = state.active.as_mut() else {
        return;
    };
    active.mesh.destroy().await.expect("destroy mesh");
}

fn test_machine_record(
    id: &str,
    subnet: Ipv4Net,
    lifecycle: MachineLifecycle,
    public_key: PublicKey,
) -> MachineMembership {
    MachineMembership {
        id: MachineId::new(id),
        public_key,
        overlay_ip: format!("fd00::{id_len:x}", id_len = id.len())
            .parse()
            .map(OverlayIp)
            .expect("valid overlay"),
        topology: MachineTopology::local(),
        region_role: ployz_model::RegionRole::HomeData,
        subnet: Some(subnet),
        bridge_ip: None,
        endpoints: vec!["127.0.0.1:51820".into()],
        lifecycle,
        storage_role: StorageParticipation::default_authority().into(),
        created_at: 0,
        updated_at: 0,
        labels: BTreeMap::new(),
    }
}

fn unique_temp_dir(label: &str) -> PathBuf {
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{}-{nanos}-{sequence}", std::process::id()))
}

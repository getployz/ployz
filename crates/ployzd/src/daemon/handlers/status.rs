use std::time::Duration;

use async_nats::jetstream::stream::ClusterInfo;
use ployz_api::{
    ControlPlaneHealthState, ControlPlaneStatus, DaemonPayload, DaemonResponse,
    EdgeSyncHealthState, EdgeSyncStatus, NatsAssetHealthState, NatsAssetReplicaStatus,
    NatsAssetStatus, StatusPayload,
};
use ployz_config::RuntimeTarget;
use ployz_nats::{NatsAssetScope, NatsAssetSpec, NatsStore};
use ployz_types::model::{AuthorityNodePosture, ControlPlaneDataBucket, ControlPlaneLossImpact};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::super::DaemonState;

const EDGE_SYNC_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const NATS_ASSET_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
impl DaemonState {
    pub(crate) async fn handle_status(&self) -> DaemonResponse {
        let id = &self.identity;
        match &self.active {
            Some(active) => {
                let local_machine_lifecycle = active
                    .mesh
                    .authoritative_self_record()
                    .await
                    .map(|machine| machine.lifecycle);
                let net = &active.config;
                let payload = StatusPayload {
                    machine_id: id.machine_id.0.clone(),
                    public_key: id.public_key.clone(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    network: Some(net.name.0.clone()),
                    network_lifecycle: Some(net.lifecycle),
                    local_machine_lifecycle,
                    local_authority: Some(AuthorityNodePosture::from_storage_participation(
                        net.storage,
                        &net.storage_participation,
                    )),
                    overlay_ip: Some(net.overlay_ip.0.to_string()),
                    mesh_phase: format!("{:?}", active.mesh.phase()),
                    edge_sync: self.edge_sync_status().await,
                    nats_assets: self.nats_asset_status().await,
                    control_plane: self.control_plane_status().await,
                };
                self.ok_with_payload(
                    format!(
                        "machine:            {}\nversion:            {}\nnetwork:            {}\nnetwork lifecycle:  {}\nlocal lifecycle:    {}\noverlay:            {}\nmesh phase:         {:?}",
                        id.machine_id,
                        env!("CARGO_PKG_VERSION"),
                        net.name,
                        net.lifecycle,
                        local_machine_lifecycle
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "unknown".into()),
                        net.overlay_ip,
                        active.mesh.phase(),
                    ),
                    Some(DaemonPayload::Status(payload)),
                )
            }
            None => self.ok_with_payload(
                format!(
                    "machine:            {}\nversion:            {}\nnetwork:            none\nnetwork lifecycle:  —\nlocal lifecycle:    —\nmesh phase:         idle",
                    id.machine_id,
                    env!("CARGO_PKG_VERSION")
                ),
                Some(DaemonPayload::Status(StatusPayload {
                    machine_id: id.machine_id.0.clone(),
                    public_key: id.public_key.clone(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    network: None,
                    network_lifecycle: None,
                    local_machine_lifecycle: None,
                    local_authority: None,
                    overlay_ip: None,
                    mesh_phase: String::from("idle"),
                    edge_sync: Vec::new(),
                    nats_assets: Vec::new(),
                    control_plane: Vec::new(),
                })),
            ),
        }
    }

    async fn control_plane_status(&self) -> Vec<ControlPlaneStatus> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        if self.runtime_is_memory_test() {
            return Vec::new();
        }
        let node_rpc_health_path = self
            .network_dir(&active.config.name.0)
            .join(crate::ipc::nats_listener::NATS_NODE_RPC_HEALTH_FILE);
        let cert_renewal_health_path = self
            .network_dir(&active.config.name.0)
            .join(crate::daemon::cert_renewal_health::NATS_CERT_RENEWAL_HEALTH_FILE);
        let network_dir = self.network_dir(&active.config.name.0);
        let mut status = Vec::new();
        match crate::ipc::nats_listener::load_health(node_rpc_health_path).await {
            Ok(health) => status.push(component_health_status("node_rpc_listener", &health)),
            Err(error) => status.push(ControlPlaneStatus {
                component: String::from("node_rpc_listener"),
                state: ControlPlaneHealthState::Unknown {
                    error: format!("read listener health: {error}"),
                },
            }),
        }
        match crate::daemon::cert_renewal_health::load_health(cert_renewal_health_path).await {
            Ok(health) => status.push(component_health_status("cert_renewal_worker", &health)),
            Err(error) => status.push(ControlPlaneStatus {
                component: String::from("cert_renewal_worker"),
                state: ControlPlaneHealthState::Unknown {
                    error: format!("read cert renewal health: {error}"),
                },
            }),
        }
        match crate::mesh_state::bootstrap::load_bootstrap_peer_seed_health(&network_dir) {
            Ok(Some(health)) => {
                status.push(component_health_status("bootstrap_peer_seed", &health))
            }
            Ok(None) => status.push(ControlPlaneStatus {
                component: String::from("bootstrap_peer_seed"),
                state: ControlPlaneHealthState::Unknown {
                    error: String::from("bootstrap peer seed health file missing"),
                },
            }),
            Err(error) => status.push(ControlPlaneStatus {
                component: String::from("bootstrap_peer_seed"),
                state: ControlPlaneHealthState::Unknown {
                    error: format!("read bootstrap peer seed health: {error}"),
                },
            }),
        }
        for health in active.mesh.task_health() {
            status.push(ControlPlaneStatus {
                component: health.name,
                state: match health.state {
                    ployz_orchestrator::mesh::tasks::MeshTaskHealthState::Healthy => {
                        ControlPlaneHealthState::Healthy
                    }
                    ployz_orchestrator::mesh::tasks::MeshTaskHealthState::Stale {
                        stale_since_unix_secs,
                        consecutive_failures,
                        last_error,
                    } => ControlPlaneHealthState::Stale {
                        stale_since_unix_secs,
                        consecutive_failures,
                        error: last_error,
                    },
                },
            });
        }
        status
    }

    async fn edge_sync_status(&self) -> Vec<EdgeSyncStatus> {
        let mut status = Vec::new();
        if let Some(addr) = self.gateway_metrics_listen_addr.as_deref() {
            let metrics = fetch_metrics(addr).await;
            for stream in ["routing", "certificates", "acme_challenges"] {
                status.push(metric_status(
                    "gateway",
                    stream,
                    "ployz_gateway_store_sync_healthy",
                    "ployz_gateway_store_sync_state_since_unix_seconds",
                    "ployz_gateway_store_sync_failures_total",
                    metrics.as_ref(),
                ));
            }
        }
        if let Some(addr) = self.dns_metrics_listen_addr.as_deref() {
            let metrics = fetch_metrics(addr).await;
            status.push(metric_status(
                "dns",
                "routing",
                "ployz_dns_store_sync_healthy",
                "ployz_dns_store_sync_state_since_unix_seconds",
                "ployz_dns_store_sync_failures_total",
                metrics.as_ref(),
            ));
        }
        status
    }

    async fn nats_asset_status(&self) -> Vec<NatsAssetStatus> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        let client_url = if self.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(active.config.overlay_ip)
        };
        match tokio::time::timeout(
            NATS_ASSET_PROBE_TIMEOUT,
            read_nats_asset_status(&client_url, &active.config),
        )
        .await
        {
            Ok(assets) => assets,
            Err(_) => {
                nats_asset_probe_error(format!("nats asset probe timed out for {client_url}"))
            }
        }
    }
}

fn component_health_status(
    component: impl Into<String>,
    health: &crate::health::ComponentHealth,
) -> ControlPlaneStatus {
    let state = match &health.state {
        crate::health::ComponentHealthState::Healthy => ControlPlaneHealthState::Healthy,
        crate::health::ComponentHealthState::Stale {
            stale_since_unix_secs,
            consecutive_failures,
            last_error,
        } => ControlPlaneHealthState::Stale {
            stale_since_unix_secs: *stale_since_unix_secs,
            consecutive_failures: *consecutive_failures,
            error: last_error.clone(),
        },
    };
    ControlPlaneStatus {
        component: component.into(),
        state,
    }
}

async fn read_nats_asset_status(
    client_url: &str,
    config: &crate::mesh_state::network::NetworkConfig,
) -> Vec<NatsAssetStatus> {
    let scope =
        ployz_nats::NatsScope::local_for_storage_participation(&config.storage_participation);
    let store = match NatsStore::connect_with_scope(client_url, scope).await {
        Ok(store) => store,
        Err(error) => return nats_asset_probe_error(error.to_string()),
    };
    let mut status = Vec::new();
    for asset in store.asset_manifest() {
        status.push(read_stream_status(&store, &asset).await);
    }
    status
}

async fn read_stream_status(store: &NatsStore, asset: &NatsAssetSpec) -> NatsAssetStatus {
    match store.asset_stream_info(asset).await {
        Ok(info) => nats_asset_status_from_info(store, asset, &info),
        Err(error) => nats_asset_status(
            store,
            asset,
            NatsAssetHealthState::Unknown {
                error: error.to_string(),
            },
        ),
    }
}

fn nats_asset_status_from_info(
    store: &NatsStore,
    asset: &NatsAssetSpec,
    info: &async_nats::jetstream::stream::Info,
) -> NatsAssetStatus {
    let replica_status = NatsAssetReplicaStatus {
        replicas: info.config.num_replicas,
        current_replicas: nats_current_replicas(info.config.num_replicas, info.cluster.as_ref()),
        offline_replicas: nats_offline_replicas(info.config.num_replicas, info.cluster.as_ref()),
        max_lag: nats_max_lag(info.cluster.as_ref()),
        leader: info
            .cluster
            .as_ref()
            .and_then(|cluster| cluster.leader.clone()),
    };
    let state = if nats_asset_is_healthy(&replica_status) {
        NatsAssetHealthState::Healthy(replica_status)
    } else {
        NatsAssetHealthState::Stale(replica_status)
    };
    nats_asset_status(store, asset, state)
}

fn nats_asset_status(
    store: &NatsStore,
    asset: &NatsAssetSpec,
    state: NatsAssetHealthState,
) -> NatsAssetStatus {
    NatsAssetStatus {
        name: asset.name.clone(),
        kind: asset.kind.to_string(),
        data_bucket: asset.data_bucket,
        loss_impact: asset.loss_impact,
        installation: Some(store.installation().to_string()),
        authority: nats_asset_authority(store, asset.scope),
        domain: Some(nats_asset_domain(store, asset.scope)),
        scope: Some(asset.scope.as_str().to_string()),
        state,
    }
}

fn nats_asset_authority(store: &NatsStore, scope: NatsAssetScope) -> Option<String> {
    match scope {
        NatsAssetScope::AuthorityLocal => Some(store.authority().to_string()),
        NatsAssetScope::InstallationRoot => None,
    }
}

fn nats_asset_domain(store: &NatsStore, scope: NatsAssetScope) -> String {
    match scope {
        NatsAssetScope::AuthorityLocal | NatsAssetScope::InstallationRoot => {
            store.asset_domain(scope)
        }
    }
}

fn nats_asset_is_healthy(status: &NatsAssetReplicaStatus) -> bool {
    status.current_replicas == status.replicas
        && status.offline_replicas == 0
        && status.max_lag == 0
}

fn nats_current_replicas(replicas: usize, cluster: Option<&ClusterInfo>) -> usize {
    if replicas <= 1 && cluster.is_none() {
        return 1;
    }
    let Some(cluster) = cluster else {
        return 0;
    };
    usize::from(cluster.leader.is_some())
        + cluster
            .replicas
            .iter()
            .filter(|replica| replica.current && !replica.offline)
            .count()
}

fn nats_offline_replicas(replicas: usize, cluster: Option<&ClusterInfo>) -> usize {
    if replicas <= 1 && cluster.is_none() {
        return 0;
    }
    let Some(cluster) = cluster else {
        return replicas;
    };
    let known_offline = cluster
        .replicas
        .iter()
        .filter(|replica| replica.offline)
        .count();
    let leader_missing = usize::from(cluster.leader.is_none());
    let known_replicas = 1 + cluster.replicas.len();
    known_offline + leader_missing + replicas.saturating_sub(known_replicas)
}

fn nats_max_lag(cluster: Option<&ClusterInfo>) -> u64 {
    cluster
        .map(|cluster| {
            cluster
                .replicas
                .iter()
                .filter_map(|replica| replica.lag)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn nats_asset_probe_error(error: String) -> Vec<NatsAssetStatus> {
    vec![NatsAssetStatus {
        name: String::from("hub"),
        kind: String::from("connection"),
        data_bucket: ControlPlaneDataBucket::HealthMetrics,
        loss_impact: ControlPlaneLossImpact::Unknown,
        installation: Some(String::from("local")),
        authority: Some(String::from("auth-default")),
        domain: Some(String::from("dom-auth-default")),
        scope: Some(String::from("authority_local")),
        state: NatsAssetHealthState::Unknown { error },
    }]
}

async fn fetch_metrics(addr: &str) -> Result<String, String> {
    tokio::time::timeout(EDGE_SYNC_PROBE_TIMEOUT, fetch_metrics_inner(addr))
        .await
        .map_err(|_| format!("metrics probe timed out for {addr}"))?
}

async fn fetch_metrics_inner(addr: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|error| format!("connect metrics endpoint {addr}: {error}"))?;
    let request = format!("GET /metrics HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("write metrics request to {addr}: {error}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(|error| format!("read metrics response from {addr}: {error}"))?;
    let response = String::from_utf8(response)
        .map_err(|error| format!("metrics response from {addr} was not UTF-8: {error}"))?;
    let Some((headers, body)) = response.split_once("\r\n\r\n") else {
        return Err(format!(
            "metrics response from {addr} did not contain headers"
        ));
    };
    if !headers.starts_with("HTTP/1.1 200") && !headers.starts_with("HTTP/1.0 200") {
        let status = headers.lines().next().unwrap_or("unknown status");
        return Err(format!("metrics endpoint {addr} returned {status}"));
    }
    Ok(body.to_string())
}

fn metric_status(
    service: &str,
    stream: &str,
    healthy_metric: &str,
    state_since_metric: &str,
    failures_metric: &str,
    metrics: Result<&String, &String>,
) -> EdgeSyncStatus {
    match metrics {
        Ok(body) => match parse_sync_metric(body, healthy_metric, stream) {
            SyncMetric::Present(healthy) => {
                let state_since = parse_sync_metric_u64(body, state_since_metric, stream);
                let failures_total =
                    parse_sync_metric_u64(body, failures_metric, stream).unwrap_or(0);
                let state = if healthy {
                    EdgeSyncHealthState::Healthy { failures_total }
                } else {
                    EdgeSyncHealthState::Stale {
                        stale_since_unix_secs: state_since.unwrap_or(0),
                        failures_total,
                    }
                };
                EdgeSyncStatus {
                    service: service.to_string(),
                    stream: stream.to_string(),
                    state,
                }
            }
            SyncMetric::Missing => EdgeSyncStatus {
                service: service.to_string(),
                stream: stream.to_string(),
                state: EdgeSyncHealthState::Unknown {
                    error: format!("metric {healthy_metric} for stream {stream} was absent"),
                    failures_total: parse_sync_metric_u64(body, failures_metric, stream),
                },
            },
            SyncMetric::Invalid(value) => EdgeSyncStatus {
                service: service.to_string(),
                stream: stream.to_string(),
                state: EdgeSyncHealthState::Unknown {
                    error: format!(
                        "metric {healthy_metric} for stream {stream} had invalid boolean value {value}"
                    ),
                    failures_total: parse_sync_metric_u64(body, failures_metric, stream),
                },
            },
        },
        Err(error) => EdgeSyncStatus {
            service: service.to_string(),
            stream: stream.to_string(),
            state: EdgeSyncHealthState::Unknown {
                error: error.clone(),
                failures_total: None,
            },
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SyncMetric {
    Present(bool),
    Missing,
    Invalid(f64),
}

fn parse_sync_metric(metrics: &str, metric: &str, stream: &str) -> SyncMetric {
    let Some(value) = parse_sync_metric_value(metrics, metric, stream) else {
        return SyncMetric::Missing;
    };
    if (value - 1.0).abs() < f64::EPSILON {
        SyncMetric::Present(true)
    } else if value.abs() < f64::EPSILON {
        SyncMetric::Present(false)
    } else {
        SyncMetric::Invalid(value)
    }
}

fn parse_sync_metric_u64(metrics: &str, metric: &str, stream: &str) -> Option<u64> {
    let value = parse_sync_metric_value(metrics, metric, stream)?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(value as u64)
}

fn parse_sync_metric_value(metrics: &str, metric: &str, stream: &str) -> Option<f64> {
    let prefix = format!("{metric}{{");
    let stream_label = format!("stream=\"{stream}\"");
    for line in metrics.lines() {
        let line = line.trim();
        if !line.starts_with(&prefix) || !line.contains(&stream_label) {
            continue;
        }
        let value = line.rsplit_once(' ')?.1;
        return value.parse::<f64>().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        ControlPlaneDataBucket, ControlPlaneLossImpact, SyncMetric, component_health_status,
        metric_status, nats_asset_is_healthy, nats_asset_probe_error, nats_current_replicas,
        nats_max_lag, nats_offline_replicas, parse_sync_metric, parse_sync_metric_u64,
    };
    use crate::health::ComponentHealth;
    use async_nats::jetstream::stream::{ClusterInfo, PeerInfo};
    use ployz_api::{
        ControlPlaneHealthState, EdgeSyncHealthState, NatsAssetHealthState, NatsAssetReplicaStatus,
    };
    use std::time::Duration;

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
    fn nats_asset_probe_error_preserves_operator_context() {
        let [status] = nats_asset_probe_error(String::from("connect failed"))
            .try_into()
            .expect("probe error returns one status entry");

        assert_eq!(status.name, "hub");
        assert_eq!(status.kind, "connection");
        assert_eq!(status.data_bucket, ControlPlaneDataBucket::HealthMetrics);
        assert_eq!(status.loss_impact, ControlPlaneLossImpact::Unknown);
        assert_eq!(status.installation.as_deref(), Some("local"));
        assert_eq!(status.authority.as_deref(), Some("auth-default"));
        assert_eq!(status.domain.as_deref(), Some("dom-auth-default"));
        assert_eq!(status.scope.as_deref(), Some("authority_local"));
        match status.state {
            NatsAssetHealthState::Unknown { error } => assert_eq!(error, "connect failed"),
            other => panic!("expected unknown NATS asset state, got {other:?}"),
        }
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
}

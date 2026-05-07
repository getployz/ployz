use std::time::Duration;

use async_nats::jetstream::stream::ClusterInfo;
use ployz_api::{
    ControlPlaneStatus, DaemonPayload, DaemonResponse, EdgeSyncStatus, NatsAssetStatus,
    StatusPayload,
};
use ployz_config::RuntimeTarget;
use ployz_nats::NatsStore;
use ployz_nats::buckets::{NATS_KV_ASSETS, NATS_STREAM_ASSETS, NatsAssetRole, NatsAssetSpec};
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
                healthy: None,
                stale_since_unix_secs: None,
                consecutive_failures: None,
                error: Some(format!("read listener health: {error}")),
            }),
        }
        match crate::daemon::cert_renewal_health::load_health(cert_renewal_health_path).await {
            Ok(health) => status.push(component_health_status("cert_renewal_worker", &health)),
            Err(error) => status.push(ControlPlaneStatus {
                component: String::from("cert_renewal_worker"),
                healthy: None,
                stale_since_unix_secs: None,
                consecutive_failures: None,
                error: Some(format!("read cert renewal health: {error}")),
            }),
        }
        match crate::mesh_state::bootstrap::load_bootstrap_seed_cache_health(&network_dir) {
            Ok(Some(health)) => status.push(ControlPlaneStatus {
                component: String::from("bootstrap_seed_cache"),
                healthy: Some(health.is_healthy()),
                stale_since_unix_secs: health.stale_since_unix_secs(),
                consecutive_failures: Some(health.consecutive_failures()),
                error: health.last_error().map(String::from),
            }),
            Ok(None) => status.push(ControlPlaneStatus {
                component: String::from("bootstrap_seed_cache"),
                healthy: None,
                stale_since_unix_secs: None,
                consecutive_failures: None,
                error: Some(String::from("bootstrap seed cache health file missing")),
            }),
            Err(error) => status.push(ControlPlaneStatus {
                component: String::from("bootstrap_seed_cache"),
                healthy: None,
                stale_since_unix_secs: None,
                consecutive_failures: None,
                error: Some(format!("read bootstrap seed cache health: {error}")),
            }),
        }
        for health in active.mesh.task_health() {
            let healthy = health.is_healthy();
            let stale_since_unix_secs = health.stale_since_unix_secs();
            let consecutive_failures = health.consecutive_failures();
            let error = health.last_error().map(String::from);
            status.push(ControlPlaneStatus {
                component: health.name,
                healthy: Some(healthy),
                stale_since_unix_secs,
                consecutive_failures: Some(consecutive_failures),
                error,
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
            read_nats_asset_status(&client_url),
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
    ControlPlaneStatus {
        component: component.into(),
        healthy: Some(health.is_healthy()),
        stale_since_unix_secs: health.stale_since_unix_secs(),
        consecutive_failures: Some(health.consecutive_failures()),
        error: health.last_error().map(String::from),
    }
}

async fn read_nats_asset_status(client_url: &str) -> Vec<NatsAssetStatus> {
    let store = match NatsStore::connect(client_url).await {
        Ok(store) => store,
        Err(error) => return nats_asset_probe_error(error.to_string()),
    };
    let mut status = Vec::new();
    for asset in NATS_STREAM_ASSETS {
        status.push(read_stream_status(&store, asset).await);
    }
    for asset in NATS_KV_ASSETS {
        status.push(read_stream_status(&store, asset).await);
    }
    status
}

async fn read_stream_status(store: &NatsStore, asset: &NatsAssetSpec) -> NatsAssetStatus {
    let stream = match asset.kind {
        "kv" => format!("KV_{}", asset.name),
        _ => asset.name.to_string(),
    };
    match store.jetstream().get_stream(stream.as_str()).await {
        Ok(mut stream_handle) => match stream_handle.info().await {
            Ok(info) => NatsAssetStatus {
                name: asset.name.to_string(),
                kind: asset.kind.to_string(),
                installation: Some(store.scope().installation.to_string()),
                authority: nats_asset_authority(store, asset.role),
                domain: Some(nats_asset_domain(store, asset.role)),
                role: Some(asset.role.as_str().to_string()),
                replicas: Some(info.config.num_replicas),
                healthy: Some(nats_asset_is_healthy(
                    info.config.num_replicas,
                    info.cluster.as_ref(),
                )),
                current_replicas: Some(nats_current_replicas(
                    info.config.num_replicas,
                    info.cluster.as_ref(),
                )),
                offline_replicas: Some(nats_offline_replicas(
                    info.config.num_replicas,
                    info.cluster.as_ref(),
                )),
                max_lag: Some(nats_max_lag(info.cluster.as_ref())),
                leader: info
                    .cluster
                    .as_ref()
                    .and_then(|cluster| cluster.leader.clone()),
                error: None,
            },
            Err(error) => NatsAssetStatus {
                name: asset.name.to_string(),
                kind: asset.kind.to_string(),
                installation: Some(store.scope().installation.to_string()),
                authority: nats_asset_authority(store, asset.role),
                domain: Some(nats_asset_domain(store, asset.role)),
                role: Some(asset.role.as_str().to_string()),
                replicas: None,
                healthy: None,
                current_replicas: None,
                offline_replicas: None,
                max_lag: None,
                leader: None,
                error: Some(format!("{error:?}")),
            },
        },
        Err(error) => NatsAssetStatus {
            name: asset.name.to_string(),
            kind: asset.kind.to_string(),
            installation: Some(store.scope().installation.to_string()),
            authority: nats_asset_authority(store, asset.role),
            domain: Some(nats_asset_domain(store, asset.role)),
            role: Some(asset.role.as_str().to_string()),
            replicas: None,
            healthy: None,
            current_replicas: None,
            offline_replicas: None,
            max_lag: None,
            leader: None,
            error: Some(format!("{error:?}")),
        },
    }
}

fn nats_asset_authority(store: &NatsStore, role: NatsAssetRole) -> Option<String> {
    match role {
        NatsAssetRole::AuthorityLocal => Some(store.scope().authority.to_string()),
        NatsAssetRole::InstallationRoot => None,
    }
}

fn nats_asset_domain(store: &NatsStore, role: NatsAssetRole) -> String {
    match role {
        NatsAssetRole::AuthorityLocal => store.scope().authority_domain(),
        NatsAssetRole::InstallationRoot => store.scope().root_domain(),
    }
}

fn nats_asset_is_healthy(replicas: usize, cluster: Option<&ClusterInfo>) -> bool {
    nats_current_replicas(replicas, cluster) == replicas
        && nats_offline_replicas(replicas, cluster) == 0
        && nats_max_lag(cluster) == 0
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
        installation: Some(String::from("local")),
        authority: Some(String::from("auth-default")),
        domain: Some(String::from("dom-auth-default")),
        role: Some(String::from("authority_local")),
        replicas: None,
        healthy: None,
        current_replicas: None,
        offline_replicas: None,
        max_lag: None,
        leader: None,
        error: Some(error),
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
            Some(healthy) => {
                let state_since = parse_sync_metric_u64(body, state_since_metric, stream);
                EdgeSyncStatus {
                    service: service.to_string(),
                    stream: stream.to_string(),
                    healthy: Some(healthy),
                    stale_since_unix_secs: if healthy { None } else { state_since },
                    failures_total: parse_sync_metric_u64(body, failures_metric, stream),
                    error: None,
                }
            }
            None => EdgeSyncStatus {
                service: service.to_string(),
                stream: stream.to_string(),
                healthy: None,
                stale_since_unix_secs: None,
                failures_total: parse_sync_metric_u64(body, failures_metric, stream),
                error: Some(format!(
                    "metric {healthy_metric} for stream {stream} was absent"
                )),
            },
        },
        Err(error) => EdgeSyncStatus {
            service: service.to_string(),
            stream: stream.to_string(),
            healthy: None,
            stale_since_unix_secs: None,
            failures_total: None,
            error: Some(error.clone()),
        },
    }
}

fn parse_sync_metric(metrics: &str, metric: &str, stream: &str) -> Option<bool> {
    let value = parse_sync_metric_value(metrics, metric, stream)?;
    if (value - 1.0).abs() < f64::EPSILON {
        Some(true)
    } else if value.abs() < f64::EPSILON {
        Some(false)
    } else {
        None
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
        component_health_status, metric_status, nats_asset_is_healthy, nats_current_replicas,
        nats_max_lag, nats_offline_replicas, parse_sync_metric, parse_sync_metric_u64,
    };
    use crate::health::ComponentHealth;
    use async_nats::jetstream::stream::{ClusterInfo, PeerInfo};
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
            Some(true)
        );
        assert_eq!(
            parse_sync_metric(metrics, "ployz_gateway_store_sync_healthy", "certificates"),
            Some(false)
        );
        assert_eq!(
            parse_sync_metric(
                metrics,
                "ployz_gateway_store_sync_healthy",
                "acme_challenges"
            ),
            None
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
        assert_eq!(status.healthy, None);
        assert_eq!(status.failures_total, Some(4));
        assert!(
            status
                .error
                .as_deref()
                .is_some_and(|error| error.contains("was absent"))
        );
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
        assert_eq!(status.healthy, Some(false));
        assert_eq!(status.stale_since_unix_secs, Some(1_777_646_000));
        assert_eq!(status.failures_total, Some(7));
        assert_eq!(status.error, None);
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
        assert_eq!(status.healthy, None);
        assert_eq!(status.stale_since_unix_secs, None);
        assert_eq!(status.failures_total, None);
        assert_eq!(status.error.as_deref(), Some("connection refused"));
    }

    #[test]
    fn component_health_status_preserves_stale_failure_details() {
        let first = ComponentHealth::stale(1_777_646_000, None, "subscription closed");
        let health = ComponentHealth::stale(1_777_646_100, Some(&first), "ack failed");

        let status = component_health_status("node_rpc_listener", &health);

        assert_eq!(status.component, "node_rpc_listener");
        assert_eq!(status.healthy, Some(false));
        assert_eq!(status.stale_since_unix_secs, Some(1_777_646_000));
        assert_eq!(status.consecutive_failures, Some(2));
        assert_eq!(status.error.as_deref(), Some("ack failed"));
    }

    #[test]
    fn nats_asset_health_treats_single_replica_without_cluster_as_current() {
        assert!(nats_asset_is_healthy(1, None));
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

        assert!(nats_asset_is_healthy(3, Some(&cluster)));
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

        assert!(!nats_asset_is_healthy(3, Some(&cluster)));
        assert_eq!(nats_current_replicas(3, Some(&cluster)), 1);
        assert_eq!(nats_offline_replicas(3, Some(&cluster)), 1);
        assert_eq!(nats_max_lag(Some(&cluster)), 44);
    }

    #[test]
    fn nats_asset_health_reports_missing_cluster_for_replicated_asset() {
        assert!(!nats_asset_is_healthy(3, None));
        assert_eq!(nats_current_replicas(3, None), 0);
        assert_eq!(nats_offline_replicas(3, None), 3);
        assert_eq!(nats_max_lag(None), 0);
    }
}

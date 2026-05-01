use std::time::Duration;

use ployz_api::{
    ControlPlaneStatus, DaemonPayload, DaemonResponse, EdgeSyncStatus, NatsAssetStatus,
    StatusPayload,
};
use ployz_config::RuntimeTarget;
use ployz_nats::NatsStore;
use ployz_nats::buckets::{
    ACME_ACCOUNTS_BUCKET, ACME_CHALLENGE_READINESS_BUCKET, ACME_CHALLENGES_BUCKET,
    CERTIFICATES_BUCKET, COORDINATOR_LEASE_BUCKET, DEPLOY_STATUS_BUCKET, INSTANCES_BUCKET,
    INVITES_BUCKET, LOCKS_BUCKET, MACHINES_BUCKET,
};
use ployz_nats::subjects::{
    CERT_JOBS_STREAM, DEPLOY_COMMITS_STREAM, REVISIONS_STREAM, ROUTING_EVENTS_STREAM,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::super::DaemonState;

const EDGE_SYNC_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const NATS_ASSET_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const NATS_STREAM_ASSETS: &[&str] = &[
    DEPLOY_COMMITS_STREAM,
    ROUTING_EVENTS_STREAM,
    REVISIONS_STREAM,
    CERT_JOBS_STREAM,
];
const NATS_KV_ASSETS: &[&str] = &[
    MACHINES_BUCKET,
    INVITES_BUCKET,
    DEPLOY_STATUS_BUCKET,
    INSTANCES_BUCKET,
    ACME_ACCOUNTS_BUCKET,
    CERTIFICATES_BUCKET,
    ACME_CHALLENGES_BUCKET,
    ACME_CHALLENGE_READINESS_BUCKET,
    LOCKS_BUCKET,
    COORDINATOR_LEASE_BUCKET,
];

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
            Ok(health) => status.push(ControlPlaneStatus {
                component: String::from("node_rpc_listener"),
                healthy: Some(health.healthy),
                stale_since_unix_secs: health.stale_since_unix_secs,
                consecutive_failures: Some(health.consecutive_failures),
                error: health.last_error,
            }),
            Err(error) => status.push(ControlPlaneStatus {
                component: String::from("node_rpc_listener"),
                healthy: None,
                stale_since_unix_secs: None,
                consecutive_failures: None,
                error: Some(format!("read listener health: {error}")),
            }),
        }
        match crate::daemon::cert_renewal_health::load_health(cert_renewal_health_path).await {
            Ok(health) => status.push(ControlPlaneStatus {
                component: String::from("cert_renewal_worker"),
                healthy: Some(health.healthy),
                stale_since_unix_secs: health.stale_since_unix_secs,
                consecutive_failures: Some(health.consecutive_failures),
                error: health.last_error,
            }),
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
                healthy: Some(health.healthy),
                stale_since_unix_secs: health.stale_since_unix_secs,
                consecutive_failures: Some(health.consecutive_failures),
                error: health.last_error,
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
            status.push(ControlPlaneStatus {
                component: health.name,
                healthy: Some(health.healthy),
                stale_since_unix_secs: health.stale_since_unix_secs,
                consecutive_failures: Some(health.consecutive_failures),
                error: health.last_error,
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

async fn read_nats_asset_status(client_url: &str) -> Vec<NatsAssetStatus> {
    let store = match NatsStore::connect(client_url).await {
        Ok(store) => store,
        Err(error) => return nats_asset_probe_error(error.to_string()),
    };
    let mut status = Vec::new();
    for stream in NATS_STREAM_ASSETS {
        status.push(read_stream_status(&store, stream, "stream").await);
    }
    for bucket in NATS_KV_ASSETS {
        let stream = format!("KV_{bucket}");
        status.push(read_stream_status(&store, &stream, "kv").await);
    }
    status
}

async fn read_stream_status(store: &NatsStore, stream: &str, kind: &str) -> NatsAssetStatus {
    match store.jetstream().get_stream(stream).await {
        Ok(mut stream_handle) => match stream_handle.info().await {
            Ok(info) => NatsAssetStatus {
                name: stream.to_string(),
                kind: kind.to_string(),
                replicas: Some(info.config.num_replicas),
                error: None,
            },
            Err(error) => NatsAssetStatus {
                name: stream.to_string(),
                kind: kind.to_string(),
                replicas: None,
                error: Some(format!("{error:?}")),
            },
        },
        Err(error) => NatsAssetStatus {
            name: stream.to_string(),
            kind: kind.to_string(),
            replicas: None,
            error: Some(format!("{error:?}")),
        },
    }
}

fn nats_asset_probe_error(error: String) -> Vec<NatsAssetStatus> {
    vec![NatsAssetStatus {
        name: String::from("hub"),
        kind: String::from("connection"),
        replicas: None,
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
    use super::{parse_sync_metric, parse_sync_metric_u64};

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
}

use crate::daemon::DaemonState;
use ployz_api::{EdgeSyncHealthState, EdgeSyncStatus};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const EDGE_SYNC_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

impl DaemonState {
    pub(super) async fn edge_sync_status(&self) -> Vec<EdgeSyncStatus> {
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

pub(super) fn metric_status(
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
pub(super) enum SyncMetric {
    Present(bool),
    Missing,
    Invalid(f64),
}

pub(super) fn parse_sync_metric(metrics: &str, metric: &str, stream: &str) -> SyncMetric {
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

pub(super) fn parse_sync_metric_u64(metrics: &str, metric: &str, stream: &str) -> Option<u64> {
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

use std::sync::OnceLock;
use std::time::Duration;

use ployz_metrics::register_metric;
use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts};

use crate::routes::GatewaySnapshot;

static GATEWAY_REQUESTS_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();
static GATEWAY_REQUEST_DURATION: OnceLock<HistogramVec> = OnceLock::new();
static GATEWAY_ROUTES: OnceLock<IntGaugeVec> = OnceLock::new();
static GATEWAY_STORE_SYNC_HEALTHY: OnceLock<IntGaugeVec> = OnceLock::new();

#[cfg(test)]
pub(crate) static ROUTE_METRICS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn observe_request(method: &str, status_code: u16, matched: bool, duration: Duration) {
    let status_class = status_class_label(status_code);
    let matched = bool_label(matched);

    register_metric(&GATEWAY_REQUESTS_TOTAL, || {
        let metric = IntCounterVec::new(
            Opts::new(
                "ployz_gateway_requests_total",
                "Total HTTP requests handled by ployz-gateway.",
            ),
            &["method", "status_class", "matched"],
        )
        .expect("gateway request counter should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("gateway request counter should register");
        metric
    })
    .with_label_values(&[method, status_class, matched])
    .inc();

    register_metric(&GATEWAY_REQUEST_DURATION, || {
        let metric = HistogramVec::new(
            HistogramOpts::new(
                "ployz_gateway_request_duration_seconds",
                "Latency of HTTP requests handled by ployz-gateway.",
            ),
            &["method", "status_class", "matched"],
        )
        .expect("gateway request histogram should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("gateway request histogram should register");
        metric
    })
    .with_label_values(&[method, status_class, matched])
    .observe(duration.as_secs_f64());
}

pub fn update_route_counts(snapshot: &GatewaySnapshot) {
    update_route_count_values(snapshot.http_routes.len(), snapshot.tcp_routes.len());
}

pub fn update_route_count_values(http_routes: usize, tcp_routes: usize) {
    let metric = register_metric(&GATEWAY_ROUTES, || {
        let metric = IntGaugeVec::new(
            Opts::new(
                "ployz_gateway_routes",
                "Current number of projected routes in ployz-gateway.",
            ),
            &["protocol"],
        )
        .expect("gateway route gauge should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("gateway route gauge should register");
        metric
    });
    let http_count = i64::try_from(http_routes).unwrap_or(i64::MAX);
    let tcp_count = i64::try_from(tcp_routes).unwrap_or(i64::MAX);
    metric.with_label_values(&["http"]).set(http_count);
    metric.with_label_values(&["tcp"]).set(tcp_count);
}

pub fn set_store_sync_healthy(stream: &str, healthy: bool) {
    let metric = register_metric(&GATEWAY_STORE_SYNC_HEALTHY, || {
        let metric = IntGaugeVec::new(
            Opts::new(
                "ployz_gateway_store_sync_healthy",
                "Whether ployz-gateway store subscriptions are current.",
            ),
            &["stream"],
        )
        .expect("gateway store sync gauge should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("gateway store sync gauge should register");
        metric
    });
    metric
        .with_label_values(&[stream])
        .set(if healthy { 1 } else { 0 });
}

fn bool_label(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn status_class_label(status_code: u16) -> &'static str {
    match status_code {
        200..=299 => "2xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "0xx",
    }
}

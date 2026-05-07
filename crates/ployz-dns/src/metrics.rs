use std::sync::OnceLock;
use std::time::Duration;

use hickory_server::proto::op::ResponseCode;
use ployz_metrics::register_metric;
use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static DNS_QUERIES_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();
static DNS_QUERY_DURATION: OnceLock<HistogramVec> = OnceLock::new();
static DNS_STORE_SYNC_HEALTHY: OnceLock<IntGaugeVec> = OnceLock::new();
static DNS_STORE_SYNC_STATE_SINCE: OnceLock<IntGaugeVec> = OnceLock::new();
static DNS_STORE_SYNC_FAILURES: OnceLock<IntCounterVec> = OnceLock::new();
static DNS_STORE_SYNC_STATE: OnceLock<Mutex<HashMap<String, StoreSyncState>>> = OnceLock::new();

pub fn observe_query(qtype: &str, response_code: ResponseCode, duration: Duration) {
    let response_code = response_code_label(response_code);

    register_metric(&DNS_QUERIES_TOTAL, || {
        let metric = IntCounterVec::new(
            Opts::new(
                "ployz_dns_queries_total",
                "Total DNS queries handled by ployz-dns.",
            ),
            &["qtype", "rcode"],
        )
        .expect("dns query counter should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("dns query counter should register");
        metric
    })
    .with_label_values(&[qtype, response_code])
    .inc();

    register_metric(&DNS_QUERY_DURATION, || {
        let metric = HistogramVec::new(
            HistogramOpts::new(
                "ployz_dns_query_duration_seconds",
                "Latency of DNS queries handled by ployz-dns.",
            ),
            &["qtype", "rcode"],
        )
        .expect("dns query histogram should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("dns query histogram should register");
        metric
    })
    .with_label_values(&[qtype, response_code])
    .observe(duration.as_secs_f64());
}

pub fn set_store_sync_healthy(stream: &str, healthy: bool) {
    let metric = register_metric(&DNS_STORE_SYNC_HEALTHY, || {
        let metric = IntGaugeVec::new(
            Opts::new(
                "ployz_dns_store_sync_healthy",
                "Whether ployz-dns store subscriptions are current.",
            ),
            &["stream"],
        )
        .expect("dns store sync gauge should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("dns store sync gauge should register");
        metric
    });
    metric
        .with_label_values(&[stream])
        .set(if healthy { 1 } else { 0 });

    let now = now_unix_secs();
    let since = update_store_sync_state(stream, healthy, now);
    register_metric(&DNS_STORE_SYNC_STATE_SINCE, || {
        let metric = IntGaugeVec::new(
            Opts::new(
                "ployz_dns_store_sync_state_since_unix_seconds",
                "Unix timestamp when the current ployz-dns store subscription health state began.",
            ),
            &["stream"],
        )
        .expect("dns store sync state-since gauge should be valid");
        prometheus::default_registry()
            .register(Box::new(metric.clone()))
            .expect("dns store sync state-since gauge should register");
        metric
    })
    .with_label_values(&[stream])
    .set(i64::try_from(since).unwrap_or(i64::MAX));
    if !healthy {
        register_metric(&DNS_STORE_SYNC_FAILURES, || {
            let metric = IntCounterVec::new(
                Opts::new(
                    "ployz_dns_store_sync_failures_total",
                    "Total ployz-dns store subscription setup, closure, and delivery failures.",
                ),
                &["stream"],
            )
            .expect("dns store sync failure counter should be valid");
            prometheus::default_registry()
                .register(Box::new(metric.clone()))
                .expect("dns store sync failure counter should register");
            metric
        })
        .with_label_values(&[stream])
        .inc();
    }
}

#[derive(Debug, Clone, Copy)]
struct StoreSyncState {
    healthy: bool,
    since_unix_secs: u64,
}

fn update_store_sync_state(stream: &str, healthy: bool, now: u64) -> u64 {
    let state = DNS_STORE_SYNC_STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut state = state.lock().expect("dns sync state lock poisoned");
    let entry = state.entry(stream.to_string()).or_insert(StoreSyncState {
        healthy,
        since_unix_secs: now,
    });
    if entry.healthy != healthy {
        *entry = StoreSyncState {
            healthy,
            since_unix_secs: now,
        };
    }
    entry.since_unix_secs
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn response_code_label(response_code: ResponseCode) -> &'static str {
    match response_code {
        ResponseCode::NoError => "NOERROR",
        ResponseCode::NXDomain => "NXDOMAIN",
        ResponseCode::FormErr => "FORMERR",
        ResponseCode::ServFail => "SERVFAIL",
        ResponseCode::NotImp
        | ResponseCode::Refused
        | ResponseCode::YXDomain
        | ResponseCode::YXRRSet
        | ResponseCode::NXRRSet
        | ResponseCode::NotAuth
        | ResponseCode::NotZone
        | ResponseCode::BADVERS
        | ResponseCode::BADSIG
        | ResponseCode::BADKEY
        | ResponseCode::BADTIME
        | ResponseCode::BADMODE
        | ResponseCode::BADNAME
        | ResponseCode::BADALG
        | ResponseCode::BADTRUNC
        | ResponseCode::BADCOOKIE
        | ResponseCode::Unknown(_) => "OTHER",
    }
}

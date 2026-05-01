use std::sync::OnceLock;
use std::time::Duration;

use hickory_server::proto::op::ResponseCode;
use ployz_metrics::register_metric;
use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts};

static DNS_QUERIES_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();
static DNS_QUERY_DURATION: OnceLock<HistogramVec> = OnceLock::new();
static DNS_STORE_SYNC_HEALTHY: OnceLock<IntGaugeVec> = OnceLock::new();

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

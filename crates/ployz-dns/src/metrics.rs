use std::sync::OnceLock;
use std::time::Duration;

use hickory_server::proto::op::ResponseCode;
use ployz_metrics::register_metric;
use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, Opts};

static DNS_QUERIES_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();
static DNS_QUERY_DURATION: OnceLock<HistogramVec> = OnceLock::new();

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

fn response_code_label(response_code: ResponseCode) -> &'static str {
    match response_code {
        ResponseCode::NoError => "NOERROR",
        ResponseCode::NXDomain => "NXDOMAIN",
        ResponseCode::FormErr => "FORMERR",
        ResponseCode::ServFail => "SERVFAIL",
        _ => "OTHER",
    }
}

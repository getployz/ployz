use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mvp_acme::AcmeKeyAuthorization;
use mvp_projection::{DnsRecordProjection, GatewayRouteProjection, ServingCertificateProjection};
use serde::{Deserialize, Serialize};

use crate::{ServingActorHandle, ServingResult, ServingStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRoleMetrics {
    pub request_count: u64,
    pub malformed_dns_count: u64,
    pub backend_failure_count: u64,
    pub latency_samples_us: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct WireMetricsRecorder {
    inner: Arc<WireMetricsInner>,
    max_latency_samples: usize,
}

#[derive(Debug)]
struct WireMetricsInner {
    request_count: AtomicU64,
    malformed_dns_count: AtomicU64,
    backend_failure_count: AtomicU64,
    latency_samples_us: Mutex<Vec<u64>>,
}

impl WireMetricsRecorder {
    #[must_use]
    pub fn new(max_latency_samples: usize) -> Self {
        Self {
            inner: Arc::new(WireMetricsInner {
                request_count: AtomicU64::new(0),
                malformed_dns_count: AtomicU64::new(0),
                backend_failure_count: AtomicU64::new(0),
                latency_samples_us: Mutex::new(Vec::new()),
            }),
            max_latency_samples,
        }
    }

    pub fn record_request(&self, elapsed: Duration) {
        let previous = self.inner.request_count.fetch_add(1, Ordering::Relaxed);
        if previous < self.max_latency_samples as u64 {
            let mut latency_samples = self
                .inner
                .latency_samples_us
                .lock()
                .expect("wire metrics latency lock poisoned");
            if latency_samples.len() < self.max_latency_samples {
                latency_samples.push(duration_to_us(elapsed));
            }
        }
    }

    pub fn record_malformed_dns(&self) {
        self.inner
            .malformed_dns_count
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_backend_failure(&self) {
        self.inner
            .backend_failure_count
            .fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> WireRoleMetrics {
        WireRoleMetrics {
            request_count: self.inner.request_count.load(Ordering::Relaxed),
            malformed_dns_count: self.inner.malformed_dns_count.load(Ordering::Relaxed),
            backend_failure_count: self.inner.backend_failure_count.load(Ordering::Relaxed),
            latency_samples_us: self
                .inner
                .latency_samples_us
                .lock()
                .expect("wire metrics latency lock poisoned")
                .clone(),
        }
    }
}

fn duration_to_us(duration: Duration) -> u64 {
    let micros = duration.as_micros();
    if micros > u128::from(u64::MAX) {
        return u64::MAX;
    }
    micros as u64
}

#[derive(Clone)]
pub struct WireServingState {
    serving: ServingActorHandle,
}

impl WireServingState {
    #[must_use]
    pub fn new(serving: ServingActorHandle) -> Self {
        Self { serving }
    }

    pub async fn gateway_route_for_host(
        &self,
        host: impl Into<String>,
    ) -> ServingResult<Option<GatewayRouteProjection>> {
        self.serving.gateway_route_for_host(host).await
    }

    pub async fn dns_records(
        &self,
        name: impl Into<String>,
        record_type: impl Into<String>,
    ) -> ServingResult<Vec<DnsRecordProjection>> {
        self.serving.dns_records(name, record_type).await
    }

    pub async fn service_dns_records(
        &self,
        name: impl Into<String>,
        record_type: impl Into<String>,
    ) -> ServingResult<Vec<DnsRecordProjection>> {
        self.serving.service_dns_records(name, record_type).await
    }

    pub async fn acme_http01_challenge(
        &self,
        host: impl Into<String>,
        token: impl Into<String>,
    ) -> ServingResult<Option<AcmeKeyAuthorization>> {
        self.serving.acme_http01_challenge(host, token).await
    }

    pub async fn certificate_for_host(
        &self,
        host: impl Into<String>,
    ) -> ServingResult<Option<ServingCertificateProjection>> {
        self.serving.certificate_for_host(host).await
    }

    pub async fn reload(&self) -> ServingResult<ServingStatus> {
        self.serving.reload().await
    }

    pub async fn status(&self) -> ServingResult<ServingStatus> {
        self.serving.status().await
    }
}

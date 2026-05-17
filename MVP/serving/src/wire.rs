use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mvp_projection::{DnsRecordProjection, GatewayRouteProjection};
use serde::{Deserialize, Serialize};

use crate::{ServingActorHandle, ServingResult, ServingStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRoleMetrics {
    pub request_count: u64,
    pub malformed_dns_count: u64,
    pub backend_failure_count: u64,
    pub latency_samples_us: Vec<u64>,
}

impl WireRoleMetrics {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            request_count: 0,
            malformed_dns_count: 0,
            backend_failure_count: 0,
            latency_samples_us: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WireMetricsRecorder {
    inner: Arc<Mutex<WireRoleMetrics>>,
    max_latency_samples: usize,
}

impl WireMetricsRecorder {
    #[must_use]
    pub fn new(max_latency_samples: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WireRoleMetrics::empty())),
            max_latency_samples,
        }
    }

    pub fn record_request(&self, elapsed: Duration) {
        let mut metrics = self.inner.lock().expect("wire metrics lock poisoned");
        metrics.request_count += 1;
        if metrics.latency_samples_us.len() < self.max_latency_samples {
            metrics.latency_samples_us.push(duration_to_us(elapsed));
        }
    }

    pub fn record_malformed_dns(&self) {
        let mut metrics = self.inner.lock().expect("wire metrics lock poisoned");
        metrics.malformed_dns_count += 1;
    }

    pub fn record_backend_failure(&self) {
        let mut metrics = self.inner.lock().expect("wire metrics lock poisoned");
        metrics.backend_failure_count += 1;
    }

    #[must_use]
    pub fn snapshot(&self) -> WireRoleMetrics {
        self.inner
            .lock()
            .expect("wire metrics lock poisoned")
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireRoleStatus {
    pub serving: ServingStatus,
    pub listen_addr: SocketAddr,
    pub metrics: WireRoleMetrics,
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

    pub async fn reload(&self) -> ServingResult<ServingStatus> {
        self.serving.reload().await
    }

    pub async fn status(&self) -> ServingResult<ServingStatus> {
        self.serving.status().await
    }
}

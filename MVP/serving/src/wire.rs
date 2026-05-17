use std::net::SocketAddr;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireRoleStatus {
    pub serving: ServingStatus,
    pub listen_addr: SocketAddr,
    pub metrics: WireRoleMetrics,
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

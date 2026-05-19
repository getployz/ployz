mod actor;
mod dns_server;
mod error;
mod gateway;
mod http_gateway;
mod model;
mod wire;

pub use actor::ServingActorHandle;
pub use dns_server::{DnsServerError, DnsServerHandle, DnsServerResult, spawn_dns_server};
pub use error::{ServingError, ServingResult};
pub use gateway::{GatewayEngineKind, GatewayHandle, GatewayOptions, spawn_gateway};
pub use http_gateway::{
    HttpGatewayError, HttpGatewayHandle, HttpGatewayResult, spawn_http_gateway,
};
pub use model::{
    ServingFailure, ServingFailureKind, ServingFreshness, ServingRevisions, ServingSnapshotBatch,
    ServingSnapshotKind, ServingSnapshotPaths, ServingStatus,
};
pub use wire::{WireMetricsRecorder, WireRoleMetrics, WireServingState};

#[cfg(test)]
mod tests;

//! Live machine and machine-role testimony contracts.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::ids::MachineName;

/// Gateway role status fact reported by the gateway process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct GatewayStatusObservation {
    pub machine_id: MachineName,
    pub listen_addr: SocketAddr,
    pub serving: GatewayServingStatus,
    pub route_count: usize,
    #[serde(default)]
    pub process_health: GatewayProcessHealth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum GatewayServingStatus {
    Current,
    LastKnownGood,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct GatewayProcessHealth {
    pub last_attempt: Option<GatewayProcessAttempt>,
    pub consecutive_failures: u64,
    pub last_http_failure: Option<GatewayHttpFailure>,
    pub consecutive_http_failures: u64,
    pub last_watch_failure: Option<GatewayWatchFailure>,
    pub consecutive_watch_failures: u64,
    pub last_status_publish_failure: Option<GatewayStatusPublishFailure>,
    pub consecutive_status_publish_failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum GatewayProcessAttempt {
    Current { route_count: usize },
    ServingLastKnownGood { route_count: usize, message: String },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "failure", rename_all = "snake_case", deny_unknown_fields)]
pub enum GatewayHttpFailure {
    Proxy { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "failure", rename_all = "snake_case", deny_unknown_fields)]
pub enum GatewayWatchFailure {
    Open { message: String },
    Ended { source: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "failure", rename_all = "snake_case", deny_unknown_fields)]
pub enum GatewayStatusPublishFailure {
    Write { message: String },
}

#[cfg(test)]
mod tests {
    use super::{GatewayProcessHealth, GatewayStatusObservation};

    #[test]
    fn gateway_status_defaults_health_for_older_payloads() {
        let observation = serde_json::from_value::<GatewayStatusObservation>(serde_json::json!({
            "machine_id": "machine_a",
            "listen_addr": "127.0.0.1:443",
            "serving": "current",
            "route_count": 2
        }))
        .expect("older gateway status payload remains readable");

        assert_eq!(observation.process_health, GatewayProcessHealth::default());
    }
}

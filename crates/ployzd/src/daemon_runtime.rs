//! Runtime dispatch for configured daemon processes.

use crate::config::DaemonProcessConfig;
use crate::control_runtime::{ControlRuntimeError, run_control_until_shutdown};
use crate::dns_process_runtime::{DnsProcessRuntimeError, run_dns_until_shutdown};
use crate::gateway_process_runtime::{GatewayProcessRuntimeError, run_gateway_until_shutdown};
use crate::node::process::{NodeProcessRuntimeError, run_node_until_shutdown};
use std::fmt;

pub async fn run_daemon_process_until_shutdown(
    config: &DaemonProcessConfig,
) -> Result<(), DaemonRuntimeError> {
    match config {
        DaemonProcessConfig::Control(config) => run_control_until_shutdown(config)
            .await
            .map_err(DaemonRuntimeError::Control),
        DaemonProcessConfig::Node(config) => run_node_until_shutdown(config)
            .await
            .map_err(DaemonRuntimeError::Node),
        DaemonProcessConfig::Gateway(config) => run_gateway_until_shutdown(config)
            .await
            .map_err(DaemonRuntimeError::Gateway),
        DaemonProcessConfig::Dns(config) => run_dns_until_shutdown(config)
            .await
            .map_err(DaemonRuntimeError::Dns),
    }
}

#[derive(Debug)]
pub enum DaemonRuntimeError {
    Control(ControlRuntimeError),
    Node(NodeProcessRuntimeError),
    Gateway(GatewayProcessRuntimeError),
    Dns(DnsProcessRuntimeError),
}

impl fmt::Display for DaemonRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Control(error) => write!(formatter, "{error}"),
            Self::Node(error) => write!(formatter, "{error}"),
            Self::Gateway(error) => write!(formatter, "{error}"),
            Self::Dns(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for DaemonRuntimeError {}

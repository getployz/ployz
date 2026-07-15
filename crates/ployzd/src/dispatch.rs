//! Dispatch for configured daemon role processes.

use crate::config::DaemonProcessConfig;
use crate::control::process::{ControlProcessError, run_control_until_shutdown};
use crate::roles::dns::process::{DnsProcessError, run_dns_until_shutdown};
use crate::roles::gateway::process::{GatewayProcessError, run_gateway_until_shutdown};
use crate::roles::machine::process::{MachineProcessError, run_machine_until_shutdown};

pub async fn run_daemon_process_until_shutdown(
    config: &DaemonProcessConfig,
) -> Result<(), DaemonError> {
    match config {
        DaemonProcessConfig::Control(config) => run_control_until_shutdown(config)
            .await
            .map_err(DaemonError::Control),
        DaemonProcessConfig::Machine(config) => run_machine_until_shutdown(config)
            .await
            .map_err(DaemonError::Machine),
        DaemonProcessConfig::Gateway(config) => run_gateway_until_shutdown(config)
            .await
            .map_err(DaemonError::Gateway),
        DaemonProcessConfig::Dns(config) => run_dns_until_shutdown(config)
            .await
            .map_err(DaemonError::Dns),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Control(ControlProcessError),
    #[error(transparent)]
    Machine(MachineProcessError),
    #[error(transparent)]
    Gateway(GatewayProcessError),
    #[error(transparent)]
    Dns(DnsProcessError),
}

//! Runtime dispatch for configured daemon processes.

use crate::config::DaemonProcessConfig;
use crate::roles::control::{ControlRuntimeError, run_control_until_shutdown};
use crate::roles::dns::process::{DnsProcessRuntimeError, run_dns_until_shutdown};
use crate::roles::gateway::process::{GatewayProcessRuntimeError, run_gateway_until_shutdown};
use crate::roles::machine::process::{MachineProcessRuntimeError, run_machine_until_shutdown};

pub async fn run_daemon_process_until_shutdown(
    config: &DaemonProcessConfig,
) -> Result<(), DaemonRuntimeError> {
    match config {
        DaemonProcessConfig::Control(config) => run_control_until_shutdown(config)
            .await
            .map_err(DaemonRuntimeError::Control),
        DaemonProcessConfig::Machine(config) => run_machine_until_shutdown(config)
            .await
            .map_err(DaemonRuntimeError::Machine),
        DaemonProcessConfig::Gateway(config) => run_gateway_until_shutdown(config)
            .await
            .map_err(DaemonRuntimeError::Gateway),
        DaemonProcessConfig::Dns(config) => run_dns_until_shutdown(config)
            .await
            .map_err(DaemonRuntimeError::Dns),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonRuntimeError {
    #[error(transparent)]
    Control(ControlRuntimeError),
    #[error(transparent)]
    Machine(MachineProcessRuntimeError),
    #[error(transparent)]
    Gateway(GatewayProcessRuntimeError),
    #[error(transparent)]
    Dns(DnsProcessRuntimeError),
}

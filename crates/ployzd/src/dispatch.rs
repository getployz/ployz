//! Dispatch for configured daemon role processes.

use crate::config::{DaemonProcessConfig, DaemonProcessConfigInner};
use crate::control::process::{ControlProcessError, run_control_until_shutdown};
use crate::roles::dns::process::{DnsProcessError, run_dns_until_shutdown};
use crate::roles::gateway::process::{GatewayProcessError, run_gateway_until_shutdown};
use crate::roles::machine::process::{MachineProcessError, run_machine_until_shutdown};

pub async fn run_daemon_process_until_shutdown(
    config: &DaemonProcessConfig,
) -> Result<(), DaemonError> {
    match config.inner() {
        DaemonProcessConfigInner::Control(config) => run_control_until_shutdown(config)
            .await
            .map_err(|error| DaemonError(DaemonErrorKind::Control(error))),
        DaemonProcessConfigInner::Machine(config) => run_machine_until_shutdown(config)
            .await
            .map_err(|error| DaemonError(DaemonErrorKind::Machine(error))),
        DaemonProcessConfigInner::Gateway(config) => run_gateway_until_shutdown(config)
            .await
            .map_err(|error| DaemonError(DaemonErrorKind::Gateway(error))),
        DaemonProcessConfigInner::Dns(config) => run_dns_until_shutdown(config)
            .await
            .map_err(|error| DaemonError(DaemonErrorKind::Dns(error))),
    }
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct DaemonError(DaemonErrorKind);

#[derive(Debug, thiserror::Error)]
enum DaemonErrorKind {
    #[error(transparent)]
    Control(ControlProcessError),
    #[error(transparent)]
    Machine(MachineProcessError),
    #[error(transparent)]
    Gateway(GatewayProcessError),
    #[error(transparent)]
    Dns(DnsProcessError),
}

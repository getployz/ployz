//! Runtime dispatch for configured daemon processes.

use crate::config::DaemonProcessConfig;
use crate::control_runtime::{ControlRuntimeError, run_control_until_shutdown};
use crate::role::DaemonProcessRole;
use std::fmt;

pub async fn run_daemon_process_until_shutdown(
    config: &DaemonProcessConfig,
) -> Result<(), DaemonRuntimeError> {
    match config {
        DaemonProcessConfig::Control(config) => run_control_until_shutdown(config)
            .await
            .map_err(DaemonRuntimeError::Control),
        DaemonProcessConfig::Node(_)
        | DaemonProcessConfig::Gateway(_)
        | DaemonProcessConfig::Dns(_)
        | DaemonProcessConfig::Tunnel(_) => Err(DaemonRuntimeError::RoleRuntimePending {
            role: config.role(),
        }),
    }
}

#[derive(Debug)]
pub enum DaemonRuntimeError {
    Control(ControlRuntimeError),
    RoleRuntimePending { role: DaemonProcessRole },
}

impl fmt::Display for DaemonRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Control(error) => write!(formatter, "{error}"),
            Self::RoleRuntimePending { role } => write!(
                formatter,
                "ployzd {} runtime is not implemented yet",
                role.process_name()
            ),
        }
    }
}

impl std::error::Error for DaemonRuntimeError {}

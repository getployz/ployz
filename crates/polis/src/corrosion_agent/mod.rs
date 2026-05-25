use std::{fs, io, net::SocketAddr, path::Path, process::ExitStatus};

use thiserror::Error;

use crate::{StoreError, StoreStatement, StoreTimeout};

mod config;
mod ownership;
mod process;

pub(crate) use config::write_config;
pub use config::{CorrosionAgentAddresses, CorrosionAgentConfig};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use config::{CorrosionAgentStartMode, CorrosionRootCleanup};
pub use ownership::CorrosionAdoption;
pub use process::{CorrosionExit, CorrosionShutdown, LocalCorrosionAgent};

#[derive(Debug, Error)]
pub enum CorrosionAgentError {
    #[error("corrosion setup failed: {message}")]
    Setup { message: String },

    #[error(
        "corrosion process exited before readiness: status={status}, stdout={stdout}, stderr={stderr}"
    )]
    ExitedBeforeReady {
        status: ExitStatus,
        stdout: String,
        stderr: String,
    },

    #[error("corrosion agent was not ready before deadline at {api_addr}")]
    ReadinessTimeout { api_addr: SocketAddr },

    #[error("corrosion store error during readiness: {source}")]
    Store { source: StoreError },

    #[error("corrosion ownership check failed: {message}")]
    Ownership { message: String },

    #[error("corrosion process exited unexpectedly: {exit}")]
    UnexpectedExit { exit: CorrosionExit },

    #[error("corrosion startup failed and cleanup failed: startup={startup}; cleanup={cleanup}")]
    StartupCleanup {
        startup: Box<CorrosionAgentError>,
        cleanup: Box<CorrosionAgentError>,
    },
}

impl CorrosionAgentError {
    #[must_use]
    pub fn is_retryable_startup_failure(&self) -> bool {
        match self {
            Self::ExitedBeforeReady { stdout, stderr, .. } => {
                let logs = format!("{stdout}\n{stderr}").to_ascii_lowercase();
                logs.contains("address already in use")
                    || logs.contains("addrinuse")
                    || logs.contains("failed to bind")
                    || logs.contains("bind")
            }
            Self::ReadinessTimeout { .. } => true,
            Self::StartupCleanup { .. } => false,
            Self::Setup { .. }
            | Self::Store { .. }
            | Self::Ownership { .. }
            | Self::UnexpectedExit { .. } => false,
        }
    }
}

fn store_probe_statement() -> Result<StoreStatement, CorrosionAgentError> {
    StoreStatement::new("SELECT 1 AS ready").map_err(|source| CorrosionAgentError::Store { source })
}

fn store_probe_timeout() -> Result<StoreTimeout, CorrosionAgentError> {
    StoreTimeout::seconds(1).map_err(|source| CorrosionAgentError::Store { source })
}

fn setup_error(error: io::Error) -> CorrosionAgentError {
    setup_message(error.to_string())
}

fn setup_message(message: String) -> CorrosionAgentError {
    CorrosionAgentError::Setup { message }
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_corrosion_binary_is_setup_error() {
        let config = crate::test_support::CorrosionAgentFixtureConfig::isolated()
            .expect("config")
            .with_binary_path("/definitely/missing/corrosion");

        let error = config.start().await.expect_err("error");

        assert!(matches!(error, CorrosionAgentError::Setup { .. }));
    }
}

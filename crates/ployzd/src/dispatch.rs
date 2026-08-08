//! Dispatch for the v2 daemon role shell.

use tracing::Instrument;

use crate::role_cli::DaemonProcessRole;
use crate::roles::api::http::ApiRoleRuntimeError;
use crate::roles::dns::DnsRoleRuntimeError;
use crate::roles::gateway::GatewayRoleRuntimeError;
use crate::roles::keeper::KeeperRoleRuntimeError;

// The attribution span is declared at ERROR level so a restrictive log filter
// still records which process role produced a startup failure.
pub async fn run_daemon_process(role: DaemonProcessRole) -> Result<(), DaemonError> {
    async move {
        match role {
            DaemonProcessRole::Api => crate::roles::api::http::run_from_environment()
                .await
                .map_err(DaemonError::Api),
            DaemonProcessRole::Keeper => crate::roles::keeper::run_from_environment()
                .await
                .map_err(DaemonError::Keeper),
            DaemonProcessRole::Dns => crate::roles::dns::run_from_environment()
                .await
                .map_err(DaemonError::Dns),
            DaemonProcessRole::Gateway => crate::roles::gateway::run_from_environment()
                .await
                .map_err(DaemonError::Gateway),
        }
    }
    .instrument(tracing::error_span!("role", process = role.as_str()))
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Api(ApiRoleRuntimeError),
    #[error(transparent)]
    Keeper(KeeperRoleRuntimeError),
    #[error(transparent)]
    Dns(DnsRoleRuntimeError),
    #[error(transparent)]
    Gateway(GatewayRoleRuntimeError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_has_a_concrete_runtime_error_lane() {
        let error = DaemonError::Gateway(GatewayRoleRuntimeError::ListenerStopped);
        assert_eq!(
            error.to_string(),
            "gateway listener stopped without process shutdown"
        );
    }
}

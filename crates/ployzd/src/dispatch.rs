//! Dispatch for the v2 daemon role shell.

use tracing::Instrument;

use crate::role_cli::DaemonProcessRole;
use crate::roles::api::http::ApiRoleRuntimeError;
use crate::roles::dns::DnsRoleRuntimeError;
use crate::roles::keeper::KeeperRoleRuntimeError;

// The attribution span is declared at ERROR level so a restrictive log filter
// still records the role on the placeholder failure.
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
            DaemonProcessRole::Gateway => Err(DaemonError::RoleNotImplemented { role }),
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
    #[error("ployzd {role} is not implemented in the v2 runtime yet", role = role.as_str())]
    RoleNotImplemented { role: DaemonProcessRole },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn gateway_exits_with_an_explicit_bounded_placeholder() {
        let role = DaemonProcessRole::Gateway;
        assert!(matches!(
            run_daemon_process(role).await,
            Err(DaemonError::RoleNotImplemented { role: actual }) if actual == role
        ));
    }
}

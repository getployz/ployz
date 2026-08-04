//! Dispatch for the v2 daemon role shell.

use tracing::Instrument;

use crate::role_cli::DaemonProcessRole;
use crate::roles::api::http::ApiRoleRuntimeError;
use crate::roles::keeper::KeeperRoleRuntimeError;

// The attribution span is declared at ERROR level so a restrictive log filter
// still records the role on the placeholder failure.
pub async fn run_daemon_process(role: DaemonProcessRole) -> Result<(), DaemonError> {
    run_daemon_role(role)
        .instrument(tracing::error_span!("role", process = role.as_str()))
        .await
}

async fn run_daemon_role(role: DaemonProcessRole) -> Result<(), DaemonError> {
    match runtime_for(role) {
        DaemonRuntime::Api => crate::roles::api::http::run_from_environment()
            .await
            .map_err(DaemonError::Api),
        DaemonRuntime::Keeper => crate::roles::keeper::run_from_environment()
            .await
            .map_err(DaemonError::Keeper),
        DaemonRuntime::Placeholder => run_role_placeholder(role).await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonRuntime {
    Api,
    Keeper,
    Placeholder,
}

const fn runtime_for(role: DaemonProcessRole) -> DaemonRuntime {
    match role {
        DaemonProcessRole::Api => DaemonRuntime::Api,
        DaemonProcessRole::Keeper => DaemonRuntime::Keeper,
        DaemonProcessRole::Gateway | DaemonProcessRole::Dns => DaemonRuntime::Placeholder,
    }
}

async fn run_role_placeholder(role: DaemonProcessRole) -> Result<(), DaemonError> {
    Err(DaemonError::RoleNotImplemented { role })
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Api(ApiRoleRuntimeError),
    #[error(transparent)]
    Keeper(KeeperRoleRuntimeError),
    #[error("ployzd {role} is not implemented in the v2 runtime yet", role = role.as_str())]
    RoleNotImplemented { role: DaemonProcessRole },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeper_and_api_select_real_runtimes() {
        assert_eq!(
            runtime_for(DaemonProcessRole::Keeper),
            DaemonRuntime::Keeper
        );
        assert_eq!(runtime_for(DaemonProcessRole::Api), DaemonRuntime::Api);
    }

    #[tokio::test]
    async fn gateway_and_dns_exit_with_an_explicit_bounded_placeholder() {
        for role in [DaemonProcessRole::Gateway, DaemonProcessRole::Dns] {
            assert!(matches!(
                run_daemon_process(role).await,
                Err(DaemonError::RoleNotImplemented { role: actual }) if actual == role
            ));
        }
    }
}

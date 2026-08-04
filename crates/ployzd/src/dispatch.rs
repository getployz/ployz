//! Dispatch for the v2 daemon role shell.

use tracing::Instrument;

use crate::role_cli::DaemonProcessRole;
use crate::roles::api::http::ApiRoleRuntimeError;

// The attribution span is declared at ERROR level so a restrictive log filter
// still records the role on the placeholder failure.
pub async fn run_daemon_process(role: DaemonProcessRole) -> Result<(), DaemonError> {
    run_daemon_role(role)
        .instrument(tracing::error_span!("role", process = role.as_str()))
        .await
}

async fn run_daemon_role(role: DaemonProcessRole) -> Result<(), DaemonError> {
    match role {
        DaemonProcessRole::Api => crate::roles::api::http::run_from_environment()
            .await
            .map_err(DaemonError::Api),
        DaemonProcessRole::Keeper | DaemonProcessRole::Gateway | DaemonProcessRole::Dns => {
            run_role_placeholder(role).await
        }
    }
}

async fn run_role_placeholder(role: DaemonProcessRole) -> Result<(), DaemonError> {
    Err(DaemonError::RoleNotImplemented { role })
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Api(ApiRoleRuntimeError),
    #[error("ployzd {role} is not implemented in the v2 runtime yet", role = role.as_str())]
    RoleNotImplemented { role: DaemonProcessRole },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unimplemented_roles_exit_with_an_explicit_bounded_placeholder() {
        for role in [
            DaemonProcessRole::Keeper,
            DaemonProcessRole::Gateway,
            DaemonProcessRole::Dns,
        ] {
            assert!(matches!(
                run_daemon_process(role).await,
                Err(DaemonError::RoleNotImplemented { role: actual }) if actual == role
            ));
        }
    }
}

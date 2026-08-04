//! Dispatch for the v2 daemon role shell.

use tracing::Instrument;

use crate::role_cli::DaemonProcessRole;

// The attribution span is declared at ERROR level so a restrictive log filter
// still records the role on the placeholder failure.
pub async fn run_daemon_process(role: DaemonProcessRole) -> Result<(), DaemonError> {
    run_role_placeholder(role)
        .instrument(tracing::error_span!("role", process = role.as_str()))
        .await
}

async fn run_role_placeholder(role: DaemonProcessRole) -> Result<(), DaemonError> {
    Err(DaemonError::RoleNotImplemented { role })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DaemonError {
    #[error("ployzd {role} is not implemented in the v2 runtime yet", role = role.as_str())]
    RoleNotImplemented { role: DaemonProcessRole },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_role_exits_with_an_explicit_bounded_placeholder() {
        for role in [
            DaemonProcessRole::Keeper,
            DaemonProcessRole::Api,
            DaemonProcessRole::Gateway,
            DaemonProcessRole::Dns,
        ] {
            assert_eq!(
                run_daemon_process(role).await,
                Err(DaemonError::RoleNotImplemented { role })
            );
        }
    }
}

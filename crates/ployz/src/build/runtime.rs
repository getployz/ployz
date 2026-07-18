use std::time::Duration;

use ployz_core::build::{BUILD_MAX_ATTACHED_WATCH_TIMEOUT, GitSource};
use ployz_sdk_types::{BuildCancelRequest, BuildSubmitRequest};

use crate::build::command::{BuildCancelCommand, BuildSubmitCommand};
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{PloyzctlExecutionOutput, api_error, operation_api_client};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildExecutionError {
    #[error(
        "environment variable {name} containing the Git secret is not set or is not valid Unicode"
    )]
    ReadGitSecret { name: String },
    #[error("Git source is invalid: {message}")]
    InvalidGitSource { message: String },
}

pub(crate) async fn submit(
    command: BuildSubmitCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let secret =
        std::env::var(&command.git_secret_env).map_err(|_| BuildExecutionError::ReadGitSecret {
            name: command.git_secret_env.clone(),
        })?;
    let source = GitSource::try_new(
        command.git.as_str(),
        command.commit.as_str(),
        command.git_username.as_str(),
        secret,
        command.subdir.as_ref().map(|path| path.as_str()),
    )
    .map_err(|error| BuildExecutionError::InvalidGitSource {
        message: error.to_string(),
    })?;
    let detach = command.detach;
    let api = operation_api_client(config).await?;
    let accepted = api
        .build_submit(&BuildSubmitRequest {
            operation_id: command.operation_id,
            source,
            adapter: command.adapter,
            platforms: command.platforms,
        })
        .await
        .map_err(api_error)?;
    if detach {
        return Ok(PloyzctlExecutionOutput::stdout(
            crate::operation::command::AcceptedOperationOutput::from_accepted(accepted).render(),
        ));
    }
    crate::operation::runtime::watch_accepted_with_timeout(
        &api,
        accepted.operation_id,
        attached_build_watch_timeout(config),
        config,
    )
    .await
}

pub(crate) async fn cancel(
    command: BuildCancelCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let api = operation_api_client(config).await?;
    let accepted = api
        .build_cancel(&BuildCancelRequest {
            operation_id: command.operation_id,
            reason: command.reason,
        })
        .await
        .map_err(api_error)?;
    crate::operation::runtime::watch_accepted_with_timeout(
        &api,
        accepted.operation_id,
        attached_build_watch_timeout(config),
        config,
    )
    .await
}

fn attached_build_watch_timeout(config: &PloyzctlRuntimeConfig) -> Duration {
    config
        .ops_watch_timeout
        .unwrap_or(BUILD_MAX_ATTACHED_WATCH_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::execution_support::DEFAULT_OPS_WATCH_TIMEOUT;

    #[test]
    fn attached_build_submit_and_cancel_use_build_budget_without_changing_generic_watch_default() {
        let config = PloyzctlRuntimeConfig::default();

        assert_eq!(
            attached_build_watch_timeout(&config),
            Duration::from_secs(35 * 60)
        );
        assert_eq!(config.ops_watch_timeout(), DEFAULT_OPS_WATCH_TIMEOUT);
        assert_eq!(DEFAULT_OPS_WATCH_TIMEOUT, Duration::from_secs(10 * 60));
        assert!(BUILD_MAX_ATTACHED_WATCH_TIMEOUT == Duration::from_secs(35 * 60));
    }

    #[test]
    fn attached_build_submit_and_cancel_honor_explicit_watch_timeout() {
        let explicit = Duration::from_secs(47);
        let config = PloyzctlRuntimeConfig {
            ops_watch_timeout: Some(explicit),
            ..PloyzctlRuntimeConfig::default()
        };

        assert_eq!(attached_build_watch_timeout(&config), explicit);
    }
}

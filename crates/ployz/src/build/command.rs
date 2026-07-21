use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgGroup, Args};
use ployz_core::build::{
    BuildAdapter, BuildCacheScope, BuildContextPath, BuildPlatforms, DockerfileStageName,
    GitCommit, GitCredentialUsername, GitRepositoryUrl,
};
use ployz_core::ids::{BuildExecutorId, BuildPoolId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::CancellationReason;

use crate::build::enrollment::EnrollmentUrl;
use crate::commands::{PloyzctlCliError, cli_error, invalid_value};
use crate::execution_support::generate_client_build_id;

const DEFAULT_CANCELLATION_REASON: &str = "operator requested build cancellation";
const DEFAULT_EXECUTOR_WAIT_TIMEOUT_SECONDS: u64 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildExecutorRunMode {
    Once { wait_timeout: Duration },
    Watch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildExecutorAdmission {
    AnyOperation,
    ExactOperation(OperationId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildEnrollCommand {
    pub enrollment_url: EnrollmentUrl,
    pub token_env: String,
    pub pool_id: BuildPoolId,
    pub executor_id: BuildExecutorId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildExecutorCommand {
    pub pool_id: BuildPoolId,
    pub executor_id: BuildExecutorId,
    pub workspace_root: Option<PathBuf>,
    pub admission: BuildExecutorAdmission,
    pub mode: BuildExecutorRunMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSubmitCommand {
    pub operation_id: OperationId,
    pub git: GitRepositoryUrl,
    pub commit: GitCommit,
    pub git_username: GitCredentialUsername,
    pub git_secret_env: String,
    pub subdir: Option<BuildContextPath>,
    pub adapter: BuildAdapter,
    pub platforms: BuildPlatforms,
    pub detach: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildCancelCommand {
    pub operation_id: OperationId,
    pub reason: CancellationReason,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("adapter").required(true).args(["dockerfile", "railpack"])))]
pub(crate) struct BuildSubmitCli {
    #[arg(long)]
    git: String,
    #[arg(long)]
    commit: String,
    #[arg(long)]
    git_username: String,
    /// Name of the environment variable containing the Git credential secret.
    #[arg(long)]
    git_secret_env: String,
    #[arg(long)]
    subdir: Option<String>,
    #[arg(long, value_name = "OS/ARCH", required = true)]
    platform: Vec<String>,
    #[arg(long, conflicts_with = "railpack")]
    dockerfile: Option<String>,
    #[arg(long, requires = "dockerfile")]
    target: Option<String>,
    #[arg(long, conflicts_with = "dockerfile")]
    railpack: bool,
    #[arg(long, requires = "railpack")]
    cache_scope: Option<String>,
    #[arg(long)]
    operation_id: Option<String>,
    #[arg(long)]
    detach: bool,
}

#[derive(Debug, Args)]
pub(crate) struct BuildCancelCli {
    operation_id: String,
    #[arg(long)]
    reason: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct BuildExecutorOnceCli {
    #[arg(long)]
    pool_id: String,
    #[arg(long)]
    executor_id: String,
    #[arg(long)]
    workspace_root: Option<PathBuf>,
    #[arg(long, default_value_t = DEFAULT_EXECUTOR_WAIT_TIMEOUT_SECONDS)]
    wait_timeout_seconds: u64,
}

#[derive(Debug, Args)]
pub(crate) struct BuildExecutorWatchCli {
    #[arg(long)]
    pool_id: String,
    #[arg(long)]
    executor_id: String,
    #[arg(long)]
    workspace_root: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct BuildEnrollCli {
    #[arg(long)]
    enrollment_url: String,
    /// Name of the environment variable containing the enrollment token.
    #[arg(long)]
    token_env: String,
    #[arg(long)]
    pool_id: String,
    #[arg(long)]
    executor_id: String,
}

pub(crate) fn build_enroll_command(
    parsed: BuildEnrollCli,
) -> Result<BuildEnrollCommand, PloyzctlCliError> {
    validate_environment_variable_name("--token-env", &parsed.token_env)?;
    Ok(BuildEnrollCommand {
        enrollment_url: EnrollmentUrl::try_new(parsed.enrollment_url)
            .map_err(|error| invalid_value("--enrollment-url", error))?,
        token_env: parsed.token_env,
        pool_id: BuildPoolId::try_new(parsed.pool_id)
            .map_err(|error| invalid_value("--pool-id", error))?,
        executor_id: BuildExecutorId::try_new(parsed.executor_id)
            .map_err(|error| invalid_value("--executor-id", error))?,
    })
}

pub(crate) fn build_executor_once_command(
    parsed: BuildExecutorOnceCli,
) -> Result<BuildExecutorCommand, PloyzctlCliError> {
    if parsed.wait_timeout_seconds == 0 {
        return Err(cli_error(
            "--wait-timeout-seconds must be greater than zero",
        ));
    }
    build_executor_command(
        parsed.pool_id,
        parsed.executor_id,
        parsed.workspace_root,
        BuildExecutorRunMode::Once {
            wait_timeout: Duration::from_secs(parsed.wait_timeout_seconds),
        },
    )
}

pub(crate) fn build_executor_watch_command(
    parsed: BuildExecutorWatchCli,
) -> Result<BuildExecutorCommand, PloyzctlCliError> {
    build_executor_command(
        parsed.pool_id,
        parsed.executor_id,
        parsed.workspace_root,
        BuildExecutorRunMode::Watch,
    )
}

fn build_executor_command(
    pool_id: String,
    executor_id: String,
    workspace_root: Option<PathBuf>,
    mode: BuildExecutorRunMode,
) -> Result<BuildExecutorCommand, PloyzctlCliError> {
    Ok(BuildExecutorCommand {
        pool_id: BuildPoolId::try_new(pool_id)
            .map_err(|error| invalid_value("--pool-id", error))?,
        executor_id: BuildExecutorId::try_new(executor_id)
            .map_err(|error| invalid_value("--executor-id", error))?,
        workspace_root,
        admission: BuildExecutorAdmission::AnyOperation,
        mode,
    })
}

pub(crate) fn build_submit_command(
    parsed: BuildSubmitCli,
) -> Result<BuildSubmitCommand, PloyzctlCliError> {
    validate_environment_variable_name("--git-secret-env", &parsed.git_secret_env)?;
    let operation_id = parsed
        .operation_id
        .map(OperationId::try_new)
        .transpose()
        .map_err(|error| invalid_value("--operation-id", error))?
        .map_or_else(
            || generate_client_build_id().map(|generated| generated.operation_id),
            Ok,
        )
        .map_err(|error| invalid_value("--operation-id", error))?;
    let platforms = parsed
        .platform
        .iter()
        .map(|platform| parse_platform(platform))
        .collect::<Result<Vec<_>, _>>()?;
    let platforms =
        BuildPlatforms::try_new(platforms).map_err(|error| invalid_value("--platform", error))?;
    let adapter = match (parsed.dockerfile, parsed.railpack, parsed.cache_scope) {
        (Some(dockerfile), false, None) => BuildAdapter::Dockerfile {
            dockerfile: BuildContextPath::try_new(dockerfile)
                .map_err(|error| invalid_value("--dockerfile", error))?,
            target: parsed
                .target
                .map(DockerfileStageName::try_new)
                .transpose()
                .map_err(|error| invalid_value("--target", error))?,
        },
        (None, true, Some(cache_scope)) => BuildAdapter::Railpack {
            cache_scope: BuildCacheScope::try_new(cache_scope)
                .map_err(|error| invalid_value("--cache-scope", error))?,
        },
        (None, true, None) => return Err(cli_error("--railpack requires --cache-scope")),
        _ => return Err(cli_error("choose exactly one build adapter")),
    };

    Ok(BuildSubmitCommand {
        operation_id,
        git: GitRepositoryUrl::try_new(parsed.git)
            .map_err(|error| invalid_value("--git", error))?,
        commit: GitCommit::try_new(parsed.commit)
            .map_err(|error| invalid_value("--commit", error))?,
        git_username: GitCredentialUsername::try_new(parsed.git_username)
            .map_err(|error| invalid_value("--git-username", error))?,
        git_secret_env: parsed.git_secret_env,
        subdir: parsed
            .subdir
            .map(BuildContextPath::try_new)
            .transpose()
            .map_err(|error| invalid_value("--subdir", error))?,
        adapter,
        platforms,
        detach: parsed.detach,
    })
}

fn validate_environment_variable_name(option: &str, name: &str) -> Result<(), PloyzctlCliError> {
    if name.is_empty() || name.contains('=') || name.chars().any(char::is_whitespace) {
        return Err(cli_error(format!(
            "{option} must name one non-empty environment variable"
        )));
    }
    Ok(())
}

pub(crate) fn build_cancel_command(
    parsed: BuildCancelCli,
) -> Result<BuildCancelCommand, PloyzctlCliError> {
    Ok(BuildCancelCommand {
        operation_id: OperationId::try_new(parsed.operation_id)
            .map_err(|error| invalid_value("<operation_id>", error))?,
        reason: CancellationReason::try_new(
            parsed
                .reason
                .unwrap_or_else(|| DEFAULT_CANCELLATION_REASON.to_owned()),
        )
        .map_err(|error| invalid_value("--reason", error))?,
    })
}

fn parse_platform(value: &str) -> Result<OciPlatform, PloyzctlCliError> {
    let Some((os, architecture)) = value.split_once('/') else {
        return Err(cli_error("--platform must use OS/ARCH"));
    };
    if architecture.contains('/') {
        return Err(cli_error("--platform must use OS/ARCH"));
    }
    OciPlatform::try_new(os, architecture).map_err(|error| invalid_value("--platform", error))
}

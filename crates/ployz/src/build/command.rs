use clap::{ArgGroup, Args};
use ployz_core::build::{
    BuildAdapter, BuildCacheScope, BuildContextPath, BuildPlatforms, DockerfileStageName,
    GitCommit, GitCredentialUsername, GitRepositoryUrl,
};
use ployz_core::ids::OperationId;
use ployz_core::image::OciPlatform;
use ployz_core::operation::CancellationReason;

use crate::commands::{PloyzctlCliError, cli_error, invalid_value};
use crate::execution_support::generate_client_build_id;

const DEFAULT_CANCELLATION_REASON: &str = "operator requested build cancellation";

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

pub(crate) fn build_submit_command(
    parsed: BuildSubmitCli,
) -> Result<BuildSubmitCommand, PloyzctlCliError> {
    if parsed.git_secret_env.is_empty()
        || parsed.git_secret_env.contains('=')
        || parsed.git_secret_env.chars().any(char::is_whitespace)
    {
        return Err(cli_error(
            "--git-secret-env must name one non-empty environment variable",
        ));
    }
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

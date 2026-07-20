use std::path::Path;
use std::time::Duration;

use crate::deploy::command::CurrentTreeDeployCommand;
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::PloyzctlExecutionOutput;

use super::DeployExecutionError;

mod cloud;
mod standalone;

const CLOUD_URL_ENV: &str = "PLOYZ_CLOUD_URL";
pub(super) const OBSERVE_TIMEOUT: Duration = Duration::from_secs(40 * 60);

enum CurrentTreeTarget {
    Cloud { url: String },
    Standalone,
}

pub(crate) async fn execute(
    command: CurrentTreeDeployCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    match target_from_environment() {
        CurrentTreeTarget::Cloud { url } => cloud::execute(command, config, &url).await,
        CurrentTreeTarget::Standalone => standalone::execute(command, config).await,
    }
}

fn target_from_environment() -> CurrentTreeTarget {
    match std::env::var(CLOUD_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(url) => CurrentTreeTarget::Cloud { url },
        None => CurrentTreeTarget::Standalone,
    }
}

fn write_private(path: &Path, contents: &str, mode: u32) -> Result<(), PloyzctlExecutionError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    use std::io::Write;
    let mut file = options.open(path).map_err(current_tree_error)?;
    file.write_all(contents.as_bytes())
        .map_err(current_tree_error)
}

fn current_tree_error(message: impl std::fmt::Display) -> PloyzctlExecutionError {
    DeployExecutionError::CurrentTree {
        message: message.to_string(),
    }
    .into()
}

use clap::Args;

use crate::commands::{PloyzctlCliError, invalid_value};
use crate::machine::ssh::SshTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorePromoteCommand {
    pub target: SshTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreReplaceCommand {
    pub target: SshTarget,
}

#[derive(Debug, Args)]
pub struct CorePromoteCli {
    pub target: String,
}

#[derive(Debug, Args)]
pub struct CoreReplaceCli {
    pub target: String,
}

pub fn core_promote_command(
    command: CorePromoteCli,
) -> Result<CorePromoteCommand, PloyzctlCliError> {
    let target =
        SshTarget::parse(&command.target).map_err(|source| invalid_value("target", source))?;
    Ok(CorePromoteCommand { target })
}

pub fn core_replace_command(
    command: CoreReplaceCli,
) -> Result<CoreReplaceCommand, PloyzctlCliError> {
    let target =
        SshTarget::parse(&command.target).map_err(|source| invalid_value("target", source))?;
    Ok(CoreReplaceCommand { target })
}

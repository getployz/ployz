use clap::Args;
use ployz_nats::connect::{NatsClientUrl, NatsClientUrlError};

use crate::commands::{PloyzctlCliError, invalid_value};
use crate::ssh::SshTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreReplaceCommand {
    pub target: SshTarget,
    pub successor_nats_url: NatsClientUrl,
}

#[derive(Debug, Args)]
pub struct CoreReplaceCli {
    pub target: String,
    #[arg(long = "successor-nats-url", value_name = "url", required = true, value_parser = parse_nats_client_url)]
    pub successor_nats_url: NatsClientUrl,
}

pub fn core_replace_command(
    command: CoreReplaceCli,
) -> Result<CoreReplaceCommand, PloyzctlCliError> {
    let target =
        SshTarget::parse(&command.target).map_err(|source| invalid_value("target", source))?;
    Ok(CoreReplaceCommand {
        target,
        successor_nats_url: command.successor_nats_url,
    })
}

fn parse_nats_client_url(value: &str) -> Result<NatsClientUrl, NatsClientUrlError> {
    NatsClientUrl::try_new(value)
}

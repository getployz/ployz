//! `ployzd` process role command-line contract.

use clap::{Parser, Subcommand};

use ployz_core::ids::MachineId;

pub use ployz_core::roles::DaemonProcessRole;

#[derive(Debug)]
pub struct DaemonRoleParseError(clap::Error);

impl std::fmt::Display for DaemonRoleParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for DaemonRoleParseError {}

pub fn parse_role_args(
    args: impl IntoIterator<Item = String>,
) -> Result<DaemonProcessRole, DaemonRoleParseError> {
    let parsed = PloyzdRoleCli::try_parse_from(std::iter::once("ployzd".to_owned()).chain(args))
        .map_err(DaemonRoleParseError)?;
    Ok(parsed.command.into_role())
}

#[derive(Debug, Parser)]
#[command(name = "ployzd", disable_help_subcommand = true)]
struct PloyzdRoleCli {
    #[command(subcommand)]
    command: PloyzdRoleCommand,
}

#[derive(Debug, Subcommand)]
enum PloyzdRoleCommand {
    Control,
    Machine {
        #[arg(long, value_parser = parse_machine_id)]
        id: MachineId,
    },
    Gateway,
    Dns,
}

impl PloyzdRoleCommand {
    fn into_role(self) -> DaemonProcessRole {
        match self {
            Self::Control => DaemonProcessRole::Control,
            Self::Machine { id } => DaemonProcessRole::Machine(id),
            Self::Gateway => DaemonProcessRole::Gateway,
            Self::Dns => DaemonProcessRole::Dns,
        }
    }
}

fn parse_machine_id(value: &str) -> Result<MachineId, String> {
    MachineId::try_new(value.to_owned()).map_err(|error| error.to_string())
}

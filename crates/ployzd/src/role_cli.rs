//! `ployzd` process role command-line contract.

use clap::{Parser, Subcommand};
pub use ployz_core::roles::PloyzdRole as DaemonProcessRole;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DaemonRoleParseError(clap::Error);

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
    Keeper,
    Api,
    Gateway,
    Dns,
}

impl PloyzdRoleCommand {
    fn into_role(self) -> DaemonProcessRole {
        match self {
            Self::Keeper => DaemonProcessRole::Keeper,
            Self::Api => DaemonProcessRole::Api,
            Self::Gateway => DaemonProcessRole::Gateway,
            Self::Dns => DaemonProcessRole::Dns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_v2_role_subcommand() {
        for (argument, expected) in [
            ("keeper", DaemonProcessRole::Keeper),
            ("api", DaemonProcessRole::Api),
            ("gateway", DaemonProcessRole::Gateway),
            ("dns", DaemonProcessRole::Dns),
        ] {
            assert_eq!(
                parse_role_args([argument.to_owned()]).expect("role parses"),
                expected
            );
        }
    }

    #[test]
    fn rejects_removed_incumbent_roles() {
        for argument in ["control", "machine"] {
            assert!(parse_role_args([argument.to_owned()]).is_err());
        }
    }
}

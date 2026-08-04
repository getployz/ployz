//! Command contracts for behavior implemented entirely on the client machine.

use clap::{Parser, Subcommand};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryCommand {
    Enable,
    Disable,
}

/// Parses a `ployz` invocation from the commands implemented by this local
/// shell. Transport-backed commands appear only when their client exists.
pub fn parse_command(
    args: impl IntoIterator<Item = String>,
) -> Result<TelemetryCommand, clap::Error> {
    let parsed = Cli::try_parse_from(std::iter::once("ployz".to_owned()).chain(args))?;
    let Some(command) = parsed.command else {
        return Err(root_help());
    };

    Ok(match command {
        CommandCli::Telemetry { command } => match command {
            TelemetryCli::Enable => TelemetryCommand::Enable,
            TelemetryCli::Disable => TelemetryCommand::Disable,
        },
    })
}

fn root_help() -> clap::Error {
    Cli::try_parse_from(["ployz", "--help"]).expect_err("--help always produces a display result")
}

#[derive(Debug, Parser)]
#[command(
    name = "ployz",
    version,
    about = "Command small Ployz clusters",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandCli>,
}

#[derive(Debug, Subcommand)]
enum CommandCli {
    /// Configure anonymous usage and error telemetry.
    Telemetry {
        #[command(subcommand)]
        command: TelemetryCli,
    },
}

#[derive(Debug, Subcommand)]
enum TelemetryCli {
    /// Enable telemetry.
    Enable,
    /// Disable telemetry.
    Disable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_telemetry_preferences() {
        for (verb, expected) in [
            ("enable", TelemetryCommand::Enable),
            ("disable", TelemetryCommand::Disable),
        ] {
            let command = parse_command(["telemetry", verb].map(str::to_owned))
                .expect("telemetry preference parses");

            assert_eq!(command, expected);
        }
    }

    #[test]
    fn bare_invocation_displays_help() {
        let error = parse_command(std::iter::empty()).expect_err("command is required");

        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        assert!(error.to_string().contains("telemetry"));
    }

    #[test]
    fn transport_commands_are_unavailable_without_a_client() {
        for command in ["deploy", "machine", "ops", "core", "host", "init"] {
            let error = parse_command([command.to_owned()]).expect_err("command was removed");

            assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
        }
    }
}

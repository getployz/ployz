use std::process::ExitCode;

use clap::error::ErrorKind;
use ployz::commands::{TelemetryCommand, parse_command};
use ployz_telemetry::ConfigFile;

fn main() -> ExitCode {
    let command = match parse_command(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            print!("{error}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprint!("{error}");
            return ExitCode::FAILURE;
        }
    };

    set_telemetry(command)
}

fn set_telemetry(command: TelemetryCommand) -> ExitCode {
    let mut config = match ConfigFile::load_or_create_default() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let enabled = match command {
        TelemetryCommand::Enable => true,
        TelemetryCommand::Disable => false,
    };
    if let Err(error) = config.set_telemetry(enabled) {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    println!(
        "Telemetry {}.",
        if enabled { "enabled" } else { "disabled" }
    );
    ExitCode::SUCCESS
}

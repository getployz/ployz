use std::process::ExitCode;

use clap::error::ErrorKind;
use ployz::commands::{Command, TelemetryCommand, parse_command};
use ployz_telemetry::ConfigFile;

#[tokio::main]
async fn main() -> ExitCode {
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

    match command {
        Command::Telemetry(command) => set_telemetry(command),
        Command::Init(command) => match ployz::init::execute(*command).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Command::Machine(command) => match ployz::machine::execute(command).await {
            Ok(output) => {
                print!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Command::Peer(command) => match ployz::peer::execute(command).await {
            Ok(output) => {
                print!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Command::Token(command) => match ployz::token::execute(command).await {
            Ok(output) => {
                print!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
    }
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

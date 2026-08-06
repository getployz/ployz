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
        Command::Namespace(command) => finish_output(ployz::namespace::execute(command).await),
        Command::Deploy(command) => finish_output(ployz::deploy::execute(command).await),
        Command::Ops(command) => finish_output(ployz::ops::execute(command).await),
        Command::Logs(command) => finish_output(ployz::logs::execute(command).await),
        Command::Peer(command) => emit_removal(ployz::removal::execute_peer(command).await),
        Command::Service(command) => emit_removal(ployz::removal::execute_service(command).await),
        Command::Route(command) => emit_removal(ployz::removal::execute_route(command).await),
        Command::Status(command) => emit_diagnostics(ployz::diagnostics::status(command).await),
        Command::Doctor(command) => emit_diagnostics(ployz::diagnostics::doctor(command).await),
    }
}

fn emit_removal(result: Result<String, ployz::removal::RemovalExecutionError>) -> ExitCode {
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn finish_output<Error>(result: Result<String, Error>) -> ExitCode
where
    Error: std::fmt::Display,
{
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn emit_diagnostics(outcome: ployz::diagnostics::DiagnosticsOutcome) -> ExitCode {
    if outcome.is_unreachable() {
        eprint!("{}", outcome.output());
    } else {
        print!("{}", outcome.output());
    }
    if outcome.is_success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
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

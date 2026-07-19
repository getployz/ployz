use std::process::ExitCode;
use std::time::Instant;

use ployz::commands::{PloyzctlCommand, TelemetryCommand, parse_invocation};
use ployz::dispatcher::{CommandExit, PloyzctlRuntimeConfig, execute_command};
use ployz_telemetry::{CommandOutcome, ConfigFile, Surface, Telemetry};

/// `ployz host ...` dispatches before the async runtime exists: Host Runner
/// commands are synchronous and drive their own bounded runtimes internally,
/// which panics under an ambient tokio runtime.
fn main() -> ExitCode {
    let raw_args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if raw_args.first().is_some_and(|arg| arg == "host") {
        return host_main(raw_args);
    }

    product_main()
}

fn host_main(raw_args: Vec<std::ffi::OsString>) -> ExitCode {
    let started_at = Instant::now();
    let command = match ployz_host_runner::cli::load_command(raw_args.into_iter().skip(1)) {
        Ok(command) => command,
        Err(error) if error.is_help_requested() => {
            let telemetry = Telemetry::bootstrap(Surface::Cli, env!("CARGO_PKG_VERSION"));
            print!("{error}");
            telemetry.shutdown();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            let telemetry = Telemetry::bootstrap(Surface::Cli, env!("CARGO_PKG_VERSION"));
            telemetry.capture_error(&error, "host_parse");
            eprintln!("{error}");
            telemetry.shutdown();
            return ExitCode::FAILURE;
        }
    };
    let command_name = host_command_name(&command);
    let telemetry = Telemetry::bootstrap(Surface::Cli, env!("CARGO_PKG_VERSION"));
    let exit = ployz_host_runner::run_host_runner_command(command);
    let outcome = if exit == ExitCode::SUCCESS {
        CommandOutcome::Succeeded
    } else {
        CommandOutcome::Failed
    };
    telemetry.capture_command(command_name, outcome, started_at.elapsed());
    telemetry.shutdown();
    exit
}

fn product_main() -> ExitCode {
    let started_at = Instant::now();
    let invocation = match parse_invocation(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(error) if error.is_help_requested() => {
            let telemetry = Telemetry::bootstrap(Surface::Cli, env!("CARGO_PKG_VERSION"));
            print!("{error}");
            telemetry.shutdown();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            let telemetry = Telemetry::bootstrap(Surface::Cli, env!("CARGO_PKG_VERSION"));
            telemetry.capture_error(&error, "cli_parse");
            eprintln!("{error}");
            telemetry.shutdown();
            return ExitCode::FAILURE;
        }
    };

    if let PloyzctlCommand::Telemetry(command) = invocation.command {
        return set_telemetry(command);
    }

    let command_name = invocation.command.telemetry_name();
    let telemetry = Telemetry::bootstrap(Surface::Cli, env!("CARGO_PKG_VERSION"));
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            telemetry.capture_error(&error, "runtime");
            eprintln!("could not start async runtime: {error}");
            telemetry.shutdown();
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(execute_command(
        invocation.command,
        &PloyzctlRuntimeConfig::from_env().with_nats_url(invocation.nats_url),
    ));
    // Telemetry sinks flush with blocking calls and must outlive the async runtime.
    drop(runtime);
    let (exit, outcome) = match result {
        Ok(output) => {
            print!("{}", output.stdout);
            eprint!("{}", output.stderr);
            match output.exit {
                CommandExit::Success => (ExitCode::SUCCESS, CommandOutcome::Succeeded),
                CommandExit::Failure => (ExitCode::FAILURE, CommandOutcome::Failed),
            }
        }
        Err(error) => {
            telemetry.capture_error(&error, "command");
            eprintln!("{error}");
            (ExitCode::FAILURE, CommandOutcome::Failed)
        }
    };

    if let Some(command_name) = command_name {
        telemetry.capture_command(command_name, outcome, started_at.elapsed());
    }
    telemetry.shutdown();
    exit
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

fn host_command_name(command: &ployz_host_runner::cli::HostRunnerCommand) -> &'static str {
    use ployz_host_runner::cli::HostRunnerCommand;

    match command {
        HostRunnerCommand::Start(_) => "host start",
        HostRunnerCommand::Bootstrap(_) => "host bootstrap",
        HostRunnerCommand::FirstMachineInstall(_) => "host install",
        HostRunnerCommand::CorePromote(_) => "host core-promote",
        HostRunnerCommand::CoreDemote(_) => "host core-demote",
        HostRunnerCommand::SubstrateUpdate(_) => "host substrate-update",
        HostRunnerCommand::StoragePrepare(_) => "host storage-prepare",
        HostRunnerCommand::StorageRecover => "host internal-storage-recover",
        HostRunnerCommand::StorageCapability => "host internal-storage-capability",
        HostRunnerCommand::StoragePoolFacts => "host internal-storage-pool-facts",
        HostRunnerCommand::StorageDatasetEnsure(_) => "host internal-storage-dataset-ensure",
        HostRunnerCommand::StorageDatasetFacts(_) => "host internal-storage-dataset-facts",
        HostRunnerCommand::StorageDatasetDestroy(_) => "host internal-storage-dataset-destroy",
    }
}

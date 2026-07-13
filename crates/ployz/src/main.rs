use std::error::Error;
use std::process::ExitCode;
use std::time::Instant;

use ployz::commands::{PloyzctlCommand, TelemetryCommand, parse_invocation};
use ployz::runtime::{CommandExit, PloyzctlRuntimeConfig, execute_command};
use ployz_telemetry::{CommandOutcome, ConfigError, ConfigFile, Consent, Surface, Telemetry};

const TELEMETRY_NOTICE: &str = "Ployz collects anonymous usage and error telemetry. Disable it with `ployz telemetry disable`.";
const SENTRY_DSN: &str = match option_env!("PLOYZ_SENTRY_DSN") {
    Some(value) => value,
    None => "",
};
const POSTHOG_KEY: &str = match option_env!("PLOYZ_POSTHOG_KEY") {
    Some(value) => value,
    None => "",
};

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
            print!("{error}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            let telemetry = start_cli_telemetry();
            capture_error(telemetry.as_ref(), &error, "host_parse");
            eprintln!("{error}");
            shutdown(telemetry);
            return ExitCode::FAILURE;
        }
    };
    let command_name = host_command_name(&command);
    let telemetry = start_cli_telemetry();
    let exit = ployz_host_runner::run_host_runner_command(command);
    capture_command(
        telemetry.as_ref(),
        command_name,
        outcome(exit == ExitCode::SUCCESS),
        started_at,
    );
    shutdown(telemetry);
    exit
}

#[tokio::main(flavor = "current_thread")]
async fn product_main() -> ExitCode {
    let started_at = Instant::now();
    let invocation = match parse_invocation(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(error) if error.is_help_requested() => {
            print!("{error}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            let telemetry = start_cli_telemetry();
            capture_error(telemetry.as_ref(), &error, "cli_parse");
            eprintln!("{error}");
            shutdown(telemetry);
            return ExitCode::FAILURE;
        }
    };

    if let PloyzctlCommand::Telemetry(command) = invocation.command {
        return set_telemetry(command);
    }

    let command_name = invocation.command.telemetry_name();
    let telemetry = start_cli_telemetry();
    let (exit, succeeded) = match execute_command(
        invocation.command,
        &PloyzctlRuntimeConfig::from_env().with_nats_url(invocation.nats_url),
    )
    .await
    {
        Ok(output) => {
            print!("{}", output.stdout);
            eprint!("{}", output.stderr);
            (
                match output.exit {
                    CommandExit::Success => ExitCode::SUCCESS,
                    CommandExit::Failure => ExitCode::FAILURE,
                },
                output.exit == CommandExit::Success,
            )
        }
        Err(error) => {
            capture_error(telemetry.as_ref(), &error, "command");
            eprintln!("{error}");
            (ExitCode::FAILURE, false)
        }
    };

    if let Some(command_name) = command_name {
        capture_command(
            telemetry.as_ref(),
            command_name,
            outcome(succeeded),
            started_at,
        );
    }
    shutdown(telemetry);
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

fn start_cli_telemetry() -> Option<Telemetry> {
    let mut config = match ConfigFile::load_or_create_default() {
        Ok(config) => config,
        Err(ConfigError::HomeUnavailable) => return None,
        Err(error) => {
            eprintln!("{error}");
            return None;
        }
    };
    if let Some(diagnostic) = config.diagnostic() {
        eprintln!("{diagnostic}");
    }
    let consent = Consent::from_environment(config.persisted_telemetry());
    let enabled = consent.is_enabled();
    if enabled && config.notice_needed(Surface::Cli) {
        eprintln!("{TELEMETRY_NOTICE}");
        if let Err(error) = config.mark_notice_seen(Surface::Cli) {
            eprintln!("{error}");
        }
    }
    let mut telemetry =
        Telemetry::start(Surface::Cli, env!("CARGO_PKG_VERSION"), consent, SENTRY_DSN).ok()?;
    let _attach_result = telemetry.attach_posthog(POSTHOG_KEY, config.install_id().to_string());
    Some(telemetry)
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
    }
}

fn capture_command(
    telemetry: Option<&Telemetry>,
    command: &str,
    outcome: CommandOutcome,
    started_at: Instant,
) {
    if let Some(telemetry) = telemetry {
        telemetry.capture_command(command, outcome, started_at.elapsed());
    }
}

fn capture_error(telemetry: Option<&Telemetry>, error: &(dyn Error + 'static), tag: &str) {
    if let Some(telemetry) = telemetry {
        telemetry.capture_error(error, tag);
    }
}

fn shutdown(telemetry: Option<Telemetry>) {
    if let Some(telemetry) = telemetry {
        telemetry.shutdown();
    }
}

const fn outcome(succeeded: bool) -> CommandOutcome {
    if succeeded {
        CommandOutcome::Succeeded
    } else {
        CommandOutcome::Failed
    }
}

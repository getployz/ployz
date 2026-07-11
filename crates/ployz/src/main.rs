use std::process::ExitCode;

use ployz::commands::parse_invocation;
use ployz::runtime::{CommandExit, PloyzctlRuntimeConfig, execute_command};

/// `ployz host ...` dispatches before the async runtime exists: Host Runner
/// commands are synchronous and drive their own bounded runtimes internally,
/// which panics under an ambient tokio runtime.
fn main() -> ExitCode {
    let raw_args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if raw_args.first().is_some_and(|arg| arg == "host") {
        return match ployz_host_runner::cli::load_command(raw_args.into_iter().skip(1)) {
            Ok(command) => ployz_host_runner::run_host_runner_command(command),
            Err(error) if error.is_help_requested() => {
                print!("{error}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }

    product_main()
}

#[tokio::main(flavor = "current_thread")]
async fn product_main() -> ExitCode {
    let invocation = match parse_invocation(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(error) if error.is_help_requested() => {
            print!("{error}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    match execute_command(
        invocation.command,
        &PloyzctlRuntimeConfig::from_env().with_nats_url(invocation.nats_url),
    )
    .await
    {
        Ok(output) => {
            print!("{}", output.stdout);
            eprint!("{}", output.stderr);
            match output.exit {
                CommandExit::Success => ExitCode::SUCCESS,
                CommandExit::Failure => ExitCode::FAILURE,
            }
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

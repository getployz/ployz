use std::process::ExitCode;

use ployzctl::commands::parse_invocation;
use ployzctl::runtime::{PloyzctlRuntimeConfig, execute_command};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
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
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

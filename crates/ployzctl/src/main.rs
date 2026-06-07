use std::process::ExitCode;

use ployzctl::commands::parse_invocation;
use ployzctl::runtime::{PloyzctlRuntimeConfig, execute_command};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match parse_invocation(std::env::args().skip(1)) {
        Ok(invocation) => match execute_command(
            invocation.command,
            &PloyzctlRuntimeConfig::from_env().with_nats_url(invocation.nats_url),
        )
        .await
        {
            Ok(output) => {
                print!("{output}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

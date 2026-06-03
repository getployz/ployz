use std::process::ExitCode;

use tokio::io::BufReader;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        eprintln!("usage: ployzctl rpc-stdio");
        return ExitCode::FAILURE;
    };
    if command != "rpc-stdio" {
        eprintln!("unknown command: {command}");
        return ExitCode::FAILURE;
    }

    match run_rpc_stdio_command().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run_rpc_stdio_command() -> Result<(), PloyzctlError> {
    let stdin = BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    ployzd::serve_rpc_stdio(ployzd::DaemonConfig::from_env_defaults()?, stdin, stdout).await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum PloyzctlError {
    #[error("{0}")]
    Config(#[from] ployzd::DaemonConfigError),

    #[error("{0}")]
    Control(#[from] ployzd::DaemonControlError),
}

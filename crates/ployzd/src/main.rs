use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run_daemon().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run_daemon() -> Result<(), PloyzdMainError> {
    let runtime = ployzd::DaemonRuntime::start(ployzd::DaemonConfig::from_env_defaults()?).await?;
    eprintln!("ployzd ready endpoint={}", runtime.endpoint_id().as_str());
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("failed to wait for shutdown signal: {error}");
    }
    let stopped = runtime.shutdown().await?;
    eprintln!("ployzd stopped ready={}", stopped.is_ready());
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum PloyzdMainError {
    #[error("{0}")]
    Config(#[from] ployzd::DaemonConfigError),

    #[error("{0}")]
    Daemon(#[from] ployzd::DaemonError),
}

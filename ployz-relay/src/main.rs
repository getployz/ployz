//! Cloud Relay process: HTTP/1.1 behind a TLS terminator.

use std::{io, net::SocketAddr, process::ExitCode};

use clap::{Parser, Subcommand};
use ployz_relay::{DialCredential, Relay, RelayError};

const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
const DIAL_ENV: &str = "PLOYZ_RELAY_DIAL_CREDENTIAL";

#[derive(Parser)]
#[command(about = "Ployz Cloud Relay")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, env = "PLOYZ_RELAY_LISTEN", default_value = DEFAULT_LISTEN)]
    listen: SocketAddr,
}

#[derive(Subcommand)]
enum Command {
    /// Print the Relay version.
    Version,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Error> {
    let args = Args::parse();
    if matches!(args.command, Some(Command::Version)) {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let dial = DialCredential::parse(env_or_empty(DIAL_ENV))?;
    let relay = Relay::new(dial);
    let (_address, handle, goaway) = relay.serve(args.listen).await?;
    shutdown_signal().await?;
    goaway.start();
    handle.await.expect("relay serve task panicked")?;
    Ok(())
}

fn env_or_empty(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

async fn shutdown_signal() -> io::Result<()> {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = ctrl_c => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Relay(#[from] RelayError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

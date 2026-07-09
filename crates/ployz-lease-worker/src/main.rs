use std::net::SocketAddr;
use std::sync::Arc;

use ployz_lease_worker::{StubLeaseWorker, serve};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), MainError> {
    let addr = std::env::var("PLOYZ_LEASE_WORKER_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8089".to_owned())
        .parse::<SocketAddr>()?;
    let listener = TcpListener::bind(addr).await.map_err(MainError::Io)?;
    let worker = Arc::new(Mutex::new(StubLeaseWorker::new()));
    serve(listener, worker).await.map_err(MainError::Runtime)
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("invalid listen address: {0}")]
    ListenAddr(#[from] std::net::AddrParseError),
    #[error("{0}")]
    Io(std::io::Error),
    #[error("{0}")]
    Runtime(ployz_lease_worker::LeaseWorkerHttpError),
}

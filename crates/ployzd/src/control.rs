mod protocol;
mod status;
mod stdio;

use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncWrite};

use crate::{DaemonConfig, DaemonRuntime};

pub use protocol::handle_protocol_request;
pub use stdio::run_rpc_stdio;

#[derive(Debug, Error)]
pub enum DaemonControlError {
    #[error("{0}")]
    Daemon(#[from] crate::DaemonError),

    #[error("{0}")]
    Primitive(#[from] ployz::PrimitiveFailure),

    #[error("failed to read rpc-stdio input: {0}")]
    Read(std::io::Error),

    #[error("failed to encode rpc-stdio output: {0}")]
    Encode(ployz_api::ProtocolError),

    #[error("failed to write rpc-stdio output: {0}")]
    Write(std::io::Error),
}

pub async fn serve_rpc_stdio<R, W>(
    config: DaemonConfig,
    reader: R,
    writer: W,
) -> Result<(), DaemonControlError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let runtime = DaemonRuntime::start(config).await?;
    let service = runtime.command_service()?;
    let result = run_rpc_stdio(reader, writer, |request| {
        let service = service.clone();
        async move { handle_protocol_request(&service, request).await }
    })
    .await;
    let shutdown = runtime.shutdown().await;
    result?;
    shutdown?;
    Ok(())
}

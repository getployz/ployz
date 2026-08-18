//! Opaque Dial/Attach frames as a single byte pipe.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_stream::StreamExt;
use tonic::{Streaming, transport::server::Connected};

use crate::TunnelFrame;

/// Byte pipe over a Dial or Attach `TunnelFrame` stream.
pub struct TunnelIo {
    io: DuplexStream,
    splice: JoinHandle<()>,
}

impl TunnelIo {
    /// Forward opaque frames to a duplex byte pipe.
    #[must_use]
    pub fn new(tx: mpsc::Sender<TunnelFrame>, inbound: Streaming<TunnelFrame>) -> Self {
        let (io, other) = tokio::io::duplex(64 * 1024);
        Self {
            io,
            splice: tokio::spawn(copy_tunnel(other, tx, inbound)),
        }
    }
}

impl Drop for TunnelIo {
    fn drop(&mut self) {
        self.splice.abort();
    }
}

impl Connected for TunnelIo {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl AsyncRead for TunnelIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(context, buffer)
    }
}

impl AsyncWrite for TunnelIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(context)
    }
}

async fn copy_tunnel(
    io: DuplexStream,
    tx: mpsc::Sender<TunnelFrame>,
    mut inbound: Streaming<TunnelFrame>,
) {
    let (mut reader, mut writer) = tokio::io::split(io);
    let to_relay = async {
        let mut buf = vec![0_u8; 16 * 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let Some(data) = buf.get(..n) else { break };
                    if tx.send(TunnelFrame::new(data.to_vec())).await.is_err() {
                        break;
                    }
                }
            }
        }
    };
    let from_relay = async {
        while let Some(Ok(frame)) = inbound.next().await {
            if writer.write_all(&frame.data).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    };
    tokio::join!(to_relay, from_relay);
}

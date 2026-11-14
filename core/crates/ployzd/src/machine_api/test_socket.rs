//! A Machine API served over a local socket in tests, the one caller besides a snapshot
//! key's direct connection that a daemon gives Full scope.

use std::path::{Path, PathBuf};

use ployz_core::MachineRpcClient;
use tokio::task::JoinHandle;
use tonic::transport::{Channel, Server};

use super::MachineApi;

struct Connector(PathBuf);

impl tower_service::Service<tonic::transport::Uri> for Connector {
    type Response = hyper_util::rt::TokioIo<tokio::net::UnixStream>;
    type Error = std::io::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<Self::Response>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: tonic::transport::Uri) -> Self::Future {
        let path = self.0.clone();
        Box::pin(async move {
            tokio::net::UnixStream::connect(path)
                .await
                .map(hyper_util::rt::TokioIo::new)
        })
    }
}

/// A socket path of its own under `dir`, short enough for a Unix socket.
pub(crate) fn socket_in(dir: &Path) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dir.join(format!("m{next}.sock"))
}

/// `api` served on `socket`, and a client of it. A request with no Operator header reaches
/// it as a Full caller, and one with it as Cloud does.
pub(crate) async fn served(
    api: MachineApi,
    socket: &Path,
) -> (
    MachineRpcClient<Channel>,
    JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let server = tokio::spawn(Server::builder().serve_with_incoming(
        api,
        tokio_stream::wrappers::UnixListenerStream::new(listener),
    ));
    (MachineRpcClient::new(channel_to(socket).await), server)
}

/// A channel to the Machine API served on `socket`.
pub(crate) async fn channel_to(socket: &Path) -> Channel {
    tonic::transport::Endpoint::try_from("http://machine.invalid")
        .unwrap()
        .connect_with_connector(Connector(socket.to_owned()))
        .await
        .unwrap()
}

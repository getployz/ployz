//! In-memory RPC transport for tests that advance Tokio time.

use std::{
    convert::Infallible,
    future::Future,
    sync::{Arc, Mutex},
};

use futures_util::StreamExt;
use ployz_core::OpaquePayload;
use tonic::{
    Request, Response, Status,
    codec::ProstCodec,
    transport::{Channel, Server},
};

use super::{BoxProxyStream, Client, ConnectError, Connector};
use crate::context::{Connection, ConnectionSource};

struct Connected(Channel);

#[tonic::async_trait]
impl Connector for Connected {
    async fn connect(&self, _: &Connection) -> Result<Channel, ConnectError> {
        Ok(self.0.clone())
    }
    async fn dial_proxy(
        &self,
        _: &Connection,
        _: &str,
        _: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        unreachable!("this RPC fixture does not open a proxy")
    }
}

pub(crate) async fn rpc_client<F, Fut>(
    rpc: F,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
)
where
    F: Fn(Request<OpaquePayload>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Response<OpaquePayload>, Status>> + Send + 'static,
{
    let rpc = tower::service_fn(rpc);
    let service = tower::service_fn(move |request: http::Request<tonic::body::Body>| {
        let rpc = rpc.clone();
        async move {
            Ok::<_, Infallible>(
                tonic::server::Grpc::new(ProstCodec::default())
                    .unary(rpc, request)
                    .await,
            )
        }
    });
    // Both peers run inside Tokio; clock advancement cannot outrun socket I/O.
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let incoming =
        tokio_stream::once(Ok::<_, std::io::Error>(server_io)).chain(tokio_stream::pending());
    let server = tokio::spawn(Server::builder().serve_with_incoming(service, incoming));
    let client_io = Arc::new(Mutex::new(Some(client_io)));
    let channel = Channel::from_static("http://memory.invalid")
        .connect_with_connector(tower::service_fn(move |_| {
            std::future::ready(Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(
                client_io.lock().unwrap().take().unwrap(),
            )))
        }))
        .await
        .unwrap();
    let client = Client::new(
        channel.clone(),
        Connection::tcp("127.0.0.1:1".parse().unwrap()),
        ConnectionSource::Direct,
        Arc::new(Connected(channel)),
    );
    (client, server)
}

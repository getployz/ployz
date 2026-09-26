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

/// One daemon generation on a real Unix socket. Its runtime stands in for the
/// process: shutting it down drops every connection without an answer.
/// Connection confirmation is answered here; `rpc` serves every other request.
pub(crate) fn unix_daemon<F, Fut>(path: &std::path::Path, rpc: F) -> tokio::runtime::Runtime
where
    F: Fn(ployz_core::RpcRequestBody) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<ployz_core::RpcResponse, Status>> + Send + 'static,
{
    use ployz_core::{RpcRequestBody, RpcResponse};
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let listener = {
        let _entered = runtime.enter();
        tokio::net::UnixListener::bind(path).unwrap()
    };
    let rpc = tower::service_fn(move |request: Request<OpaquePayload>| {
        let rpc = rpc.clone();
        async move {
            #[expect(
                clippy::wildcard_enum_match_arm,
                reason = "the fixture answers confirmation and forwards every other request"
            )]
            let response = match request.into_inner().decode_request().unwrap().body {
                RpcRequestBody::DescribeContract(_) => {
                    RpcResponse::from(ployz_core::ContractDescription {
                        machine_id: ployz_core::MachineId::random(),
                        protocol_major: ployz_core::PROTOCOL_MAJOR,
                        daemon_version: "fixture".into(),
                        capabilities: Default::default(),
                    })
                }
                body => rpc(body).await?,
            };
            Ok::<_, Status>(Response::new(response.encode().unwrap()))
        }
    });
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
    runtime.spawn(Server::builder().serve_with_incoming(
        service,
        tokio_stream::wrappers::UnixListenerStream::new(listener),
    ));
    runtime
}

/// A client confirmed against the daemon at `path`.
pub(crate) async fn unix_client(path: &std::path::Path) -> Client {
    let selected = crate::context::SelectedConnections {
        source: ConnectionSource::Direct,
        connections: vec![Connection::unix(path).unwrap()],
    };
    super::connect_selected_with(selected, Arc::new(super::SystemConnector::default()))
        .await
        .unwrap()
}

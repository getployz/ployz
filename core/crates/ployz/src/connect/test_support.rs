//! In-memory RPC transport for tests that advance Tokio time.

use std::{
    convert::Infallible,
    future::Future,
    sync::{Arc, Mutex},
};

use futures_util::{Stream, StreamExt};
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

/// A connector whose every dial opens `channel`.
pub(crate) fn connector(channel: Channel) -> Arc<dyn Connector> {
    Arc::new(Connected(channel))
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
    connected_client(service, false).await
}

pub(crate) async fn rpc_stream_client<F, Fut, S>(
    rpc: F,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
)
where
    F: Fn(Request<OpaquePayload>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Response<S>, Status>> + Send + 'static,
    S: Stream<Item = Result<OpaquePayload, Status>> + Send + 'static,
{
    let rpc = tower::service_fn(rpc);
    let service = tower::service_fn(move |request: http::Request<tonic::body::Body>| {
        let rpc = rpc.clone();
        async move {
            Ok::<_, Infallible>(
                tonic::server::Grpc::new(ProstCodec::default())
                    .server_streaming(rpc, request)
                    .await,
            )
        }
    });
    connected_client(service, false).await
}

pub(crate) async fn rpc_bidi_client<F, Fut, S>(
    rpc: F,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
)
where
    F: Fn(Request<tonic::Streaming<OpaquePayload>>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Response<S>, Status>> + Send + 'static,
    S: Stream<Item = Result<OpaquePayload, Status>> + Send + 'static,
{
    let rpc = tower::service_fn(rpc);
    let service = tower::service_fn(move |request: http::Request<tonic::body::Body>| {
        let rpc = rpc.clone();
        async move {
            Ok::<_, Infallible>(
                tonic::server::Grpc::new(ProstCodec::default())
                    .streaming(rpc, request)
                    .await,
            )
        }
    });
    connected_client(service, false).await
}

/// The log routes, served as streams.
pub(crate) const LOG_ROUTES: &[&str] = &[
    ployz_core::op::TailLogs::PATH,
    ployz_core::op::LogHistory::PATH,
];

/// One fake Machine answering the calls on `streams` with `stream`, and every
/// other call with `unary`. Aborting the returned task drops its connection.
pub(crate) async fn rpc_routes<U, UFut, F, Fut, S>(
    unary: U,
    streams: &'static [&'static str],
    stream: F,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
)
where
    U: Fn(Request<OpaquePayload>) -> UFut + Clone + Send + Sync + 'static,
    UFut: Future<Output = Result<Response<OpaquePayload>, Status>> + Send + 'static,
    F: Fn(Request<tonic::Streaming<OpaquePayload>>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Response<S>, Status>> + Send + 'static,
    S: Stream<Item = Result<OpaquePayload, Status>> + Send + 'static,
{
    let unary = tower::service_fn(unary);
    let stream = tower::service_fn(stream);
    let service = tower::service_fn(move |request: http::Request<tonic::body::Body>| {
        let unary = unary.clone();
        let stream = stream.clone();
        async move {
            let mut grpc = tonic::server::Grpc::new(ProstCodec::default());
            // A server stream is a bidi stream whose client sends one message.
            Ok::<_, Infallible>(if streams.contains(&request.uri().path()) {
                grpc.streaming(stream, request).await
            } else {
                grpc.unary(unary, request).await
            })
        }
    });
    connected_client(service, true).await
}

/// A channel to an in-memory daemon that answers only its own confirmation.
pub(crate) async fn confirming_channel() -> Channel {
    use ployz_core::{RpcRequestBody, RpcResponse};
    let rpc = tower::service_fn(|request: Request<OpaquePayload>| async move {
        let RpcRequestBody::DescribeContract(_) =
            request.into_inner().decode_request().unwrap().body
        else {
            panic!("connect asks only for confirmation")
        };
        let response = RpcResponse::from(ployz_core::ContractDescription {
            machine_id: ployz_core::MachineId::random(),
            protocol_major: ployz_core::PROTOCOL_MAJOR,
            daemon_version: "fixture".into(),
            capabilities: Default::default(),
        });
        Ok::<_, Status>(Response::new(response.encode().unwrap()))
    });
    let service = tower::service_fn(
        move |request: http::Request<tonic::body::Body>| async move {
            Ok::<_, Infallible>(
                tonic::server::Grpc::new(ProstCodec::default())
                    .unary(rpc, request)
                    .await,
            )
        },
    );
    memory_channel(service, false).await.0
}

async fn memory_channel<S>(
    service: S,
    severable: bool,
) -> (
    Channel,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
)
where
    S: tower::Service<
            http::Request<tonic::body::Body>,
            Response = http::Response<tonic::body::Body>,
            Error = Infallible,
        > + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    // Both peers run inside Tokio; clock advancement cannot outrun socket I/O.
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (server_io, link) = if severable {
        // The returned task carries the bytes between the peers, so aborting it
        // drops the connection without an answer, as a lost network does.
        let (mut relayed, far) = tokio::io::duplex(64 * 1024);
        let mut near = server_io;
        let link = async move {
            let _ = tokio::io::copy_bidirectional(&mut near, &mut relayed).await;
        };
        (far, Some(link))
    } else {
        (server_io, None)
    };
    let incoming =
        tokio_stream::once(Ok::<_, std::io::Error>(server_io)).chain(tokio_stream::pending());
    let serve = Server::builder().serve_with_incoming(service, incoming);
    let server = tokio::spawn(async move {
        match link {
            Some(link) => tokio::join!(serve, link).0,
            None => serve.await,
        }
    });
    let client_io = Arc::new(Mutex::new(Some(client_io)));
    let channel = Channel::from_static("http://memory.invalid")
        .connect_with_connector(tower::service_fn(move |_| {
            std::future::ready(Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(
                client_io.lock().unwrap().take().unwrap(),
            )))
        }))
        .await
        .unwrap();
    (channel, server)
}

async fn connected_client<S>(
    service: S,
    severable: bool,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
)
where
    S: tower::Service<
            http::Request<tonic::body::Body>,
            Response = http::Response<tonic::body::Body>,
            Error = Infallible,
        > + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    let (channel, server) = memory_channel(service, severable).await;
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

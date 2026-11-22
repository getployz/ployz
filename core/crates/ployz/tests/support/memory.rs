//! A Client wired to a fake Machine over in-process streams, so a paused clock never
//! outruns OS I/O. Transport tests dial a real listener instead.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use ployz::{
    connect::{BoxProxyStream, Client, ConnectError, Connector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
};
use ployz_core::MachineRpcServer;
use ployz_testkit::fake_machine::{Fake, FakeMachine};
use tokio::sync::mpsc;
use tonic::transport::{Channel, Server};

type Incoming = mpsc::UnboundedSender<Result<tokio::io::DuplexStream, std::io::Error>>;

struct MemoryConnector {
    incoming: Incoming,
    connects: Arc<AtomicUsize>,
}

#[tonic::async_trait]
impl Connector for MemoryConnector {
    async fn connect(&self, _: &Connection) -> Result<Channel, ConnectError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let incoming = self.incoming.clone();
        Channel::from_static("http://memory.invalid")
            .connect_with_connector(tower::service_fn(move |_| {
                let (client, server) = tokio::io::duplex(64 * 1024);
                incoming.send(Ok(server)).unwrap();
                async move { Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(client)) }
            }))
            .await
            .map_err(Into::into)
    }

    async fn dial_proxy(
        &self,
        _: &Connection,
        _: &str,
        _: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        Err(ConnectError::Attempt("unused".into()))
    }
}

/// A Client of `service`, the server task, and how many times the Client dialed.
pub async fn connected_in_memory<T: FakeMachine>(
    service: T,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    Arc<AtomicUsize>,
) {
    let (incoming, receiver) = mpsc::unbounded_channel();
    let server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(Fake(service)))
            .serve_with_incoming(tokio_stream::wrappers::UnboundedReceiverStream::new(
                receiver,
            )),
    );
    let connects = Arc::new(AtomicUsize::new(0));
    let client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp("127.0.0.1:1".parse().unwrap())],
        },
        Arc::new(MemoryConnector {
            incoming,
            connects: connects.clone(),
        }),
    )
    .await
    .unwrap();
    (client, server, connects)
}

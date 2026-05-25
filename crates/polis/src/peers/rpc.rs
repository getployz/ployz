use irpc::{Client, LocalSender, WithChannels, channel::oneshot, rpc::RemoteService, rpc_requests};
use irpc_iroh::{IrohLazyRemoteConnection, IrohProtocol};
use serde::{Deserialize, Serialize};

use crate::IrohEndpointId;

use super::{
    PLOYZ_PEER_ALPN, PeerEndpoint, PeerError, PeerProbe, PeerProbeDeadline, PeerProbeReceipt,
    PeerProbeResult, PeerTicket, PeerTicketPath,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerPreflightRequest;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PeerPreflightReceipt;

#[rpc_requests(message = PeerRpcMessage, no_spans)]
#[derive(Debug, Serialize, Deserialize)]
enum PeerRpcProtocol {
    #[rpc(tx=oneshot::Sender<PeerPreflightReceipt>)]
    Preflight(PeerPreflightRequest),
}

#[derive(Clone)]
pub struct PeerRpcClient {
    client: Client<PeerRpcProtocol>,
}

pub(crate) struct PeerRpcServer {
    client: PeerRpcClient,
    sender: LocalSender<PeerRpcProtocol>,
    actor: tokio::task::JoinHandle<()>,
}

pub struct PeerRpcListener {
    server: PeerRpcServer,
    router: iroh::protocol::Router,
}

impl PeerRpcListener {
    pub fn start(endpoint: PeerEndpoint) -> PeerProbeResult<Self> {
        let server = PeerRpcServer::spawn()?;
        let router = iroh::protocol::Router::builder(endpoint)
            .accept(Self::ALPN, server.protocol_handler())
            .spawn();

        Ok(Self { server, router })
    }

    pub const ALPN: &'static [u8] = PLOYZ_PEER_ALPN;

    pub async fn shutdown(self, deadline: PeerProbeDeadline) -> PeerProbeResult<()> {
        let Self { server, router } = self;
        tokio::time::timeout(deadline.duration(), router.shutdown())
            .await
            .map_err(|_| PeerError::RpcTimeout)?
            .map_err(|error| PeerError::RpcRuntime {
                message: error.to_string(),
            })?;
        server.shutdown(deadline).await
    }
}

pub struct PeerRpcProbe {
    endpoint: IrohEndpointId,
    observed_path: PeerTicketPath,
    client: PeerRpcClient,
}

impl PeerRpcProbe {
    pub fn connect(endpoint: PeerEndpoint, ticket: &PeerTicket) -> PeerProbeResult<Self> {
        Self::from_service(
            ticket.endpoint_id(),
            ticket.path(),
            PeerRpcClient::connect(endpoint, ticket.endpoint_addr().clone()),
        )
    }

    pub(crate) fn from_service(
        endpoint: IrohEndpointId,
        observed_path: PeerTicketPath,
        client: PeerRpcClient,
    ) -> PeerProbeResult<Self> {
        Ok(Self {
            endpoint,
            observed_path,
            client,
        })
    }
}

impl PeerProbe for PeerRpcProbe {
    async fn probe(
        &self,
        target: &IrohEndpointId,
        deadline: PeerProbeDeadline,
    ) -> PeerProbeResult<PeerProbeReceipt> {
        if target != &self.endpoint {
            return Err(PeerError::ProbeFailed {
                endpoint: target.clone(),
                reason: "peer rpc probe was built for a different endpoint".to_string(),
            });
        }

        self.client.preflight(deadline).await?;
        Ok(PeerProbeReceipt::new(target.clone(), self.observed_path))
    }
}

impl PeerRpcClient {
    pub const ALPN: &'static [u8] = PLOYZ_PEER_ALPN;

    #[must_use]
    pub fn connect(endpoint: iroh::Endpoint, addr: impl Into<iroh::EndpointAddr>) -> Self {
        let connection = IrohLazyRemoteConnection::new(endpoint, addr.into(), Self::ALPN.to_vec());
        Self {
            client: Client::boxed(connection),
        }
    }

    pub async fn preflight(&self, deadline: PeerProbeDeadline) -> PeerProbeResult<()> {
        tokio::time::timeout(deadline.duration(), self.client.rpc(PeerPreflightRequest))
            .await
            .map_err(|_| PeerError::RpcTimeout)?
            .map(|_| ())
            .map_err(|error| PeerError::RpcTransport {
                message: error.to_string(),
            })
    }
}

impl PeerRpcServer {
    pub fn spawn() -> PeerProbeResult<Self> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let handle =
            tokio::runtime::Handle::try_current().map_err(|error| PeerError::RpcRuntime {
                message: error.to_string(),
            })?;
        let actor = handle.spawn(run_peer_rpc_actor(rx));
        Ok(Self {
            client: PeerRpcClient {
                client: Client::local(tx.clone()),
            },
            sender: LocalSender::from(tx),
            actor,
        })
    }

    #[cfg(test)]
    #[must_use]
    pub fn client(&self) -> PeerRpcClient {
        self.client.clone()
    }

    pub fn protocol_handler(&self) -> impl iroh::protocol::ProtocolHandler {
        IrohProtocol::new(PeerRpcProtocol::remote_handler(self.sender.clone()))
    }

    pub async fn shutdown(self, deadline: PeerProbeDeadline) -> PeerProbeResult<()> {
        let Self {
            client,
            sender,
            actor,
        } = self;
        drop(client);
        drop(sender);
        tokio::time::timeout(deadline.duration(), actor)
            .await
            .map_err(|_| PeerError::RpcTimeout)?
            .map_err(|error| PeerError::RpcRuntime {
                message: error.to_string(),
            })?;
        Ok(())
    }
}

async fn run_peer_rpc_actor(mut rx: tokio::sync::mpsc::Receiver<PeerRpcMessage>) {
    while let Some(message) = rx.recv().await {
        match message {
            PeerRpcMessage::Preflight(message) => {
                let WithChannels { tx, .. } = message;
                let _ = tx.send(PeerPreflightReceipt).await;
            }
        }
    }
}

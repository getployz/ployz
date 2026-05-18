use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use futures_util::StreamExt;
use p2panda_core_git::{Operation, SigningKey, Topic};
use p2panda_net::addrs::NodeInfo;
use p2panda_net::iroh_endpoint::{EndpointAddr, IrohConfig, from_verifying_key};
use p2panda_net::{AddressBook, Discovery, Endpoint, Gossip, LogSync, NetworkId};
use p2panda_store_git::SqliteStore;
use p2panda_sync_git::FromSync;
use p2panda_sync_git::protocols::TopicLogSyncEvent;
use tokio::time::timeout;

use crate::{PandaNetLogId, PandaNetQuarantineLog, PandaNetStartupStep, PandaNetTransportError};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const STREAM_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PandaNetNetworkId([u8; 32]);

impl PandaNetNetworkId {
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    fn into_inner(self) -> NetworkId {
        self.0
    }
}

impl From<[u8; 32]> for PandaNetNetworkId {
    fn from(bytes: [u8; 32]) -> Self {
        Self::new(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PandaNetNodeSeed([u8; 32]);

impl PandaNetNodeSeed {
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    fn signing_key(self) -> SigningKey {
        SigningKey::from_bytes(&self.0)
    }
}

impl From<[u8; 32]> for PandaNetNodeSeed {
    fn from(bytes: [u8; 32]) -> Self {
        Self::new(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PandaNetTopic([u8; 32]);

impl PandaNetTopic {
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    fn into_inner(self) -> Topic {
        self.0.into()
    }
}

impl From<[u8; 32]> for PandaNetTopic {
    fn from(bytes: [u8; 32]) -> Self {
        Self::new(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PandaNetBindConfig {
    pub ipv4: Ipv4Addr,
    pub port_v4: u16,
    pub ipv6: Ipv6Addr,
    pub port_v6: u16,
}

impl PandaNetBindConfig {
    #[must_use]
    pub fn localhost(port_v4: u16, port_v6: u16) -> Self {
        Self {
            ipv4: Ipv4Addr::LOCALHOST,
            port_v4,
            ipv6: Ipv6Addr::LOCALHOST,
            port_v6,
        }
    }

    #[must_use]
    pub fn localhost_ephemeral() -> Self {
        Self::localhost(0, 0)
    }

    fn iroh_config(self) -> IrohConfig {
        IrohConfig {
            bind_ip_v4: self.ipv4,
            bind_port_v4: self.port_v4,
            bind_ip_v6: self.ipv6,
            bind_port_v6: self.port_v6,
        }
    }
}

#[derive(Clone)]
pub struct PandaNetNodeInfo(NodeInfo);

impl PandaNetNodeInfo {
    #[must_use]
    pub(crate) fn into_inner(self) -> NodeInfo {
        self.0
    }
}

#[derive(Clone)]
pub struct PandaNetNodeConfig {
    network_id: PandaNetNetworkId,
    signing_key: SigningKey,
    bind: PandaNetBindConfig,
    bootstrap_nodes: Vec<PandaNetNodeInfo>,
}

impl PandaNetNodeConfig {
    #[must_use]
    pub fn localhost(
        network_id: PandaNetNetworkId,
        signing_key_seed: PandaNetNodeSeed,
        bind: PandaNetBindConfig,
        bootstrap_nodes: Vec<PandaNetNodeInfo>,
    ) -> Self {
        Self {
            network_id,
            signing_key: signing_key_seed.signing_key(),
            bind,
            bootstrap_nodes,
        }
    }

    #[must_use]
    pub fn localhost_ephemeral(
        network_id: PandaNetNetworkId,
        signing_key_seed: PandaNetNodeSeed,
        bootstrap_nodes: Vec<PandaNetNodeInfo>,
    ) -> Self {
        Self::localhost(
            network_id,
            signing_key_seed,
            PandaNetBindConfig::localhost_ephemeral(),
            bootstrap_nodes,
        )
    }
}

pub struct PandaNetNode {
    address_book: AddressBook,
    discovery: Discovery,
    endpoint: Endpoint,
    gossip: Gossip,
    log_sync: LogSync<SqliteStore, PandaNetLogId, ()>,
    quarantine_log: PandaNetQuarantineLog,
    node_info: PandaNetNodeInfo,
}

impl PandaNetNode {
    pub async fn spawn(config: PandaNetNodeConfig) -> Result<Self, PandaNetTransportError> {
        let quarantine_log = PandaNetQuarantineLog::new(config.signing_key.clone()).await?;
        let signing_key = config.signing_key.clone();

        let address_book = with_startup_timeout(
            PandaNetStartupStep::AddressBook,
            AddressBook::builder().spawn(),
        )
        .await?;

        for bootstrap in config.bootstrap_nodes {
            with_startup_timeout(
                PandaNetStartupStep::BootstrapNode,
                address_book.insert_node_info(bootstrap.into_inner()),
            )
            .await?;
        }

        let endpoint = with_startup_timeout(
            PandaNetStartupStep::Endpoint,
            Endpoint::builder(address_book.clone())
                .network_id(config.network_id.into_inner())
                .config(config.bind.iroh_config())
                .signing_key(config.signing_key)
                .spawn(),
        )
        .await?;
        let node_info = node_info_from_endpoint(&signing_key, &endpoint).await?;

        let discovery = with_startup_timeout(
            PandaNetStartupStep::Discovery,
            Discovery::builder(address_book.clone(), endpoint.clone()).spawn(),
        )
        .await?;

        let gossip = with_startup_timeout(
            PandaNetStartupStep::Gossip,
            Gossip::builder(address_book.clone(), endpoint.clone()).spawn(),
        )
        .await?;

        let log_sync = with_startup_timeout(
            PandaNetStartupStep::LogSync,
            LogSync::builder(quarantine_log.store(), endpoint.clone(), gossip.clone()).spawn(),
        )
        .await?;

        Ok(Self {
            address_book,
            discovery,
            endpoint,
            gossip,
            log_sync,
            quarantine_log,
            node_info,
        })
    }

    #[must_use]
    pub fn node_info(&self) -> PandaNetNodeInfo {
        self.node_info.clone()
    }

    pub async fn open_stream(
        &self,
        topic: PandaNetTopic,
        live_mode: bool,
    ) -> Result<PandaNetStream, PandaNetTransportError> {
        let topic = topic.into_inner();
        let handle = with_startup_timeout(
            PandaNetStartupStep::Stream,
            self.log_sync.stream(topic, live_mode),
        )
        .await?;
        let subscription =
            with_startup_timeout(PandaNetStartupStep::Stream, handle.subscribe()).await?;
        Ok(PandaNetStream {
            topic,
            _handle: handle,
            subscription,
        })
    }

    pub async fn append_to_topic(
        &mut self,
        topic: PandaNetTopic,
        body: &[u8],
    ) -> Result<(), PandaNetTransportError> {
        let topic = topic.into_inner();
        self.quarantine_log
            .append_to_topic(&topic, body, 0)
            .await
            .map(|_| ())
    }
}

impl Drop for PandaNetNode {
    fn drop(&mut self) {
        let _ = (
            &self.address_book,
            &self.discovery,
            &self.endpoint,
            &self.gossip,
        );
    }
}

pub struct PandaNetStream {
    topic: Topic,
    _handle: p2panda_net::sync::SyncHandle<Operation<()>, TopicLogSyncEvent>,
    subscription: p2panda_net::sync::SyncSubscription<TopicLogSyncEvent>,
}

pub(crate) enum PandaNetStreamBody {
    Body(Vec<u8>),
    TooLarge { size: usize, max: usize },
}

impl PandaNetStream {
    pub async fn next_body(&mut self) -> Result<Vec<u8>, PandaNetTransportError> {
        match self.next_body_limited(usize::MAX).await? {
            PandaNetStreamBody::Body(body) => Ok(body),
            PandaNetStreamBody::TooLarge { .. } => unreachable!("usize::MAX accepts every body"),
        }
    }

    pub(crate) async fn next_body_limited(
        &mut self,
        max_body_bytes: usize,
    ) -> Result<PandaNetStreamBody, PandaNetTransportError> {
        loop {
            let event = timeout(STREAM_TIMEOUT, self.subscription.next())
                .await
                .map_err(|_| PandaNetTransportError::Timeout {
                    step: PandaNetStartupStep::Stream,
                    timeout_ms: STREAM_TIMEOUT.as_millis() as u64,
                })?
                .ok_or(PandaNetTransportError::StreamEnded { topic: self.topic })?
                .map_err(|error| PandaNetTransportError::StreamLagged {
                    topic: self.topic,
                    message: error.to_string(),
                })?;

            match event {
                FromSync {
                    event: TopicLogSyncEvent::OperationReceived { operation, .. },
                    ..
                } => {
                    let body_size =
                        usize::try_from(operation.header.payload_size).unwrap_or(usize::MAX);
                    if body_size > max_body_bytes {
                        return Ok(PandaNetStreamBody::TooLarge {
                            size: body_size,
                            max: max_body_bytes,
                        });
                    }
                    let Some(body) = operation.body else {
                        return Err(PandaNetTransportError::MissingBody { topic: self.topic });
                    };
                    return Ok(PandaNetStreamBody::Body(body.to_bytes()));
                }
                FromSync {
                    event: TopicLogSyncEvent::Failed { error },
                    ..
                } => {
                    return Err(PandaNetTransportError::StreamFailed {
                        topic: self.topic,
                        message: error.to_string(),
                    });
                }
                FromSync {
                    event:
                        TopicLogSyncEvent::SyncFinished { .. }
                        | TopicLogSyncEvent::SessionFinished { .. }
                        | TopicLogSyncEvent::LiveModeStarted
                        | TopicLogSyncEvent::SessionStarted
                        | TopicLogSyncEvent::SyncStarted { .. },
                    ..
                } => {}
            }
        }
    }
}

async fn node_info_from_endpoint(
    signing_key: &SigningKey,
    endpoint: &Endpoint,
) -> Result<PandaNetNodeInfo, PandaNetTransportError> {
    let iroh_endpoint =
        with_startup_timeout(PandaNetStartupStep::Endpoint, endpoint.endpoint()).await?;
    let Some(bound_socket) = preferred_bound_socket(iroh_endpoint.bound_sockets()) else {
        return Err(PandaNetTransportError::Startup {
            step: PandaNetStartupStep::Endpoint,
            message: "endpoint reported no bound sockets".to_string(),
        });
    };
    let endpoint_addr = EndpointAddr::new(from_verifying_key(signing_key.verifying_key()))
        .with_ip_addr(bound_socket);
    Ok(PandaNetNodeInfo(NodeInfo::from(endpoint_addr).bootstrap()))
}

fn preferred_bound_socket(bound_sockets: Vec<SocketAddr>) -> Option<SocketAddr> {
    bound_sockets
        .iter()
        .copied()
        .find(SocketAddr::is_ipv4)
        .or_else(|| bound_sockets.first().copied())
}

async fn with_startup_timeout<T, E: ToString>(
    step: PandaNetStartupStep,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, PandaNetTransportError> {
    timeout(STARTUP_TIMEOUT, future)
        .await
        .map_err(|_| PandaNetTransportError::Timeout {
            step,
            timeout_ms: STARTUP_TIMEOUT.as_millis() as u64,
        })?
        .map_err(|error| PandaNetTransportError::startup(step, error))
}

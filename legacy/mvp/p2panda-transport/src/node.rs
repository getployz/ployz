use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use p2panda_core::cbor::{decode_cbor, encode_cbor};
use p2panda_core::{Hash, SigningKey, Topic};
use p2panda_net::addrs::NodeInfo;
use p2panda_net::iroh_endpoint::{EndpointAddr, IrohConfig};
use p2panda_net::utils::from_verifying_key;
use p2panda_net::{Endpoint, NetworkId};
use thiserror::Error;
use tokio::time::timeout;

use crate::{PandaNetStartupStep, PandaNetTransportError};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const DEFAULT_REPLAY_CACHE_CAPACITY: usize = 8192;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PandaNetConfigError {
    #[error("{kind} must be 64 lowercase hex characters, got {got}")]
    InvalidHexLength { kind: &'static str, got: usize },
    #[error("{kind} contains invalid hex")]
    InvalidHex { kind: &'static str },
    #[error("encode p2panda node ticket: {message}")]
    TicketEncode { message: String },
    #[error("decode p2panda node ticket: {message}")]
    TicketDecode { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PandaNetNetworkId([u8; 32]);

impl PandaNetNetworkId {
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn parse_hex(value: &str) -> Result<Self, PandaNetConfigError> {
        parse_hex_32("network id", value).map(Self)
    }

    #[must_use]
    pub fn as_hex(self) -> String {
        encode_hex(&self.0)
    }

    pub(crate) fn into_inner(self) -> NetworkId {
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

    pub fn parse_hex(value: &str) -> Result<Self, PandaNetConfigError> {
        parse_hex_32("node seed", value).map(Self)
    }

    #[must_use]
    pub fn as_hex(self) -> String {
        encode_hex(&self.0)
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

    pub fn parse_hex(value: &str) -> Result<Self, PandaNetConfigError> {
        parse_hex_32("topic", value).map(Self)
    }

    #[must_use]
    pub fn as_hex(self) -> String {
        encode_hex(&self.0)
    }

    pub(crate) fn into_inner(self) -> Topic {
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

    pub(crate) fn iroh_config(self) -> IrohConfig {
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

    pub fn to_ticket(&self) -> Result<PandaNetNodeTicket, PandaNetConfigError> {
        PandaNetNodeTicket::new(self.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PandaNetNodeTicket {
    encoded: String,
}

impl PandaNetNodeTicket {
    pub fn from_socket_addr(
        signing_key_seed: PandaNetNodeSeed,
        socket_addr: SocketAddr,
    ) -> Result<Self, PandaNetConfigError> {
        let signing_key = signing_key_seed.signing_key();
        let endpoint_addr = EndpointAddr::new(from_verifying_key(signing_key.verifying_key()))
            .with_ip_addr(socket_addr);
        Self::new(PandaNetNodeInfo(NodeInfo::from(endpoint_addr).bootstrap()))
    }

    pub fn new(node_info: PandaNetNodeInfo) -> Result<Self, PandaNetConfigError> {
        let endpoint_addr = EndpointAddr::try_from(node_info.0).map_err(|error| {
            PandaNetConfigError::TicketEncode {
                message: error.to_string(),
            }
        })?;
        let bytes =
            encode_cbor(&endpoint_addr).map_err(|error| PandaNetConfigError::TicketEncode {
                message: error.to_string(),
            })?;
        Ok(Self {
            encoded: encode_hex(&bytes),
        })
    }

    pub fn parse(value: &str) -> Result<Self, PandaNetConfigError> {
        let ticket = Self {
            encoded: value.trim().to_string(),
        };
        let _node_info = ticket.node_info()?;
        Ok(ticket)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.encoded
    }

    pub fn into_node_info(self) -> Result<PandaNetNodeInfo, PandaNetConfigError> {
        self.node_info()
    }

    fn node_info(&self) -> Result<PandaNetNodeInfo, PandaNetConfigError> {
        let bytes = decode_hex(self.encoded.as_str(), "node ticket")?;
        let endpoint_addr = decode_cbor::<EndpointAddr, _>(&bytes[..]).map_err(|error| {
            PandaNetConfigError::TicketDecode {
                message: error.to_string(),
            }
        })?;
        Ok(PandaNetNodeInfo(NodeInfo::from(endpoint_addr).bootstrap()))
    }
}

#[derive(Clone)]
pub struct PandaNetNodeConfig {
    network_id: PandaNetNetworkId,
    signing_key: SigningKey,
    bind: PandaNetBindConfig,
    bootstrap_nodes: Vec<PandaNetNodeInfo>,
}

fn parse_hex_32(kind: &'static str, value: &str) -> Result<[u8; 32], PandaNetConfigError> {
    let bytes = decode_hex(value, kind)?;
    let Ok(bytes) = <[u8; 32]>::try_from(bytes.as_slice()) else {
        return Err(PandaNetConfigError::InvalidHexLength {
            kind,
            got: value.trim().len(),
        });
    };
    Ok(bytes)
}

fn decode_hex(value: &str, kind: &'static str) -> Result<Vec<u8>, PandaNetConfigError> {
    let trimmed = value.trim();
    if !trimmed.len().is_multiple_of(2) {
        return Err(PandaNetConfigError::InvalidHexLength {
            kind,
            got: trimmed.len(),
        });
    }
    let mut bytes = Vec::with_capacity(trimmed.len() / 2);
    for chunk in trimmed.as_bytes().chunks_exact(2) {
        let high = decode_hex_nibble(chunk[0]).ok_or(PandaNetConfigError::InvalidHex { kind })?;
        let low = decode_hex_nibble(chunk[1]).ok_or(PandaNetConfigError::InvalidHex { kind })?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
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

    pub(crate) fn clone_parts(
        &self,
    ) -> (
        PandaNetNetworkId,
        p2panda_core::SigningKey,
        PandaNetBindConfig,
        Vec<PandaNetNodeInfo>,
    ) {
        (
            self.network_id,
            self.signing_key.clone(),
            self.bind,
            self.bootstrap_nodes.clone(),
        )
    }
}

pub(crate) struct PandaNetReplayCache {
    order: VecDeque<Hash>,
    set: HashSet<Hash>,
    capacity: usize,
}

impl PandaNetReplayCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            order: VecDeque::new(),
            set: HashSet::new(),
            capacity,
        }
    }

    fn contains(&self, hash: Hash) -> bool {
        self.set.contains(&hash)
    }

    pub(crate) fn insert(&mut self, hash: Hash) {
        if self.capacity == 0 || !self.set.insert(hash) {
            return;
        }
        self.order.push_back(hash);
        while self.order.len() > self.capacity {
            let Some(expired) = self.order.pop_front() else {
                return;
            };
            self.set.remove(&expired);
        }
    }

    pub(crate) fn check_seen(&mut self, hash: Hash) -> PandaNetReplayStatus {
        if self.contains(hash) {
            return PandaNetReplayStatus::Replayed;
        }
        PandaNetReplayStatus::Fresh
    }
}

pub(crate) enum PandaNetReplayStatus {
    Fresh,
    Replayed,
}

pub(crate) async fn node_info_from_endpoint(
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

pub(crate) async fn with_startup_timeout<T, E: ToString>(
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

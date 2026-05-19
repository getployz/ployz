use mvp_acme::{AcmeHttp01ClearedFact, AcmeHttp01PresentedFact};
use mvp_identity::NodeId;
use mvp_lease::{LeaseClaimed, LeaseReleased, LeaseRenewed};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ServiceName(String);

impl ServiceName {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RouteId(String);

impl RouteId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectionFactPayload {
    NodeJoined(NodeJoinedFact),
    NodeRemovalStarted(NodeRemovalStartedFact),
    NodeTombstoned(NodeTombstonedFact),
    PeerAdmitted(PeerAdmittedFact),
    ServiceRegistered(ServiceRegistrationFact),
    ServingCommit(ServingCommitFact),
    RouteCommit(RouteCommitFact),
    GatewayCommit(GatewayCommitFact),
    DnsCommit(DnsCommitFact),
    LeaseClaimed(LeaseClaimed),
    LeaseRenewed(LeaseRenewed),
    LeaseReleased(LeaseReleased),
    AcmeHttp01Presented(AcmeHttp01PresentedFact),
    AcmeHttp01Cleared(AcmeHttp01ClearedFact),
}

impl ProjectionFactPayload {
    pub fn to_fact_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn from_fact_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeJoinedFact {
    pub node_id: NodeId,
    pub epoch: u64,
    pub overlay_ip: String,
    pub iroh_endpoint_id: String,
    pub wg_public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRemovalStartedFact {
    pub node_id: NodeId,
    pub epoch: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTombstonedFact {
    pub node_id: NodeId,
    pub epoch: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAdmittedFact {
    pub node_id: NodeId,
    pub principal_id: String,
    pub author_key_hex: String,
    pub p2panda_ticket: String,
    pub invite_id: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRegistrationFact {
    pub service: ServiceName,
    pub node_id: NodeId,
    pub version: String,
    pub endpoint_subject: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteCommitFact {
    pub route_commit_id: String,
    pub route_id: RouteId,
    pub hostnames: Vec<String>,
    pub backends: Vec<BackendEndpoint>,
    pub old_backends_to_drain: Vec<BackendEndpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServingCommitFact {
    pub serving_commit_id: String,
    pub route_commit_id: String,
    pub gateway_commit_id: String,
    pub dns_commit_id: String,
    pub route_id: RouteId,
    pub hostnames: Vec<String>,
    pub backends: Vec<BackendEndpoint>,
    pub old_backends_to_drain: Vec<BackendEndpoint>,
    pub dns_records: Vec<DnsRecordFact>,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BackendEndpoint {
    pub node_id: NodeId,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayCommitFact {
    pub gateway_commit_id: String,
    pub route_commit_id: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsCommitFact {
    pub dns_commit_id: String,
    pub epoch: u64,
    pub records: Vec<DnsRecordFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DnsRecordFact {
    pub name: String,
    pub record_type: String,
    pub value: String,
    pub ttl_seconds: u32,
}

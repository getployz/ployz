use std::collections::BTreeMap;

use mvp_acme::{AcmeChallengeToken, AcmeHostname, AcmeKeyAuthorization};
use mvp_bus::IslandId;
use mvp_lease::{LeaseContentHash, LeaseEpoch, LeaseHolder, LeaseTimestamp};
use serde::{Deserialize, Serialize};

use crate::facts::{BackendEndpoint, DnsRecordFact, NodeId, RouteId, ServiceName};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionState {
    pub island: IslandId,
    pub nodes: BTreeMap<NodeId, NodeProjection>,
    pub tombstoned_nodes: BTreeMap<NodeId, u64>,
    pub services: BTreeMap<(ServiceName, NodeId), ServiceProjection>,
    pub gateway: Option<GatewayProjection>,
    pub dns: Option<DnsProjection>,
    pub acme_http01: BTreeMap<AcmeHttp01ChallengeKey, AcmeHttp01ChallengeProjection>,
    pub statuses: Vec<ProjectionStatus>,
}

impl ProjectionState {
    #[must_use]
    pub fn for_island(island: IslandId) -> Self {
        Self {
            island,
            nodes: BTreeMap::new(),
            tombstoned_nodes: BTreeMap::new(),
            services: BTreeMap::new(),
            gateway: None,
            dns: None,
            acme_http01: BTreeMap::new(),
            statuses: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProjection {
    pub node_id: NodeId,
    pub epoch: u64,
    pub overlay_ip: String,
    pub iroh_endpoint_id: String,
    pub wg_public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceProjection {
    pub service: ServiceName,
    pub node_id: NodeId,
    pub version: String,
    pub endpoint_subject: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayProjection {
    pub gateway_commit_id: String,
    pub route_commit_id: String,
    pub routes: Vec<GatewayRouteProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GatewayRouteProjection {
    pub route_id: RouteId,
    pub hostnames: Vec<String>,
    pub backends: Vec<BackendEndpoint>,
    pub old_backends_to_drain: Vec<BackendEndpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsProjection {
    pub dns_commit_id: String,
    pub records: Vec<DnsRecordProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DnsRecordProjection {
    pub name: String,
    pub record_type: String,
    pub value: String,
    pub ttl_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AcmeHttp01ChallengeKey {
    pub hostname: AcmeHostname,
    pub token: AcmeChallengeToken,
}

impl AcmeHttp01ChallengeKey {
    #[must_use]
    pub fn new(hostname: AcmeHostname, token: AcmeChallengeToken) -> Self {
        Self { hostname, token }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AcmeHttp01ChallengeProjection {
    pub hostname: AcmeHostname,
    pub token: AcmeChallengeToken,
    pub key_authorization: AcmeKeyAuthorization,
    pub holder: LeaseHolder,
    pub lease_epoch: LeaseEpoch,
    pub claim_hash: LeaseContentHash,
    pub published_at: LeaseTimestamp,
    pub expires_at: LeaseTimestamp,
}

impl From<DnsRecordFact> for DnsRecordProjection {
    fn from(value: DnsRecordFact) -> Self {
        Self {
            name: value.name,
            record_type: value.record_type,
            value: value.value,
            ttl_seconds: value.ttl_seconds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProjectionStatus {
    pub reason: ProjectionIgnoreReason,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProjectionIgnoreReason {
    CrossIsland,
    Unverified,
    Unauthorized,
    Conflict,
    Superseded,
    MissingPayload,
    MalformedPayload,
    UnsupportedFactKind,
}

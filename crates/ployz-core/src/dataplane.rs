//! Dataplane preparation models.

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

use crate::deploy::DeployPlan;
use crate::ids::{MachineId, OperationId};
use crate::ops::FailureMessage;

pub const DEFAULT_WIREGUARD_LISTEN_PORT: u16 = 51820;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataplanePrepareRequest {
    pub operation_id: OperationId,
    pub membership: Vec<DataplaneMember>,
}

impl DataplanePrepareRequest {
    #[must_use]
    pub fn for_deploy_plan(
        operation_id: OperationId,
        plan: &DeployPlan,
        dataplane_machines: &[MachineId],
    ) -> Self {
        let machines = sorted_unique_machines(
            plan.target_machines()
                .into_iter()
                .chain(dataplane_machines.iter().cloned()),
        );
        Self::for_machines(operation_id, machines)
    }

    #[must_use]
    pub fn for_machines(operation_id: OperationId, machines: Vec<MachineId>) -> Self {
        Self {
            operation_id,
            membership: machines
                .into_iter()
                .map(DataplaneMember::default_for_machine)
                .collect(),
        }
    }

    #[must_use]
    pub fn machines(&self) -> Vec<MachineId> {
        self.membership
            .iter()
            .map(|member| member.machine_id.clone())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DataplaneMember {
    pub machine_id: MachineId,
    pub endpoint_subnet: MachineEndpointSubnet,
}

impl DataplaneMember {
    #[must_use]
    pub fn default_for_machine(machine_id: MachineId) -> Self {
        let endpoint_subnet = MachineEndpointSubnet::try_new(default_endpoint_subnet(&machine_id))
            .expect("default endpoint subnet is a valid CIDR");
        Self {
            machine_id,
            endpoint_subnet,
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineEndpointSubnet(IpNet);

impl MachineEndpointSubnet {
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, MachineEndpointSubnetError> {
        IpNet::from_str(value.as_ref())
            .map(Self)
            .map_err(|_| MachineEndpointSubnetError::Invalid {
                value: value.as_ref().to_owned(),
            })
    }

    #[must_use]
    pub const fn ipnet(&self) -> IpNet {
        self.0
    }

    #[must_use]
    pub fn as_string(&self) -> String {
        self.0.to_string()
    }
}

impl fmt::Debug for MachineEndpointSubnet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MachineEndpointSubnet")
            .field(&self.0.to_string())
            .finish()
    }
}

impl TryFrom<String> for MachineEndpointSubnet {
    type Error = MachineEndpointSubnetError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineEndpointSubnet> for String {
    fn from(value: MachineEndpointSubnet) -> Self {
        value.0.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineEndpointSubnetError {
    Invalid { value: String },
}

impl fmt::Display for MachineEndpointSubnetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { value } => {
                write!(formatter, "machine endpoint subnet {value:?} is not a CIDR")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "provider", content = "report", rename_all = "snake_case")]
pub enum DataplanePrepareProviderReport {
    PloyzNativeMesh(PloyzNativeMeshPrepareReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataplanePrepareError {
    Unavailable {
        machine_id: MachineId,
        provider: DataplaneProviderFailure,
        message: FailureMessage,
    },
    InvalidReport {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DataplaneProviderKind {
    PloyzNativeMesh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum DataplaneProviderFailure {
    PloyzNativeMesh { component: PloyzNativeMeshComponent },
}

impl DataplaneProviderFailure {
    #[must_use]
    pub const fn kind(&self) -> DataplaneProviderKind {
        match self {
            Self::PloyzNativeMesh { .. } => DataplaneProviderKind::PloyzNativeMesh,
        }
    }
}

impl From<WireGuardEbpfPrepareError> for DataplanePrepareError {
    fn from(value: WireGuardEbpfPrepareError) -> Self {
        match value {
            WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component,
                message,
            } => Self::Unavailable {
                machine_id,
                provider: DataplaneProviderFailure::PloyzNativeMesh { component },
                message,
            },
            WireGuardEbpfPrepareError::InvalidReport { message } => Self::InvalidReport { message },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzNativeMeshPrepareRequest {
    pub operation_id: OperationId,
    pub machines: Vec<MachineId>,
    pub endpoint_routes: Vec<WireGuardEbpfEndpointRoute>,
    pub peer_endpoints: Vec<WireGuardPeerEndpoint>,
    pub peers: Vec<WireGuardPeer>,
}

impl PloyzNativeMeshPrepareRequest {
    #[must_use]
    pub fn for_deploy_plan(
        operation_id: OperationId,
        plan: &DeployPlan,
        dataplane_machines: &[MachineId],
        peer_endpoints: &[WireGuardPeerEndpoint],
    ) -> Self {
        let machines = sorted_unique_machines(
            plan.target_machines()
                .into_iter()
                .chain(dataplane_machines.iter().cloned()),
        );
        let requested = machines.iter().collect::<BTreeSet<_>>();
        let peer_endpoints = peer_endpoints
            .iter()
            .filter(|peer| requested.contains(&peer.machine_id))
            .cloned()
            .collect();
        Self::for_machines(operation_id, machines, peer_endpoints, Vec::new())
    }

    #[must_use]
    pub fn from_dataplane_request(
        request: DataplanePrepareRequest,
        peer_endpoints: Vec<WireGuardPeerEndpoint>,
    ) -> Self {
        let machines = request.machines();
        let requested = machines.iter().collect::<BTreeSet<_>>();
        let peer_endpoints = peer_endpoints
            .into_iter()
            .filter(|peer| requested.contains(&peer.machine_id))
            .collect();
        Self {
            operation_id: request.operation_id,
            endpoint_routes: request
                .membership
                .into_iter()
                .map(WireGuardEbpfEndpointRoute::from_member)
                .collect(),
            machines,
            peer_endpoints,
            peers: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_peers(mut self, peers: Vec<WireGuardPeer>) -> Self {
        self.peers = peers;
        self
    }

    #[must_use]
    fn for_machines(
        operation_id: OperationId,
        machines: Vec<MachineId>,
        peer_endpoints: Vec<WireGuardPeerEndpoint>,
        peers: Vec<WireGuardPeer>,
    ) -> Self {
        Self {
            operation_id,
            endpoint_routes: machines
                .iter()
                .map(WireGuardEbpfEndpointRoute::default_for_machine)
                .collect(),
            machines,
            peer_endpoints,
            peers,
        }
    }
}

fn sorted_unique_machines(machines: impl IntoIterator<Item = MachineId>) -> Vec<MachineId> {
    machines
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardEbpfEndpointRoute {
    pub machine_id: MachineId,
    pub endpoint_subnet: String,
}

impl WireGuardEbpfEndpointRoute {
    #[must_use]
    pub fn default_for_machine(machine_id: &MachineId) -> Self {
        Self {
            machine_id: machine_id.clone(),
            endpoint_subnet: default_endpoint_subnet(machine_id),
        }
    }

    #[must_use]
    pub fn from_member(member: DataplaneMember) -> Self {
        Self {
            machine_id: member.machine_id,
            endpoint_subnet: member.endpoint_subnet.as_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardPeerEndpoint {
    pub machine_id: MachineId,
    pub endpoint_subnet: String,
    pub public_endpoint: SocketAddr,
}

impl WireGuardPeerEndpoint {
    #[must_use]
    pub fn new(machine_id: MachineId, public_endpoint: SocketAddr) -> Self {
        Self {
            endpoint_subnet: default_endpoint_subnet(&machine_id),
            machine_id,
            public_endpoint,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardPeer {
    pub machine_id: MachineId,
    pub endpoint_subnet: String,
    pub public_endpoint: SocketAddr,
    pub public_key: WireGuardPublicKey,
}

impl WireGuardPeer {
    #[must_use]
    pub fn from_endpoint(endpoint: WireGuardPeerEndpoint, public_key: WireGuardPublicKey) -> Self {
        Self {
            machine_id: endpoint.machine_id,
            endpoint_subnet: endpoint.endpoint_subnet,
            public_endpoint: endpoint.public_endpoint,
            public_key,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct WireGuardPublicKey(String);

impl WireGuardPublicKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, WireGuardPublicKeyError> {
        let value = value.into();
        if value.is_empty() {
            return Err(WireGuardPublicKeyError::Empty);
        }
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(WireGuardPublicKeyError::Invalid { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WireGuardPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("WireGuardPublicKey")
            .field(&self.0)
            .finish()
    }
}

impl TryFrom<String> for WireGuardPublicKey {
    type Error = WireGuardPublicKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<WireGuardPublicKey> for String {
    fn from(value: WireGuardPublicKey) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardPublicKeyError {
    Empty,
    Invalid { value: String },
}

impl fmt::Display for WireGuardPublicKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("wireguard public key is empty"),
            Self::Invalid { value } => {
                write!(
                    formatter,
                    "wireguard public key {value:?} must not contain whitespace"
                )
            }
        }
    }
}

#[must_use]
pub fn default_endpoint_subnet(machine_id: &MachineId) -> String {
    let octet = trailing_machine_number(machine_id.as_str())
        .map(|number| ((number.saturating_sub(1)) % 254 + 1) as u8)
        .unwrap_or_else(|| stable_machine_octet(machine_id.as_str()));
    format!("10.42.{octet}.0/24")
}

fn trailing_machine_number(value: &str) -> Option<u16> {
    let digits = value
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    digits.chars().rev().collect::<String>().parse().ok()
}

fn stable_machine_octet(value: &str) -> u8 {
    let hash = value.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    (hash % 254 + 1) as u8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PloyzNativeMeshComponent {
    #[serde(rename = "wireguard")]
    WireGuard,
    EbpfForwarding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PloyzNativeMeshPrepareReport {
    pub machines: Vec<PloyzNativeMeshMachineReady>,
}

impl PloyzNativeMeshPrepareReport {
    pub fn for_request(
        request: &PloyzNativeMeshPrepareRequest,
        machines: impl IntoIterator<Item = PloyzNativeMeshMachineReady>,
    ) -> Result<Self, WireGuardEbpfPrepareReportError> {
        let machines = machines.into_iter().collect::<Vec<_>>();
        if request.machines.is_empty() || machines.is_empty() {
            return Err(WireGuardEbpfPrepareReportError::Empty);
        }
        let requested = request.machines.iter().collect::<BTreeSet<_>>();
        if requested.len() != request.machines.len() {
            return Err(WireGuardEbpfPrepareReportError::DuplicateMachine);
        }
        let actual = machines
            .iter()
            .map(|machine| &machine.machine_id)
            .collect::<BTreeSet<_>>();
        if actual.len() != machines.len() {
            return Err(WireGuardEbpfPrepareReportError::DuplicateMachine);
        }
        if requested != actual {
            return Err(WireGuardEbpfPrepareReportError::MachineSetMismatch);
        }

        Ok(Self { machines })
    }

    pub fn from_machines(
        machines: impl IntoIterator<Item = PloyzNativeMeshMachineReady>,
    ) -> Result<Self, WireGuardEbpfPrepareReportError> {
        let machines = machines.into_iter().collect::<Vec<_>>();
        if machines.is_empty() {
            return Err(WireGuardEbpfPrepareReportError::Empty);
        }
        let mut seen = BTreeSet::new();
        if machines
            .iter()
            .any(|machine| !seen.insert(machine.machine_id.clone()))
        {
            return Err(WireGuardEbpfPrepareReportError::DuplicateMachine);
        }

        Ok(Self { machines })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(
    from = "PloyzNativeMeshMachineReadyWire",
    into = "PloyzNativeMeshMachineReadyWire"
)]
pub struct PloyzNativeMeshMachineReady {
    pub machine_id: MachineId,
    #[cfg_attr(feature = "typescript", ts(flatten))]
    pub ready: PloyzNativeMeshReady,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PloyzNativeMeshMachineReadyWire {
    machine_id: MachineId,
    wireguard: WireGuardReady,
    ebpf_forwarding: EbpfForwardingReady,
}

impl From<PloyzNativeMeshMachineReadyWire> for PloyzNativeMeshMachineReady {
    fn from(value: PloyzNativeMeshMachineReadyWire) -> Self {
        let PloyzNativeMeshMachineReadyWire {
            machine_id,
            wireguard,
            ebpf_forwarding,
        } = value;
        Self {
            machine_id,
            ready: PloyzNativeMeshReady {
                wireguard,
                ebpf_forwarding,
            },
        }
    }
}

impl From<PloyzNativeMeshMachineReady> for PloyzNativeMeshMachineReadyWire {
    fn from(value: PloyzNativeMeshMachineReady) -> Self {
        let PloyzNativeMeshMachineReady { machine_id, ready } = value;
        let PloyzNativeMeshReady {
            wireguard,
            ebpf_forwarding,
        } = ready;
        Self {
            machine_id,
            wireguard,
            ebpf_forwarding,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PloyzNativeMeshReady {
    pub wireguard: WireGuardReady,
    pub ebpf_forwarding: EbpfForwardingReady,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardReady {
    pub public_key: WireGuardPublicKey,
    pub evidence: Vec<WireGuardReadyEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct EbpfForwardingReady {
    pub evidence: Vec<EbpfForwardingReadyEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardReadyEvidence {
    HostPath { path: String },
    Command { program: String, args: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EbpfForwardingReadyEvidence {
    HostPath { path: String },
    Command { program: String, args: Vec<String> },
    PloyzTcBytecode { path: String, symbols: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardEbpfPrepareReportError {
    Empty,
    DuplicateMachine,
    MachineSetMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardEbpfPrepareError {
    Unavailable {
        machine_id: MachineId,
        component: PloyzNativeMeshComponent,
        message: FailureMessage,
    },
    InvalidReport {
        message: FailureMessage,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_endpoint_subnet_uses_trailing_machine_number() {
        assert_eq!(
            default_endpoint_subnet(&machine_id("edge_2")),
            "10.42.2.0/24"
        );
        assert_eq!(
            default_endpoint_subnet(&machine_id("machine_255")),
            "10.42.1.0/24"
        );
    }

    #[test]
    fn deploy_prepare_request_carries_endpoint_routes_for_target_machines() {
        let plan = DeployPlan {
            service_id: crate::ids::ServiceId::try_new("svc_api").expect("valid service id"),
            target_revision: crate::ids::RevisionId::try_new("rev_1").expect("valid revision id"),
            steps: vec![
                crate::deploy::DeployPlanStep::RunContainer {
                    machine_id: machine_id("edge_2"),
                    slot: crate::deploy::ReplicaSlot::try_new(1).expect("valid slot"),
                },
                crate::deploy::DeployPlanStep::RunContainer {
                    machine_id: machine_id("core_1"),
                    slot: crate::deploy::ReplicaSlot::try_new(2).expect("valid slot"),
                },
            ],
            cleanup_containers: Vec::new(),
        };

        let request =
            PloyzNativeMeshPrepareRequest::for_deploy_plan(operation_id("op_1"), &plan, &[], &[]);

        assert_eq!(
            request.endpoint_routes,
            vec![
                WireGuardEbpfEndpointRoute {
                    machine_id: machine_id("core_1"),
                    endpoint_subnet: "10.42.1.0/24".to_owned(),
                },
                WireGuardEbpfEndpointRoute {
                    machine_id: machine_id("edge_2"),
                    endpoint_subnet: "10.42.2.0/24".to_owned(),
                }
            ]
        );
    }

    #[test]
    fn dataplane_prepare_request_declares_membership_only() {
        let plan = DeployPlan {
            service_id: crate::ids::ServiceId::try_new("svc_api").expect("valid service id"),
            target_revision: crate::ids::RevisionId::try_new("rev_1").expect("valid revision id"),
            steps: vec![
                crate::deploy::DeployPlanStep::RunContainer {
                    machine_id: machine_id("edge_2"),
                    slot: crate::deploy::ReplicaSlot::try_new(1).expect("valid slot"),
                },
                crate::deploy::DeployPlanStep::RunContainer {
                    machine_id: machine_id("core_1"),
                    slot: crate::deploy::ReplicaSlot::try_new(2).expect("valid slot"),
                },
            ],
            cleanup_containers: Vec::new(),
        };

        let request = DataplanePrepareRequest::for_deploy_plan(operation_id("op_1"), &plan, &[]);

        assert_eq!(
            request.membership,
            vec![
                DataplaneMember {
                    machine_id: machine_id("core_1"),
                    endpoint_subnet: MachineEndpointSubnet::try_new("10.42.1.0/24")
                        .expect("valid subnet"),
                },
                DataplaneMember {
                    machine_id: machine_id("edge_2"),
                    endpoint_subnet: MachineEndpointSubnet::try_new("10.42.2.0/24")
                        .expect("valid subnet"),
                }
            ]
        );
    }

    #[test]
    fn machine_endpoint_subnet_accepts_ipv4_and_ipv6_cidrs() {
        assert_eq!(
            MachineEndpointSubnet::try_new("10.42.1.0/24")
                .expect("valid ipv4")
                .as_string(),
            "10.42.1.0/24"
        );
        assert_eq!(
            MachineEndpointSubnet::try_new("fd7a:115c:a1e0::/64")
                .expect("valid ipv6")
                .as_string(),
            "fd7a:115c:a1e0::/64"
        );
        assert!(MachineEndpointSubnet::try_new("not-a-cidr").is_err());
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }
}

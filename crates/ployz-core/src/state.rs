//! Current-state records stored in JetStream KV.

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

use crate::ids::{MachineId, NamespaceId, OperationId, RevisionId, ServiceId};
use crate::machine::MachineName;
use crate::ops::{RoutePort, RouteTarget};
use crate::wire::id_prefixed_state_key;
use std::net::{IpAddr, SocketAddr};

pub const KV_CORE_BUCKET: &str = "KV_CORE";
pub const KV_OPS_BUCKET: &str = "KV_OPS";
pub const KV_OBS_BUCKET: &str = "KV_OBS";

pub const ACTIVE_SERVICE_STATE_PREFIX: &str = "services";
pub const ACTIVE_MACHINE_STATE_PREFIX: &str = "machines";
/// `KV_CORE` prefix of the durable NATS authorized-principal records
/// (ADR-0001: their recovery evidence is `authorized-users.conf`).
pub const NATS_AUTHORIZED_USER_PREFIX: &str = "nats_authorized_user";
pub const ACTIVE_ROUTE_STATE_PREFIX: &str = "routes";
pub const MACHINE_CONTAINER_OBSERVATION_PREFIX: &str = "containers";
pub const MACHINE_PUBLIC_IP_OBSERVATION_PREFIX: &str = "machines";
pub const GATEWAY_STATUS_OBSERVATION_PREFIX: &str = "gateways";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveServiceState {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub active_revision: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveRouteState {
    pub namespace_id: NamespaceId,
    pub target: RouteTarget,
    pub endpoint_port: RoutePort,
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveMachineState {
    pub machine_id: MachineId,
    pub name: MachineName,
    pub activated_by: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachinePublicIpObservation {
    pub machine_id: MachineId,
    pub public_ip: IpAddr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct GatewayStatusObservation {
    pub machine_id: MachineId,
    pub listen_addr: SocketAddr,
    pub serving: GatewayServingStatus,
    pub route_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum GatewayServingStatus {
    Current,
    LastKnownGood,
    Unavailable,
}

id_prefixed_state_key! { pub struct ActiveServiceStateKey; prefix: ACTIVE_SERVICE_STATE_PREFIX; fn from_service_id(&ServiceId); }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRouteStateKey(String);

impl ActiveRouteStateKey {
    #[must_use]
    pub fn from_target(target: &RouteTarget) -> Self {
        Self(format!(
            "{ACTIVE_ROUTE_STATE_PREFIX}.{}.{}",
            route_hostname_key_token(target),
            target.port.get()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn route_hostname_key_token(target: &RouteTarget) -> String {
    let hostname = target.hostname.as_str();
    let mut token = String::with_capacity(hostname.len() * 2);
    for byte in hostname.bytes() {
        write!(&mut token, "{byte:02x}").expect("writing to string cannot fail");
    }
    token
}

id_prefixed_state_key! { pub struct ActiveMachineStateKey; prefix: ACTIVE_MACHINE_STATE_PREFIX; fn from_machine_id(&MachineId); }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreStateRevision(u64);

impl CoreStateRevision {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveServiceCommitRequest {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub expected_current: ExpectedActiveService,
    pub target_revision: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveRouteCommitRequest {
    pub namespace_id: NamespaceId,
    pub target: RouteTarget,
    pub endpoint_port: RoutePort,
    pub expected_current: ExpectedActiveRoute,
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ExpectedActiveService {
    Absent,
    Revision(RevisionId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ExpectedActiveRoute {
    Absent,
    ServiceRevision(ExpectedActiveRouteRevision),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ExpectedActiveRouteRevision {
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
    pub endpoint_port: RoutePort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveServiceCommit {
    Stored {
        revision: CoreStateRevision,
    },
    AlreadyCommitted {
        current_revision: RevisionId,
    },
    ActiveServiceChanged {
        expected_current: ExpectedActiveService,
        current_revision: Option<RevisionId>,
        attempted_revision: RevisionId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveRouteCommit {
    Stored {
        revision: CoreStateRevision,
    },
    AlreadyCommitted {
        service_id: ServiceId,
        revision_id: RevisionId,
    },
    ActiveRouteChanged {
        expected_current: ExpectedActiveRoute,
        current: Option<ActiveRouteState>,
        attempted: ActiveRouteState,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MachineContainerObservationKey(String);

impl MachineContainerObservationKey {
    #[must_use]
    pub fn from_machine_id(machine_id: &MachineId) -> Self {
        Self(format!(
            "{MACHINE_CONTAINER_OBSERVATION_PREFIX}.{}",
            machine_id.as_str()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MachinePublicIpObservationKey(String);

impl MachinePublicIpObservationKey {
    #[must_use]
    pub fn from_machine_id(machine_id: &MachineId) -> Self {
        Self(format!(
            "{MACHINE_PUBLIC_IP_OBSERVATION_PREFIX}.{}.public_ip",
            machine_id.as_str()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn matches(value: &str) -> bool {
        value.starts_with(&format!("{MACHINE_PUBLIC_IP_OBSERVATION_PREFIX}."))
            && value.ends_with(".public_ip")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GatewayStatusObservationKey(String);

impl GatewayStatusObservationKey {
    #[must_use]
    pub fn from_machine_id(machine_id: &MachineId) -> Self {
        Self(format!(
            "{GATEWAY_STATUS_OBSERVATION_PREFIX}.{}.status",
            machine_id.as_str()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

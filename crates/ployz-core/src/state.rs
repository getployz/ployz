//! Current-state records stored in JetStream KV.

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

use crate::ids::{NodeId, OperationId, RevisionId, ServiceId};
use crate::machine::MachineName;
use crate::ops::{RoutePort, RouteTarget};

pub const ACTIVE_SERVICE_STATE_PREFIX: &str = "services";
pub const ACTIVE_MACHINE_STATE_PREFIX: &str = "machines";
pub const ACTIVE_ROUTE_STATE_PREFIX: &str = "routes";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveServiceState {
    pub service_id: ServiceId,
    pub active_revision: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveRouteState {
    pub target: RouteTarget,
    pub endpoint_port: RoutePort,
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveMachineState {
    pub node_id: NodeId,
    pub name: MachineName,
    pub activated_by: OperationId,
}

impl ActiveMachineState {
    #[must_use]
    pub const fn is_schedulable(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveServiceStateKey(String);

impl ActiveServiceStateKey {
    #[must_use]
    pub fn from_service_id(service_id: &ServiceId) -> Self {
        Self(format!(
            "{ACTIVE_SERVICE_STATE_PREFIX}.{}",
            service_id.as_str()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveMachineStateKey(String);

impl ActiveMachineStateKey {
    #[must_use]
    pub fn from_node_id(node_id: &NodeId) -> Self {
        Self(format!(
            "{ACTIVE_MACHINE_STATE_PREFIX}.{}",
            node_id.as_str()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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
    pub service_id: ServiceId,
    pub expected_current: ExpectedActiveService,
    pub target_revision: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveRouteCommitRequest {
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
pub struct NodeContainerObservationKey(String);

impl NodeContainerObservationKey {
    #[must_use]
    pub fn from_node_id(node_id: &NodeId) -> Self {
        Self(format!("containers.{}", node_id.as_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

//! Machine-scoped DNS RPC wire types.

use std::net::Ipv4Addr;

use ployz_core::ids::MachineId;
use ployz_core::internal_dns::{InternalDnsStatus, InternalServiceName};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DnsResolveRpcRequest {
    pub name: InternalServiceName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DnsResolveRpcOk {
    pub machine_id: MachineId,
    pub name: InternalServiceName,
    pub addresses: Vec<Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DnsStatusRpcRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DnsStatusRpcOk {
    pub machine_id: MachineId,
    pub value: InternalDnsStatus,
}

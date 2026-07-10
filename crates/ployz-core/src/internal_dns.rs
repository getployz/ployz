//! Internal DNS names and their projection from machine facts.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::dataplane::INTERNAL_DNS_SUFFIX;
use crate::ids::{MachineId, NamespaceId, ServiceId};
use crate::machine_runtime::{ContainerRuntimeState, MachineFactsSnapshot};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct InternalDnsStatus {
    pub resolver: InternalDnsResolverStatus,
    pub fact_watermarks: Vec<InternalDnsFactWatermark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum InternalDnsResolverStatus {
    AwaitingBind { attempts: u64 },
    Serving { bound: SocketAddr },
    NotConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct InternalDnsFactWatermark {
    pub machine_id: MachineId,
    pub observed_at_unix_ms: u64,
}

/// A validated, lower-case `<service>.<namespace>.internal` wire name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"InternalServiceName\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct InternalServiceName(String);

impl InternalServiceName {
    /// Builds an internal name from its typed service and namespace ids.
    #[must_use]
    pub fn new(service_id: &ServiceId, namespace_id: &NamespaceId) -> Self {
        Self(
            format!(
                "{}.{}.{}",
                service_id.as_str(),
                namespace_id.as_str(),
                INTERNAL_DNS_SUFFIX
            )
            .to_ascii_lowercase(),
        )
    }

    /// Parses an exact three-label internal service name.
    pub fn try_new(name: impl Into<String>) -> Result<Self, InternalServiceNameError> {
        let name = name.into();
        let mut labels = name.split('.');
        let (Some(service), Some(namespace), Some(suffix), None) =
            (labels.next(), labels.next(), labels.next(), labels.next())
        else {
            return Err(InternalServiceNameError { name });
        };
        if !suffix.eq_ignore_ascii_case(INTERNAL_DNS_SUFFIX) {
            return Err(InternalServiceNameError { name });
        }
        let service_id = ServiceId::try_new(service.to_ascii_lowercase())
            .map_err(|_| InternalServiceNameError { name: name.clone() })?;
        let namespace_id = NamespaceId::try_new(namespace.to_ascii_lowercase())
            .map_err(|_| InternalServiceNameError { name })?;
        Ok(Self::new(&service_id, &namespace_id))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for InternalServiceName {
    type Error = InternalServiceNameError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::try_new(name)
    }
}

impl From<InternalServiceName> for String {
    fn from(name: InternalServiceName) -> Self {
        name.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid internal service name {name:?}")]
pub struct InternalServiceNameError {
    pub name: String,
}

/// Fully-qualified internal service names mapped to their running service
/// containers' endpoint IPv4 addresses.
#[must_use]
pub fn internal_dns_records(
    snapshots: &[MachineFactsSnapshot],
) -> BTreeMap<InternalServiceName, Vec<Ipv4Addr>> {
    let mut records = BTreeMap::<InternalServiceName, Vec<Ipv4Addr>>::new();
    for container in snapshots
        .iter()
        .flat_map(|snapshot| snapshot.containers().containers())
    {
        if !container.is_service() {
            continue;
        }
        let ContainerRuntimeState::Running {
            ip: Some(IpAddr::V4(ip)),
            ..
        } = &container.state
        else {
            continue;
        };
        records
            .entry(InternalServiceName::new(
                &container.identity.service_id,
                &container.identity.namespace_id,
            ))
            .or_default()
            .push(*ip);
    }
    for addresses in records.values_mut() {
        addresses.sort_unstable();
        addresses.dedup();
    }
    records
}

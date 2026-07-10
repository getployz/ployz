use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};

use ployz_core::dataplane::INTERNAL_DNS_SUFFIX;
use ployz_core::ids::{NamespaceId, ServiceId};
use ployz_core::machine_runtime::{ContainerRuntimeState, MachineFactsSnapshot};

/// A validated, lower-case `<service>.<namespace>.internal` wire name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct InternalServiceName(String);

impl InternalServiceName {
    /// Builds an internal name from its typed service and namespace ids.
    #[must_use]
    pub(crate) fn new(service_id: &ServiceId, namespace_id: &NamespaceId) -> Self {
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
    #[must_use]
    pub(crate) fn parse(name: &str) -> Option<Self> {
        let mut labels = name.split('.');
        let service = labels.next()?;
        let namespace = labels.next()?;
        let suffix = labels.next()?;
        if labels.next().is_some() || !suffix.eq_ignore_ascii_case(INTERNAL_DNS_SUFFIX) {
            return None;
        }
        let service_id = ServiceId::try_new(service.to_ascii_lowercase()).ok()?;
        let namespace_id = NamespaceId::try_new(namespace.to_ascii_lowercase()).ok()?;
        Some(Self::new(&service_id, &namespace_id))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Fully-qualified internal service names mapped to their running service
/// containers' endpoint IPv4 addresses.
#[must_use]
pub(crate) fn internal_dns_records(
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

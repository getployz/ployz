//! Ordinary internal service DNS projected from Corrosion rows.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::corrosion::{
    ClusterDocument, ContainerDocument, MachineDocument, MachineTransport, NamespaceDocument,
    ServiceDocument, StoredRow, read_named_roster_rows, read_named_rows, read_rows,
};
use crate::ids::{ClusterId, MachineRowId, NamespaceRowId, ServiceRowId};

const MAX_DNS_LABEL_LEN: usize = 63;
pub const INTERNAL_DNS_SUFFIX: &str = "internal";
/// Resolver-local proof used by founding and join readiness checks.
pub const INTERNAL_DNS_READINESS_NAME: &str = "readiness.ployz.internal";
/// RFC 5737 documentation address reserved for the resolver-local readiness proof.
pub const INTERNAL_DNS_READINESS_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 53);

/// A validated, lower-case `<service>.<namespace>.internal` wire name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"InternalServiceName\">"))]
#[serde(try_from = "String", into = "String")]
pub struct InternalServiceName(String);

impl InternalServiceName {
    /// Builds an internal name from human-facing Corrosion row labels.
    pub fn try_from_labels(
        service: &str,
        namespace: &str,
    ) -> Result<Self, InternalServiceNameError> {
        let name = format!("{service}.{namespace}.{INTERNAL_DNS_SUFFIX}");
        if service.bytes().any(|byte| byte.is_ascii_uppercase())
            || namespace.bytes().any(|byte| byte.is_ascii_uppercase())
        {
            return Err(InternalServiceNameError { name });
        }
        Self::try_from_label_parts(service, namespace, name)
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
        if [service, namespace, suffix]
            .iter()
            .any(|label| label.len() > MAX_DNS_LABEL_LEN)
        {
            return Err(InternalServiceNameError { name });
        }
        Self::try_from_label_parts(
            &service.to_ascii_lowercase(),
            &namespace.to_ascii_lowercase(),
            name,
        )
    }

    fn try_from_label_parts(
        service: &str,
        namespace: &str,
        name: String,
    ) -> Result<Self, InternalServiceNameError> {
        let service = service.to_ascii_lowercase();
        let namespace = namespace.to_ascii_lowercase();
        if !is_dns_label(&service) || !is_dns_label(&namespace) {
            return Err(InternalServiceNameError { name });
        }
        Ok(Self(format!("{service}.{namespace}.{INTERNAL_DNS_SUFFIX}")))
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
        let InternalServiceName(name) = name;
        name
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid internal service name {name:?}")]
pub struct InternalServiceNameError {
    pub name: String,
}

/// A validated, lower-case `<namespace>.internal` container search domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalDnsSearchDomain(String);

impl InternalDnsSearchDomain {
    /// Builds a search domain from the authoritative human-facing namespace label.
    pub fn try_from_namespace_label(namespace: &str) -> Result<Self, InternalDnsSearchDomainError> {
        let domain = format!("{namespace}.{INTERNAL_DNS_SUFFIX}");
        if namespace.bytes().any(|byte| byte.is_ascii_uppercase()) || !is_dns_label(namespace) {
            return Err(InternalDnsSearchDomainError { domain });
        }
        Ok(Self(domain))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid internal DNS search domain {domain:?}")]
pub struct InternalDnsSearchDomainError {
    pub domain: String,
}

fn is_dns_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= MAX_DNS_LABEL_LEN
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// All durable Corrosion inputs required to rebuild one machine's internal
/// DNS view from scratch.
#[derive(Debug)]
pub struct InternalDnsRowProjectionInput {
    pub cluster_id: ClusterId,
    pub local_machine_id: MachineRowId,
    pub cluster_rows: Vec<StoredRow>,
    pub machine_rows: Vec<StoredRow>,
    pub namespace_rows: Vec<StoredRow>,
    pub service_rows: Vec<StoredRow>,
    pub container_rows: Vec<StoredRow>,
}

/// One complete internal DNS view derived from current accepted Corrosion
/// rows. Replacing this value atomically prevents partial cross-table views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalDnsRowProjection {
    pub bind: SocketAddr,
    pub records: BTreeMap<InternalServiceName, Vec<Ipv4Addr>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InternalDnsRowProjectionError {
    #[error("cluster {cluster_id} is not present in the accepted cluster rows")]
    MissingCluster { cluster_id: ClusterId },
    #[error("cluster {cluster_id} does not have one valid canonical cluster row")]
    InvalidCluster { cluster_id: ClusterId },
    #[error("local machine {machine_id} is not present in the accepted machine roster")]
    LocalMachineMissing { machine_id: MachineRowId },
}

/// Applies the shared tolerant-reader, provider-roster, and lowest-ULID name
/// laws before joining current service intent to retained container rows.
#[must_use = "the internal DNS projection or startup error must be applied"]
pub fn project_internal_dns_rows(
    input: InternalDnsRowProjectionInput,
) -> Result<InternalDnsRowProjection, InternalDnsRowProjectionError> {
    let InternalDnsRowProjectionInput {
        cluster_id,
        local_machine_id,
        cluster_rows,
        machine_rows,
        namespace_rows,
        service_rows,
        container_rows,
    } = input;

    let cluster_report = read_rows::<ClusterDocument>(&cluster_id, cluster_rows);
    let cluster = match cluster_report.accepted.as_slice() {
        [accepted]
            if accepted.source.key == cluster_id.as_str()
                && accepted.value.cluster_id == cluster_id =>
        {
            accepted.value.clone()
        }
        [] if cluster_report.skipped.is_empty() => {
            return Err(InternalDnsRowProjectionError::MissingCluster { cluster_id });
        }
        _ => return Err(InternalDnsRowProjectionError::InvalidCluster { cluster_id }),
    };

    let machine_report = read_named_roster_rows::<MachineDocument>(&cluster, machine_rows);
    let mut accepted_machine_ids = BTreeSet::new();
    let mut endpoint_subnet = None;
    for row in machine_report.accepted {
        let Ok(machine_id) = MachineRowId::try_new(row.id.as_str().to_owned()) else {
            continue;
        };
        if machine_id == local_machine_id {
            endpoint_subnet = Some(match row.value.transport {
                MachineTransport::Wireguard { subnet_v4, .. }
                | MachineTransport::Tailscale { subnet_v4, .. } => subnet_v4,
            });
        }
        accepted_machine_ids.insert(machine_id);
    }
    let Some(endpoint_subnet) = endpoint_subnet else {
        return Err(InternalDnsRowProjectionError::LocalMachineMissing {
            machine_id: local_machine_id,
        });
    };

    let namespace_report = read_named_rows::<NamespaceDocument>(&cluster_id, namespace_rows);
    let namespaces = namespace_report
        .accepted
        .into_iter()
        .filter_map(|row| {
            NamespaceRowId::try_new(row.id.as_str().to_owned())
                .ok()
                .map(|id| (id, row.value.name))
        })
        .collect::<BTreeMap<_, _>>();

    let service_report = read_named_rows::<ServiceDocument>(&cluster_id, service_rows);
    let services = service_report
        .accepted
        .into_iter()
        .filter_map(|row| {
            ServiceRowId::try_new(row.id.as_str().to_owned())
                .ok()
                .map(|id| (id, row.value))
        })
        .collect::<BTreeMap<_, _>>();

    let mut records = BTreeMap::<InternalServiceName, Vec<Ipv4Addr>>::new();
    for service in services.values() {
        let Some(namespace_name) = namespaces.get(&service.namespace_id) else {
            continue;
        };
        let Ok(name) = InternalServiceName::try_from_labels(&service.name, namespace_name) else {
            continue;
        };
        if is_reserved_readiness_record(&name) {
            continue;
        }
        records.entry(name).or_default();
    }
    for row in read_rows::<ContainerDocument>(&cluster_id, container_rows).accepted {
        let container = row.value;
        if !accepted_machine_ids.contains(&container.machine_id) {
            continue;
        }
        let Some(service) = services.get(&container.service_id) else {
            continue;
        };
        if container.namespace_id != service.namespace_id
            || container.deploy != service.active_deploy
        {
            continue;
        }
        let Some(namespace_name) = namespaces.get(&service.namespace_id) else {
            continue;
        };
        let Ok(name) = InternalServiceName::try_from_labels(&service.name, namespace_name) else {
            continue;
        };
        if is_reserved_readiness_record(&name) {
            continue;
        }
        records.entry(name).or_default().push(container.ip);
    }
    for addresses in records.values_mut() {
        addresses.sort_unstable();
        addresses.dedup();
    }

    let bind = SocketAddr::from((endpoint_subnet.bridge_gateway_ipv4(), 53));
    Ok(InternalDnsRowProjection { bind, records })
}

fn is_reserved_readiness_record(name: &InternalServiceName) -> bool {
    name.as_str() == INTERNAL_DNS_READINESS_NAME
}

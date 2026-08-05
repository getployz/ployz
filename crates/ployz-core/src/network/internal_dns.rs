//! Ordinary internal service DNS projected from intent plus machine facts.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::corrosion::{
    ClusterDocument, ContainerDocument, MachineDocument, MachineTransport, NamespaceDocument,
    ServiceDocument, StoredRow, read_named_roster_rows, read_named_rows, read_rows,
};
use crate::ids::{
    ClusterId, MachineId, MachineRowId, NamespaceId, NamespaceRowId, ServiceId, ServiceRowId,
    SubjectToken, SubjectTokenError,
};
use crate::intent::IntentSnapshot;
use crate::machine::runtime::{ContainerRuntimeState, MachineFactsSnapshot};
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

const MAX_DNS_LABEL_LEN: usize = 63;
pub const INTERNAL_DNS_SUFFIX: &str = "internal";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct InternalDnsStatus {
    pub resolver: InternalDnsResolverStatus,
    pub fact_watermarks: Vec<InternalDnsFactWatermark>,
    #[serde(default)]
    pub intent_health: InternalDnsIntentHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct InternalDnsIntentHealth {
    pub refresh: InternalDnsIntentRefreshHealth,
    pub watch: InternalDnsIntentWatchHealth,
}

impl InternalDnsIntentHealth {
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            refresh: InternalDnsIntentRefreshHealth::Unknown,
            watch: InternalDnsIntentWatchHealth::Unknown,
        }
    }

    #[must_use]
    pub const fn pending() -> Self {
        Self {
            refresh: InternalDnsIntentRefreshHealth::Pending,
            watch: InternalDnsIntentWatchHealth::Pending,
        }
    }
}

impl Default for InternalDnsIntentHealth {
    fn default() -> Self {
        Self::unknown()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum InternalDnsIntentRefreshHealth {
    Unknown,
    Pending,
    Current,
    RequestFailed { message: String },
    TimedOut { timeout_seconds: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum InternalDnsIntentWatchHealth {
    Unknown,
    Pending,
    Watching,
    OpenFailed { message: String },
    SubscriptionClosed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum InternalDnsResolverStatus {
    AwaitingBind { attempts: u64 },
    Serving { bound: SocketAddr },
    NotConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct InternalDnsFactWatermark {
    pub machine_id: MachineId,
    pub observed_at_unix_ms: u64,
    pub resolver_cache_incarnation: InternalDnsResolverCacheIncarnation,
    pub generation: InternalDnsFactGeneration,
}

/// An opaque identity minted for one resolver fact-cache lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(type = "Brand<string, \"InternalDnsResolverCacheIncarnation\">")
)]
#[serde(transparent)]
pub struct InternalDnsResolverCacheIncarnation(SubjectToken);

impl InternalDnsResolverCacheIncarnation {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        Ok(Self(SubjectToken::try_new(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

positive_u64_wire_newtype! {
    /// A resolver-local count of full snapshots recorded for one machine.
    pub struct InternalDnsFactGeneration;
    ts_brand: "Brand<string, \"InternalDnsFactGeneration\">";
    accessor: get;
    error: InternalDnsFactGenerationError;
}

positive_u64_wire_error! {
    pub enum InternalDnsFactGenerationError;
    noun: "internal DNS fact generation";
}

impl InternalDnsFactGeneration {
    #[must_use]
    pub fn next(self) -> Self {
        Self::try_new(self.get().saturating_add(1)).unwrap_or(self)
    }
}

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

    /// Builds an internal name from its typed service and namespace ids.
    pub fn try_from_ids(
        service_id: &ServiceId,
        namespace_id: &NamespaceId,
    ) -> Result<Self, InternalServiceNameError> {
        let name = format!(
            "{}.{}.{}",
            service_id.as_str(),
            namespace_id.as_str(),
            INTERNAL_DNS_SUFFIX
        );
        if service_id
            .as_str()
            .bytes()
            .any(|byte| byte.is_ascii_uppercase())
            || namespace_id
                .as_str()
                .bytes()
                .any(|byte| byte.is_ascii_uppercase())
        {
            return Err(InternalServiceNameError { name });
        }
        Self::try_from_label_parts(service_id.as_str(), namespace_id.as_str(), name)
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
        Self::try_from_label_parts(service, namespace, name.clone())
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
        records.entry(name).or_default().push(container.ip);
    }
    for addresses in records.values_mut() {
        addresses.sort_unstable();
        addresses.dedup();
    }

    let bind = SocketAddr::from((endpoint_subnet.bridge_gateway_ipv4(), 53));
    Ok(InternalDnsRowProjection { bind, records })
}

/// Fully-qualified internal service names mapped to their running service
/// containers' endpoint IPv4 addresses, constrained by current operator intent.
#[must_use]
pub fn internal_dns_records(
    intent: &IntentSnapshot,
    snapshots: &[MachineFactsSnapshot],
) -> BTreeMap<InternalServiceName, Vec<Ipv4Addr>> {
    let active_machine_ids = intent
        .active_machines
        .iter()
        .map(|machine| &machine.machine_id)
        .collect::<BTreeSet<_>>();
    let serving_entries = intent
        .serving_target_entries
        .iter()
        .map(|entry| {
            (
                &entry.namespace_id,
                &entry.service_id,
                &entry.namespace_revision_entry_id,
            )
        })
        .collect::<BTreeSet<_>>();
    let mut records = BTreeMap::<InternalServiceName, Vec<Ipv4Addr>>::new();
    for container in snapshots
        .iter()
        .filter(|snapshot| active_machine_ids.contains(snapshot.machine_id()))
        .flat_map(|snapshot| snapshot.containers().containers())
    {
        if !container.is_service()
            || !serving_entries.contains(&(
                &container.identity.namespace_id,
                &container.identity.service_id,
                &container.identity.namespace_revision_entry_id,
            ))
        {
            continue;
        }
        let ContainerRuntimeState::Running {
            ip: Some(IpAddr::V4(ip)),
            ..
        } = &container.state
        else {
            continue;
        };
        let Ok(name) = InternalServiceName::try_from_ids(
            &container.identity.service_id,
            &container.identity.namespace_id,
        ) else {
            continue;
        };
        records.entry(name).or_default().push(*ip);
    }
    for addresses in records.values_mut() {
        addresses.sort_unstable();
        addresses.dedup();
    }
    records
}

#[cfg(test)]
mod tests {
    use super::{
        InternalDnsIntentHealth, InternalDnsIntentRefreshHealth, InternalDnsIntentWatchHealth,
        InternalDnsStatus,
    };

    #[test]
    fn legacy_status_without_intent_health_decodes_as_unknown() {
        let status = serde_json::from_value::<InternalDnsStatus>(serde_json::json!({
            "resolver": { "status": "not_configured" },
            "fact_watermarks": []
        }))
        .expect("legacy internal DNS status");

        assert_eq!(status.intent_health, InternalDnsIntentHealth::unknown());
    }

    #[test]
    fn intent_loop_failures_have_distinct_wire_shapes() {
        let health = InternalDnsIntentHealth {
            refresh: InternalDnsIntentRefreshHealth::TimedOut { timeout_seconds: 5 },
            watch: InternalDnsIntentWatchHealth::OpenFailed {
                message: "subscription unavailable".to_owned(),
            },
        };

        assert_eq!(
            serde_json::to_value(health).expect("intent health"),
            serde_json::json!({
                "refresh": { "status": "timed_out", "timeout_seconds": 5 },
                "watch": {
                    "status": "open_failed",
                    "message": "subscription unavailable"
                }
            })
        );
    }
}

//! Ordinary internal service DNS projected from intent plus machine facts.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, NamespaceId, ServiceId, SubjectToken, SubjectTokenError};
use crate::intent::IntentSnapshot;
use crate::machine::runtime::{ContainerRuntimeState, MachineFactsSnapshot};
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

const MAX_DNS_LABEL_LEN: usize = 63;
pub const INTERNAL_DNS_SUFFIX: &str = "internal";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct InternalDnsStatus {
    pub resolver: InternalDnsResolverStatus,
    pub fact_watermarks: Vec<InternalDnsFactWatermark>,
    #[serde(default)]
    pub intent_health: InternalDnsIntentHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum InternalDnsIntentRefreshHealth {
    Unknown,
    Pending,
    Current,
    RequestFailed { message: String },
    TimedOut { timeout_seconds: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum InternalDnsIntentWatchHealth {
    Unknown,
    Pending,
    Watching,
    OpenFailed { message: String },
    SubscriptionClosed,
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
    pub resolver_cache_incarnation: InternalDnsResolverCacheIncarnation,
    pub generation: InternalDnsFactGeneration,
}

/// An opaque identity minted for one resolver fact-cache lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"InternalServiceName\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct InternalServiceName(String);

impl InternalServiceName {
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
        Self::try_new(name)
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
        let service_id = ServiceId::try_new(service.to_ascii_lowercase())
            .map_err(|_| InternalServiceNameError { name: name.clone() })?;
        let namespace_id = NamespaceId::try_new(namespace.to_ascii_lowercase())
            .map_err(|_| InternalServiceNameError { name })?;
        Ok(Self(
            format!(
                "{}.{}.{}",
                service_id.as_str(),
                namespace_id.as_str(),
                INTERNAL_DNS_SUFFIX
            )
            .to_ascii_lowercase(),
        ))
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

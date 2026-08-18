//! Complete Runtime Watch observations carried on the Machine RPC stream.

use serde::{Deserialize, Serialize};

use crate::{
    ContainerId, ContainerObservation, DockerVolume, DockerVolumeId, IngressHost, MachineId,
    MachineObservation, ServiceObservation,
};

crate::value::open_string_enum!(CertificateAvailability, Unrecognized {
    Available => "available",
    Pending => "pending",
    Failure => "failure",
    Unknown => "unknown",
});

crate::value::open_string_enum!(CertificateFailureKind, Unrecognized {
    DoesNotResolve => "does_not_resolve",
    ResolvesElsewhere => "resolves_elsewhere",
    Authority => "authority",
});

/// Shared backoff clock after a refusal or an authority failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CertificateBackoff {
    pub failure_kind: CertificateFailureKind,
    pub next_attempt_at: String,
    pub failures: u32,
}

/// Redacted certificate status keyed by Ingress Hostname.
///
/// Never carries Certificate Material or HTTP-01 challenge bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CertificateObservation {
    pub hostname: IngressHost,
    pub status: CertificateAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<CertificateBackoff>,
}

/// Typed incomplete replicated IDs. An incomplete row is not a delete.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeWatchIncompleteIds {
    #[serde(default)]
    pub machines: Vec<MachineId>,
    #[serde(default)]
    pub containers: Vec<ContainerId>,
    #[serde(default)]
    pub volumes: Vec<DockerVolumeId>,
    #[serde(default)]
    pub certificates: Vec<IngressHost>,
}

/// One complete Runtime Watch observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeWatchFrame {
    #[serde(default)]
    pub machines: Vec<MachineObservation>,
    #[serde(default)]
    pub containers: Vec<ContainerObservation>,
    #[serde(default)]
    pub services: Vec<ServiceObservation>,
    #[serde(default)]
    pub volumes: Vec<DockerVolume>,
    #[serde(default)]
    pub certificates: Vec<CertificateObservation>,
    /// Hosted DNS hostname only; never the renewal token or endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_dns_hostname: Option<String>,
    #[serde(default)]
    pub incomplete_ids: RuntimeWatchIncompleteIds,
    /// Freshness of the entry-local membership/RTT sample. Not Cluster truth.
    pub observed_at: String,
}

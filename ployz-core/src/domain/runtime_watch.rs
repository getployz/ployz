//! Complete Runtime Watch observations carried on the Machine RPC stream.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    CodecError, ContainerId, ContainerObservation, DockerVolume, DockerVolumeId, IngressHost,
    MachineId, MachineObservation, OpaquePayload, RUNTIME_WATCH_MESSAGE_SIZE_LIMIT,
    ServiceObservation, derive_services,
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

#[derive(Serialize)]
struct RuntimeWatchPayload<'frame> {
    machines: &'frame [MachineObservation],
    containers: &'frame [ContainerObservation],
    volumes: &'frame [DockerVolume],
    certificates: &'frame [CertificateObservation],
    #[serde(skip_serializing_if = "Option::is_none")]
    hosted_dns_hostname: Option<&'frame str>,
    incomplete_ids: &'frame RuntimeWatchIncompleteIds,
    observed_at: &'frame str,
}

/// Runtime Watch JSON could not be encoded, decoded, or admitted under its size ceiling.
#[derive(Debug, Error)]
pub enum RuntimeWatchPayloadError {
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error(
        "Runtime Watch message length too large: found {found} bytes, the limit is {limit} bytes"
    )]
    MessageTooLarge { found: usize, limit: usize },
}

/// Encode a frame without the Service view clients derive from its Containers.
///
/// # Errors
///
/// Returns when the frame cannot be serialized or exceeds the Runtime Watch ceiling.
pub fn encode_runtime_watch_frame(
    frame: &RuntimeWatchFrame,
) -> Result<OpaquePayload, RuntimeWatchPayloadError> {
    let RuntimeWatchFrame {
        machines,
        containers,
        services: _,
        volumes,
        certificates,
        hosted_dns_hostname,
        incomplete_ids,
        observed_at,
    } = frame;
    let payload = OpaquePayload::from_json(&RuntimeWatchPayload {
        machines,
        containers,
        volumes,
        certificates,
        hosted_dns_hostname: hosted_dns_hostname.as_deref(),
        incomplete_ids,
        observed_at,
    })?;
    validate_runtime_watch_payload_size(&payload)?;
    Ok(payload)
}

/// Decode a frame and derive its Service view from the transmitted Containers.
///
/// # Errors
///
/// Returns when the payload is not a Runtime Watch frame or exceeds its ceiling.
pub fn decode_runtime_watch_frame(
    payload: &OpaquePayload,
) -> Result<RuntimeWatchFrame, RuntimeWatchPayloadError> {
    validate_runtime_watch_payload_size(payload)?;
    let mut frame = payload.decode_json::<RuntimeWatchFrame>()?;
    frame.services = derive_services(frame.containers.iter().cloned());
    Ok(frame)
}

fn validate_runtime_watch_payload_size(
    payload: &OpaquePayload,
) -> Result<(), RuntimeWatchPayloadError> {
    if payload.json.len() > RUNTIME_WATCH_MESSAGE_SIZE_LIMIT {
        return Err(RuntimeWatchPayloadError::MessageTooLarge {
            found: payload.json.len(),
            limit: RUNTIME_WATCH_MESSAGE_SIZE_LIMIT,
        });
    }
    Ok(())
}

//! Complete Runtime Watch observations carried on the Machine RPC stream.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

use crate::{
    CertificateHost, CodecError, ContainerId, ContainerObservation, DockerVolume, DockerVolumeId,
    IssuanceFailure, MachineId, MachineObservation, OpaquePayload,
    RUNTIME_WATCH_MESSAGE_SIZE_LIMIT, Refusal, ServiceObservation, derive_services,
};

crate::value::open_string_enum!(CertificateAvailability, Unrecognized {
    Available => "available",
    Pending => "pending",
    Failure => "failure",
    Unknown => "unknown",
});

crate::value::open_string_enum!(CertificateFailureKind, Unrecognized {
    DoesNotResolve => "does_not_resolve",
    Unreachable => "unreachable",
    RedirectsToHttps => "redirects_to_https",
    ReachesElsewhere => "reaches_elsewhere",
    Authority => "authority",
});

impl From<IssuanceFailure> for CertificateFailureKind {
    fn from(failure: IssuanceFailure) -> Self {
        match failure {
            IssuanceFailure::Refused(Refusal::DoesNotResolve) => Self::DoesNotResolve,
            IssuanceFailure::Refused(Refusal::Unreachable) => Self::Unreachable,
            IssuanceFailure::Refused(Refusal::RedirectsToHttps) => Self::RedirectsToHttps,
            IssuanceFailure::Refused(Refusal::ReachesElsewhere) => Self::ReachesElsewhere,
            IssuanceFailure::Authority => Self::Authority,
        }
    }
}

impl CertificateFailureKind {
    /// The failure this kind names. `None` for a spelling this reader does not know.
    #[must_use]
    pub fn issuance_failure(&self) -> Option<IssuanceFailure> {
        Some(match self {
            Self::DoesNotResolve => IssuanceFailure::Refused(Refusal::DoesNotResolve),
            Self::Unreachable => IssuanceFailure::Refused(Refusal::Unreachable),
            Self::RedirectsToHttps => IssuanceFailure::Refused(Refusal::RedirectsToHttps),
            Self::ReachesElsewhere => IssuanceFailure::Refused(Refusal::ReachesElsewhere),
            Self::Authority => IssuanceFailure::Authority,
            Self::Unrecognized(_) => return None,
        })
    }
}

/// Why a Machine orders no certificate for a hostname though nothing failed. A hold has no
/// field of its own: it travels as the sentence in the row's `last_error`, which older readers
/// and the dashboard show as written and which every reader recognizes by its exact spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertificateHold {
    /// Every Server uses the Tunnel Door, so Cloudflare serves the certificate.
    TunnelServes,
}

impl CertificateHold {
    const TUNNEL_SERVES: &'static str =
        "Cloudflare serves the certificate while every Server uses the Tunnel Door";

    /// The sentence recorded in `last_error` for this hold.
    #[must_use]
    pub const fn sentence(self) -> &'static str {
        match self {
            Self::TunnelServes => Self::TUNNEL_SERVES,
        }
    }

    /// The hold `last_error` records, if it is exactly one hold's sentence.
    #[must_use]
    pub fn of_sentence(last_error: &str) -> Option<Self> {
        (last_error == Self::TUNNEL_SERVES).then_some(Self::TunnelServes)
    }
}

/// Shared backoff clock after a refusal or an authority failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct CertificateBackoff {
    pub failure_kind: CertificateFailureKind,
    pub next_attempt_at: String,
    pub failures: u32,
}

/// Redacted certificate status keyed by certificate hostname.
///
/// Never carries Certificate Material or HTTP-01 challenge bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct CertificateObservation {
    pub hostname: CertificateHost,
    pub status: CertificateAvailability,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub backoff: Option<CertificateBackoff>,
    /// The issued certificate was ordered through a proxy in front of this Cluster.
    #[serde(default)]
    pub via_proxy: bool,
}

impl CertificateObservation {
    /// The hold that withholds this certificate. A certificate a Machine still serves, or one
    /// still being ordered, is not withheld, whatever sentence `last_error` lingers with.
    #[must_use]
    pub fn hold(&self) -> Option<CertificateHold> {
        if self.status != CertificateAvailability::Failure {
            return None;
        }
        self.last_error
            .as_deref()
            .and_then(CertificateHold::of_sentence)
    }
}

/// Typed incomplete replicated IDs. An incomplete row is not a delete.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RuntimeWatchIncompleteIds {
    #[serde(default)]
    pub machines: Vec<MachineId>,
    #[serde(default)]
    pub containers: Vec<ContainerId>,
    #[serde(default)]
    pub volumes: Vec<DockerVolumeId>,
    #[serde(default)]
    pub certificates: Vec<CertificateHost>,
}

/// One complete Runtime Watch observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RuntimeWatchFrame {
    #[serde(default)]
    pub machines: Vec<MachineObservation>,
    #[serde(default)]
    pub containers: Vec<ContainerObservation>,
    #[serde(default)]
    pub volumes: Vec<DockerVolume>,
    #[serde(default)]
    pub certificates: Vec<CertificateObservation>,
    #[serde(default)]
    pub incomplete_ids: RuntimeWatchIncompleteIds,
    /// Machines whose daemon didn't answer the entry's last probes, though their
    /// membership may read Up: their rows are only as they last reported them. The
    /// entry's view, not Cluster truth; empty from daemons that don't probe.
    #[serde(default)]
    pub unanswered: Vec<MachineId>,
    /// Machines whose daemon answered the entry: the entry itself, and each peer that
    /// answered a probe since it was last named `unanswered`. A peer in neither list is
    /// not known either way yet, as every peer is in a watch's first frame, sent before
    /// any probe. Absent from daemons that don't say, where only `unanswered` is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answered: Option<Vec<MachineId>>,
    /// Freshness of the entry-local membership/RTT sample. Not Cluster truth.
    pub observed_at: String,
}

impl RuntimeWatchFrame {
    /// Derive Services from this frame's current Container observations.
    #[must_use]
    pub fn services(&self) -> Vec<ServiceObservation> {
        derive_services(self.containers.iter().cloned())
    }
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
    let payload = OpaquePayload::from_json(frame)?;
    validate_runtime_watch_payload_size(&payload)?;
    Ok(payload)
}

/// Decode a frame whose Service view is derived on access from its Containers.
///
/// # Errors
///
/// Returns when the payload is not a Runtime Watch frame or exceeds its ceiling.
pub fn decode_runtime_watch_frame(
    payload: &OpaquePayload,
) -> Result<RuntimeWatchFrame, RuntimeWatchPayloadError> {
    validate_runtime_watch_payload_size(payload)?;
    Ok(payload.decode_json::<RuntimeWatchFrame>()?)
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

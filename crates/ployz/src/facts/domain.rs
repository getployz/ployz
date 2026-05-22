//! Product-owned domain readiness facts and reducer.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::acme::{
    CertificateActivation, CertificateMaterialState, CertificateUnusableReason,
    CertificateUsability, Hostname, RevocationFreshness,
};
use crate::domain::{
    DomainFailure, DomainName, DomainPendingReason, DomainReadyRecord, DomainStatus,
};
use crate::error::{CertificateFailure, ServingFailure};
use crate::facts::{
    ProductFact, ProductFactEnvelope, ProductFactKey, ProductFactKind, ProductFactPayload,
    ProductFactResource, ProductFactTarget,
};
use crate::serving::ServingGeneration;

pub const DOMAIN_PENDING_KIND: &str = "ployz.domain.pending.v1";
pub const DOMAIN_READY_KIND: &str = "ployz.domain.ready.v1";
pub const DOMAIN_FAILED_KIND: &str = "ployz.domain.failed.v1";

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DomainFactError {
    #[error("domain fact payload is invalid")]
    InvalidPayload,
    #[error("domain fact target does not match payload")]
    TargetMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainStatusEvent {
    Pending {
        domain: DomainName,
        reason: DomainPendingReason,
    },
    Ready(DomainReadyRecord),
    Failed {
        domain: DomainName,
        failure: DomainFailure,
    },
}

impl DomainStatusEvent {
    #[must_use]
    pub fn pending(domain: DomainName, reason: DomainPendingReason) -> Self {
        Self::Pending { domain, reason }
    }

    #[must_use]
    pub fn ready(record: DomainReadyRecord) -> Self {
        Self::Ready(record)
    }

    #[must_use]
    pub fn failed(domain: DomainName, failure: DomainFailure) -> Self {
        Self::Failed { domain, failure }
    }

    pub fn decode_payload(payload: &[u8]) -> Result<Self, DomainFactError> {
        let payload = serde_json::from_slice::<DomainFactPayload>(payload)
            .map_err(|_| DomainFactError::InvalidPayload)?;
        payload.try_into()
    }

    #[must_use]
    pub fn domain(&self) -> &DomainName {
        match self {
            Self::Pending { domain, .. } | Self::Failed { domain, .. } => domain,
            Self::Ready(record) => record.domain(),
        }
    }

    #[must_use]
    pub fn status(&self) -> DomainStatus {
        match self {
            Self::Pending { reason, .. } => DomainStatus::Pending(reason.clone()),
            Self::Ready(record) => DomainStatus::Ready(record.clone()),
            Self::Failed { failure, .. } => DomainStatus::Failed(failure.clone()),
        }
    }

    fn payload(&self) -> Result<DomainFactPayload, DomainFactError> {
        match self {
            Self::Pending { domain, reason } => Ok(DomainFactPayload::Pending {
                domain: domain.as_str().to_owned(),
                reason: pending_reason_code(reason).to_owned(),
            }),
            Self::Ready(record) => Ok(DomainFactPayload::Ready {
                domain: record.domain().as_str().to_owned(),
                certificate: CertificatePayload::from_certificate(record.certificate())?,
                serving_generation: record.serving_generation().value(),
            }),
            Self::Failed { domain, failure } => Ok(DomainFactPayload::Failed {
                domain: domain.as_str().to_owned(),
                failure: FailurePayload::from_failure(failure),
            }),
        }
    }

    fn target(&self) -> Result<ProductFactTarget, DomainFactError> {
        let domain = self.domain().as_str();
        let payload = self.payload()?;
        let suffix = payload_suffix(&payload)?;
        let (key, kind) = match &payload {
            DomainFactPayload::Pending { reason, .. } => (
                format!("/facts/domain/{domain}/pending/{reason}/{suffix}"),
                DOMAIN_PENDING_KIND,
            ),
            DomainFactPayload::Ready {
                serving_generation, ..
            } => (
                format!("/facts/domain/{domain}/ready/{serving_generation}/{suffix}"),
                DOMAIN_READY_KIND,
            ),
            DomainFactPayload::Failed { failure, .. } => (
                format!(
                    "/facts/domain/{domain}/failed/{}/{}",
                    failure.key_fragment(),
                    suffix
                ),
                DOMAIN_FAILED_KIND,
            ),
        };
        Ok(ProductFactTarget::new(
            ProductFactResource::parse(format!("domain-status:{domain}"))
                .map_err(|_| DomainFactError::InvalidPayload)?,
            ProductFactKey::parse(key).map_err(|_| DomainFactError::InvalidPayload)?,
            ProductFactKind::parse(kind).map_err(|_| DomainFactError::InvalidPayload)?,
        ))
    }
}

impl ProductFact for DomainStatusEvent {
    type Error = DomainFactError;

    fn encode(&self) -> Result<ProductFactEnvelope, Self::Error> {
        let payload =
            serde_json::to_vec(&self.payload()?).map_err(|_| DomainFactError::InvalidPayload)?;
        Ok(ProductFactEnvelope::new(
            self.target()?,
            ProductFactPayload::new(payload).map_err(|_| DomainFactError::InvalidPayload)?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainStatusReducer {
    domain: DomainName,
}

impl DomainStatusReducer {
    #[must_use]
    pub fn new(domain: DomainName) -> Self {
        Self { domain }
    }

    pub fn reduce<I>(&self, events: I) -> Result<DomainStatus, DomainFactError>
    where
        I: IntoIterator<Item = DomainStatusEvent>,
    {
        let mut status = DomainStatus::Unknown;
        for event in events {
            if event.domain() != &self.domain {
                return Err(DomainFactError::TargetMismatch);
            }
            status = event.status();
        }
        Ok(status)
    }
}

fn payload_suffix(payload: &DomainFactPayload) -> Result<String, DomainFactError> {
    let bytes = serde_json::to_vec(payload).map_err(|_| DomainFactError::InvalidPayload)?;
    let digest = Sha256::digest(bytes);
    Ok(hex_prefix(&digest, 12))
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    let mut suffix = String::with_capacity(len * 2);
    for byte in bytes.iter().take(len) {
        suffix.push_str(&format!("{byte:02x}"));
    }
    suffix
}

fn pending_reason_code(reason: &DomainPendingReason) -> &'static str {
    match reason {
        DomainPendingReason::CertificateIssuance => "certificate_issuance",
        DomainPendingReason::CertificateIssuanceInterrupted => "certificate_issuance_interrupted",
        DomainPendingReason::ServingActivation => "serving_activation",
    }
}

fn pending_reason_from_code(value: &str) -> Result<DomainPendingReason, DomainFactError> {
    match value {
        "certificate_issuance" => Ok(DomainPendingReason::CertificateIssuance),
        "certificate_issuance_interrupted" => {
            Ok(DomainPendingReason::CertificateIssuanceInterrupted)
        }
        "serving_activation" => Ok(DomainPendingReason::ServingActivation),
        _ => Err(DomainFactError::InvalidPayload),
    }
}

fn unusable_reason_code(reason: &CertificateUnusableReason) -> &'static str {
    match reason {
        CertificateUnusableReason::Missing => "missing",
        CertificateUnusableReason::HostnameMismatch => "hostname_mismatch",
        CertificateUnusableReason::InvalidChain => "invalid_chain",
        CertificateUnusableReason::UnauthorizedBinding => "unauthorized_binding",
        CertificateUnusableReason::MissingPrivateKey => "missing_private_key",
        CertificateUnusableReason::UnsafeMaterial => "unsafe_material",
        CertificateUnusableReason::Expired => "expired",
        CertificateUnusableReason::KnownRevoked => "known_revoked",
        CertificateUnusableReason::FreshnessUnknown => "freshness_unknown",
        CertificateUnusableReason::SafetyWindowTooShort => "safety_window_too_short",
        CertificateUnusableReason::ServingMaterialUnavailable => "serving_material_unavailable",
        CertificateUnusableReason::ActivationRejected => "activation_rejected",
    }
}

fn unusable_reason_from_code(value: &str) -> Result<CertificateUnusableReason, DomainFactError> {
    match value {
        "missing" => Ok(CertificateUnusableReason::Missing),
        "hostname_mismatch" => Ok(CertificateUnusableReason::HostnameMismatch),
        "invalid_chain" => Ok(CertificateUnusableReason::InvalidChain),
        "unauthorized_binding" => Ok(CertificateUnusableReason::UnauthorizedBinding),
        "missing_private_key" => Ok(CertificateUnusableReason::MissingPrivateKey),
        "unsafe_material" => Ok(CertificateUnusableReason::UnsafeMaterial),
        "expired" => Ok(CertificateUnusableReason::Expired),
        "known_revoked" => Ok(CertificateUnusableReason::KnownRevoked),
        "freshness_unknown" => Ok(CertificateUnusableReason::FreshnessUnknown),
        "safety_window_too_short" => Ok(CertificateUnusableReason::SafetyWindowTooShort),
        "serving_material_unavailable" => Ok(CertificateUnusableReason::ServingMaterialUnavailable),
        "activation_rejected" => Ok(CertificateUnusableReason::ActivationRejected),
        _ => Err(DomainFactError::InvalidPayload),
    }
}

fn certificate_failure_code(failure: &CertificateFailure) -> &'static str {
    match failure {
        CertificateFailure::UnauthorizedBinding => "unauthorized_binding",
        CertificateFailure::ChallengeFailed => "challenge_failed",
        CertificateFailure::IssuanceFailed => "issuance_failed",
        CertificateFailure::MaterialUnsafe => "material_unsafe",
        CertificateFailure::SafetyWindowFailed => "safety_window_failed",
        CertificateFailure::KnownRevoked => "known_revoked",
        CertificateFailure::FreshnessUnknown => "freshness_unknown",
        CertificateFailure::ActivationRejected => "activation_rejected",
    }
}

fn certificate_failure_from_code(value: &str) -> Result<CertificateFailure, DomainFactError> {
    match value {
        "unauthorized_binding" => Ok(CertificateFailure::UnauthorizedBinding),
        "challenge_failed" => Ok(CertificateFailure::ChallengeFailed),
        "issuance_failed" => Ok(CertificateFailure::IssuanceFailed),
        "material_unsafe" => Ok(CertificateFailure::MaterialUnsafe),
        "safety_window_failed" => Ok(CertificateFailure::SafetyWindowFailed),
        "known_revoked" => Ok(CertificateFailure::KnownRevoked),
        "freshness_unknown" => Ok(CertificateFailure::FreshnessUnknown),
        "activation_rejected" => Ok(CertificateFailure::ActivationRejected),
        _ => Err(DomainFactError::InvalidPayload),
    }
}

fn serving_failure_code(failure: &ServingFailure) -> &'static str {
    match failure {
        ServingFailure::SnapshotRejected => "snapshot_rejected",
        ServingFailure::ProjectionStale => "projection_stale",
        ServingFailure::CertificateUnusable => "certificate_unusable",
        ServingFailure::ReloadFailed => "reload_failed",
        ServingFailure::LastGoodExpired => "last_good_expired",
        ServingFailure::LiveObservationUnknown => "live_observation_unknown",
    }
}

fn serving_failure_from_code(value: &str) -> Result<ServingFailure, DomainFactError> {
    match value {
        "snapshot_rejected" => Ok(ServingFailure::SnapshotRejected),
        "projection_stale" => Ok(ServingFailure::ProjectionStale),
        "certificate_unusable" => Ok(ServingFailure::CertificateUnusable),
        "reload_failed" => Ok(ServingFailure::ReloadFailed),
        "last_good_expired" => Ok(ServingFailure::LastGoodExpired),
        "live_observation_unknown" => Ok(ServingFailure::LiveObservationUnknown),
        _ => Err(DomainFactError::InvalidPayload),
    }
}

fn system_time_to_secs(value: SystemTime) -> Result<u64, DomainFactError> {
    value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| DomainFactError::InvalidPayload)
        .map(|duration| duration.as_secs())
}

fn system_time_from_secs(value: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DomainFactPayload {
    Pending {
        domain: String,
        reason: String,
    },
    Ready {
        domain: String,
        certificate: CertificatePayload,
        serving_generation: u64,
    },
    Failed {
        domain: String,
        failure: FailurePayload,
    },
}

impl TryFrom<DomainFactPayload> for DomainStatusEvent {
    type Error = DomainFactError;

    fn try_from(value: DomainFactPayload) -> Result<Self, Self::Error> {
        match value {
            DomainFactPayload::Pending { domain, reason } => Ok(Self::Pending {
                domain: DomainName::parse(domain).map_err(|_| DomainFactError::InvalidPayload)?,
                reason: pending_reason_from_code(&reason)?,
            }),
            DomainFactPayload::Ready {
                domain,
                certificate,
                serving_generation,
            } => {
                let domain =
                    DomainName::parse(domain).map_err(|_| DomainFactError::InvalidPayload)?;
                let certificate = certificate.try_into_certificate()?;
                let serving_generation = ServingGeneration::new(serving_generation);
                let record = DomainReadyRecord::new(domain, certificate, serving_generation)
                    .map_err(|_| DomainFactError::InvalidPayload)?;
                Ok(Self::Ready(record))
            }
            DomainFactPayload::Failed { domain, failure } => Ok(Self::Failed {
                domain: DomainName::parse(domain).map_err(|_| DomainFactError::InvalidPayload)?,
                failure: failure.try_into_failure()?,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CertificatePayload {
    hostname: String,
    not_after_unix_seconds: u64,
    activation: String,
    material: String,
    revocation: String,
}

impl CertificatePayload {
    fn from_certificate(certificate: &CertificateUsability) -> Result<Self, DomainFactError> {
        Ok(Self {
            hostname: certificate.hostname.as_str().to_owned(),
            not_after_unix_seconds: system_time_to_secs(certificate.not_after)?,
            activation: certificate_activation_code(&certificate.activation).to_owned(),
            material: certificate_material_code(&certificate.material).to_owned(),
            revocation: revocation_freshness_code(&certificate.revocation).to_owned(),
        })
    }

    fn try_into_certificate(self) -> Result<CertificateUsability, DomainFactError> {
        Ok(CertificateUsability {
            hostname: Hostname::parse(self.hostname)
                .map_err(|_| DomainFactError::InvalidPayload)?,
            not_after: system_time_from_secs(self.not_after_unix_seconds),
            activation: certificate_activation_from_code(&self.activation)?,
            material: certificate_material_from_code(&self.material)?,
            revocation: revocation_freshness_from_code(&self.revocation)?,
        })
    }
}

fn certificate_activation_code(value: &CertificateActivation) -> &'static str {
    match value {
        CertificateActivation::Acknowledged => "acknowledged",
        CertificateActivation::Rejected => "rejected",
        CertificateActivation::Pending => "pending",
        CertificateActivation::Unknown => "unknown",
    }
}

fn certificate_activation_from_code(value: &str) -> Result<CertificateActivation, DomainFactError> {
    match value {
        "acknowledged" => Ok(CertificateActivation::Acknowledged),
        "rejected" => Ok(CertificateActivation::Rejected),
        "pending" => Ok(CertificateActivation::Pending),
        "unknown" => Ok(CertificateActivation::Unknown),
        _ => Err(DomainFactError::InvalidPayload),
    }
}

fn certificate_material_code(value: &CertificateMaterialState) -> &'static str {
    match value {
        CertificateMaterialState::PresentProtected => "present_protected",
        CertificateMaterialState::Missing => "missing",
        CertificateMaterialState::Unsafe => "unsafe",
    }
}

fn certificate_material_from_code(
    value: &str,
) -> Result<CertificateMaterialState, DomainFactError> {
    match value {
        "present_protected" => Ok(CertificateMaterialState::PresentProtected),
        "missing" => Ok(CertificateMaterialState::Missing),
        "unsafe" => Ok(CertificateMaterialState::Unsafe),
        _ => Err(DomainFactError::InvalidPayload),
    }
}

fn revocation_freshness_code(value: &RevocationFreshness) -> &'static str {
    match value {
        RevocationFreshness::KnownFresh => "known_fresh",
        RevocationFreshness::KnownRevoked => "known_revoked",
        RevocationFreshness::Unknown => "unknown",
    }
}

fn revocation_freshness_from_code(value: &str) -> Result<RevocationFreshness, DomainFactError> {
    match value {
        "known_fresh" => Ok(RevocationFreshness::KnownFresh),
        "known_revoked" => Ok(RevocationFreshness::KnownRevoked),
        "unknown" => Ok(RevocationFreshness::Unknown),
        _ => Err(DomainFactError::InvalidPayload),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FailurePayload {
    InvalidDomain,
    ClaimRejected,
    ClaimResourceMismatch,
    StaleClaim,
    CertificateUnusable { reason: String },
    CertificateFailed { failure: String },
    ServingActivationFailed,
    ServingFailed { failure: String },
    StatusUnavailable,
    UnknownReadiness,
}

impl FailurePayload {
    fn from_failure(failure: &DomainFailure) -> Self {
        match failure {
            DomainFailure::InvalidDomain => Self::InvalidDomain,
            DomainFailure::ClaimRejected => Self::ClaimRejected,
            DomainFailure::ClaimResourceMismatch => Self::ClaimResourceMismatch,
            DomainFailure::StaleClaim => Self::StaleClaim,
            DomainFailure::CertificateUnusable(reason) => Self::CertificateUnusable {
                reason: unusable_reason_code(reason).to_owned(),
            },
            DomainFailure::CertificateFailed(failure) => Self::CertificateFailed {
                failure: certificate_failure_code(failure).to_owned(),
            },
            DomainFailure::ServingActivationFailed => Self::ServingActivationFailed,
            DomainFailure::ServingFailed(failure) => Self::ServingFailed {
                failure: serving_failure_code(failure).to_owned(),
            },
            DomainFailure::StatusUnavailable => Self::StatusUnavailable,
            DomainFailure::UnknownReadiness => Self::UnknownReadiness,
        }
    }

    fn try_into_failure(self) -> Result<DomainFailure, DomainFactError> {
        match self {
            Self::InvalidDomain => Ok(DomainFailure::InvalidDomain),
            Self::ClaimRejected => Ok(DomainFailure::ClaimRejected),
            Self::ClaimResourceMismatch => Ok(DomainFailure::ClaimResourceMismatch),
            Self::StaleClaim => Ok(DomainFailure::StaleClaim),
            Self::CertificateUnusable { reason } => Ok(DomainFailure::CertificateUnusable(
                unusable_reason_from_code(&reason)?,
            )),
            Self::CertificateFailed { failure } => Ok(DomainFailure::CertificateFailed(
                certificate_failure_from_code(&failure)?,
            )),
            Self::ServingActivationFailed => Ok(DomainFailure::ServingActivationFailed),
            Self::ServingFailed { failure } => Ok(DomainFailure::ServingFailed(
                serving_failure_from_code(&failure)?,
            )),
            Self::StatusUnavailable => Ok(DomainFailure::StatusUnavailable),
            Self::UnknownReadiness => Ok(DomainFailure::UnknownReadiness),
        }
    }

    fn key_fragment(&self) -> &'static str {
        match self {
            Self::InvalidDomain => "invalid_domain",
            Self::ClaimRejected => "claim_rejected",
            Self::ClaimResourceMismatch => "claim_resource_mismatch",
            Self::StaleClaim => "stale_claim",
            Self::CertificateUnusable { .. } => "certificate_unusable",
            Self::CertificateFailed { .. } => "certificate_failed",
            Self::ServingActivationFailed => "serving_activation_failed",
            Self::ServingFailed { .. } => "serving_failed",
            Self::StatusUnavailable => "status_unavailable",
            Self::UnknownReadiness => "unknown_readiness",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_event_round_trips_through_product_payload() {
        let event = DomainStatusEvent::ready(ready_record());

        let envelope = event.encode().expect("encode");
        let decoded =
            DomainStatusEvent::decode_payload(envelope.payload().as_bytes()).expect("decode");

        assert_eq!(decoded, event);
        assert_eq!(
            envelope.target().resource().as_str(),
            "domain-status:app.example.com"
        );
        assert!(
            envelope
                .target()
                .key()
                .as_str()
                .starts_with("/facts/domain/app.example.com/ready/7/")
        );
        assert_eq!(envelope.target().kind().as_str(), DOMAIN_READY_KIND);
    }

    #[test]
    fn failure_event_preserves_structured_failure() {
        let event = DomainStatusEvent::failed(
            domain(),
            DomainFailure::CertificateUnusable(CertificateUnusableReason::SafetyWindowTooShort),
        );

        let envelope = event.encode().expect("encode");
        let decoded =
            DomainStatusEvent::decode_payload(envelope.payload().as_bytes()).expect("decode");

        assert_eq!(decoded, event);
        assert!(
            envelope
                .target()
                .key()
                .as_str()
                .contains("/failed/certificate_unusable/")
        );
    }

    #[test]
    fn reducer_returns_latest_cursor_ordered_status() {
        let reducer = DomainStatusReducer::new(domain());

        let status = reducer
            .reduce([
                DomainStatusEvent::pending(domain(), DomainPendingReason::CertificateIssuance),
                DomainStatusEvent::ready(ready_record()),
            ])
            .expect("status");

        assert!(matches!(status, DomainStatus::Ready(_)));
    }

    #[test]
    fn reducer_rejects_mismatched_domain() {
        let reducer = DomainStatusReducer::new(domain());

        let result = reducer.reduce([DomainStatusEvent::pending(
            DomainName::parse("other.example.com").expect("domain"),
            DomainPendingReason::CertificateIssuance,
        )]);

        assert_eq!(result, Err(DomainFactError::TargetMismatch));
    }

    fn ready_record() -> DomainReadyRecord {
        DomainReadyRecord::new(
            domain(),
            CertificateUsability {
                hostname: Hostname::parse("app.example.com").expect("hostname"),
                not_after: UNIX_EPOCH + Duration::from_secs(86_400),
                activation: CertificateActivation::Acknowledged,
                material: CertificateMaterialState::PresentProtected,
                revocation: RevocationFreshness::KnownFresh,
            },
            ServingGeneration::new(7),
        )
        .expect("ready record")
    }

    fn domain() -> DomainName {
        DomainName::parse("app.example.com").expect("domain")
    }
}

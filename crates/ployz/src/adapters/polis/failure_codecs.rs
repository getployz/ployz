use serde::{Deserialize, Serialize};

use crate::acme::CertificateUnusableReason;
use crate::domain::DomainStatusFailure;
use crate::error::{CertificateFailure, ServingFailure};

pub(super) fn certificate_failure_payload(
    failure: &CertificateFailure,
) -> Result<String, polis::StoreError> {
    serde_json::to_string(&StoredCertificateFailure::from(failure))
        .map_err(|_| polis::StoreError::MalformedPayload)
}

pub(super) fn certificate_failure_from_payload(
    payload: &str,
) -> Result<CertificateFailure, polis::StoreError> {
    let stored = serde_json::from_str::<StoredCertificateFailure>(payload)
        .map_err(|_| polis::StoreError::MalformedPayload)?;
    Ok(stored.into())
}

pub(super) fn domain_status_failure_payload(
    failure: &DomainStatusFailure,
) -> Result<String, polis::StoreError> {
    serde_json::to_string(&StoredDomainStatusFailure::from(failure))
        .map_err(|_| polis::StoreError::MalformedPayload)
}

pub(super) fn domain_status_failure_from_payload(
    payload: &str,
) -> Result<DomainStatusFailure, polis::StoreError> {
    let stored = serde_json::from_str::<StoredDomainStatusFailure>(payload)
        .map_err(|_| polis::StoreError::MalformedPayload)?;
    Ok(stored.into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredCertificateFailure {
    UnauthorizedBinding,
    ChallengeFailed,
    IssuanceFailed,
    MaterialUnsafe,
    SafetyWindowFailed,
    KnownRevoked,
    FreshnessUnknown,
    ActivationRejected,
    FailureRecordUnavailable {
        primary: Box<StoredCertificateFailure>,
        evidence: Box<StoredCertificateFailure>,
    },
}

impl From<&CertificateFailure> for StoredCertificateFailure {
    fn from(failure: &CertificateFailure) -> Self {
        match failure {
            CertificateFailure::UnauthorizedBinding => Self::UnauthorizedBinding,
            CertificateFailure::ChallengeFailed => Self::ChallengeFailed,
            CertificateFailure::IssuanceFailed => Self::IssuanceFailed,
            CertificateFailure::MaterialUnsafe => Self::MaterialUnsafe,
            CertificateFailure::SafetyWindowFailed => Self::SafetyWindowFailed,
            CertificateFailure::KnownRevoked => Self::KnownRevoked,
            CertificateFailure::FreshnessUnknown => Self::FreshnessUnknown,
            CertificateFailure::ActivationRejected => Self::ActivationRejected,
            CertificateFailure::FailureRecordUnavailable { primary, evidence } => {
                Self::FailureRecordUnavailable {
                    primary: Box::new(Self::from(primary.as_ref())),
                    evidence: Box::new(Self::from(evidence.as_ref())),
                }
            }
        }
    }
}

impl From<StoredCertificateFailure> for CertificateFailure {
    fn from(failure: StoredCertificateFailure) -> Self {
        match failure {
            StoredCertificateFailure::UnauthorizedBinding => Self::UnauthorizedBinding,
            StoredCertificateFailure::ChallengeFailed => Self::ChallengeFailed,
            StoredCertificateFailure::IssuanceFailed => Self::IssuanceFailed,
            StoredCertificateFailure::MaterialUnsafe => Self::MaterialUnsafe,
            StoredCertificateFailure::SafetyWindowFailed => Self::SafetyWindowFailed,
            StoredCertificateFailure::KnownRevoked => Self::KnownRevoked,
            StoredCertificateFailure::FreshnessUnknown => Self::FreshnessUnknown,
            StoredCertificateFailure::ActivationRejected => Self::ActivationRejected,
            StoredCertificateFailure::FailureRecordUnavailable { primary, evidence } => {
                Self::FailureRecordUnavailable {
                    primary: Box::new((*primary).into()),
                    evidence: Box::new((*evidence).into()),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredDomainStatusFailure {
    InvalidDomain,
    ClaimRejected,
    ClaimResourceMismatch,
    StaleClaim,
    CertificateUnusable {
        reason: StoredCertificateUnusableReason,
    },
    CertificateFailed {
        failure: StoredCertificateFailure,
    },
    ServingActivationFailed,
    ServingFailed {
        failure: StoredServingFailure,
    },
}

impl From<&DomainStatusFailure> for StoredDomainStatusFailure {
    fn from(failure: &DomainStatusFailure) -> Self {
        match failure {
            DomainStatusFailure::InvalidDomain => Self::InvalidDomain,
            DomainStatusFailure::ClaimRejected => Self::ClaimRejected,
            DomainStatusFailure::ClaimResourceMismatch => Self::ClaimResourceMismatch,
            DomainStatusFailure::StaleClaim => Self::StaleClaim,
            DomainStatusFailure::CertificateUnusable(reason) => Self::CertificateUnusable {
                reason: StoredCertificateUnusableReason::from(reason),
            },
            DomainStatusFailure::CertificateFailed(failure) => Self::CertificateFailed {
                failure: StoredCertificateFailure::from(failure),
            },
            DomainStatusFailure::ServingActivationFailed => Self::ServingActivationFailed,
            DomainStatusFailure::ServingFailed(failure) => Self::ServingFailed {
                failure: StoredServingFailure::from(failure),
            },
        }
    }
}

impl From<StoredDomainStatusFailure> for DomainStatusFailure {
    fn from(failure: StoredDomainStatusFailure) -> Self {
        match failure {
            StoredDomainStatusFailure::InvalidDomain => Self::InvalidDomain,
            StoredDomainStatusFailure::ClaimRejected => Self::ClaimRejected,
            StoredDomainStatusFailure::ClaimResourceMismatch => Self::ClaimResourceMismatch,
            StoredDomainStatusFailure::StaleClaim => Self::StaleClaim,
            StoredDomainStatusFailure::CertificateUnusable { reason } => {
                Self::CertificateUnusable(reason.into())
            }
            StoredDomainStatusFailure::CertificateFailed { failure } => {
                Self::CertificateFailed(failure.into())
            }
            StoredDomainStatusFailure::ServingActivationFailed => Self::ServingActivationFailed,
            StoredDomainStatusFailure::ServingFailed { failure } => {
                Self::ServingFailed(failure.into())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredCertificateUnusableReason {
    Missing,
    HostnameMismatch,
    InvalidChain,
    UnauthorizedBinding,
    MissingPrivateKey,
    UnsafeMaterial,
    Expired,
    KnownRevoked,
    FreshnessUnknown,
    SafetyWindowTooShort,
    ServingMaterialUnavailable,
    ActivationRejected,
}

impl From<&CertificateUnusableReason> for StoredCertificateUnusableReason {
    fn from(reason: &CertificateUnusableReason) -> Self {
        match reason {
            CertificateUnusableReason::Missing => Self::Missing,
            CertificateUnusableReason::HostnameMismatch => Self::HostnameMismatch,
            CertificateUnusableReason::InvalidChain => Self::InvalidChain,
            CertificateUnusableReason::UnauthorizedBinding => Self::UnauthorizedBinding,
            CertificateUnusableReason::MissingPrivateKey => Self::MissingPrivateKey,
            CertificateUnusableReason::UnsafeMaterial => Self::UnsafeMaterial,
            CertificateUnusableReason::Expired => Self::Expired,
            CertificateUnusableReason::KnownRevoked => Self::KnownRevoked,
            CertificateUnusableReason::FreshnessUnknown => Self::FreshnessUnknown,
            CertificateUnusableReason::SafetyWindowTooShort => Self::SafetyWindowTooShort,
            CertificateUnusableReason::ServingMaterialUnavailable => {
                Self::ServingMaterialUnavailable
            }
            CertificateUnusableReason::ActivationRejected => Self::ActivationRejected,
        }
    }
}

impl From<StoredCertificateUnusableReason> for CertificateUnusableReason {
    fn from(reason: StoredCertificateUnusableReason) -> Self {
        match reason {
            StoredCertificateUnusableReason::Missing => Self::Missing,
            StoredCertificateUnusableReason::HostnameMismatch => Self::HostnameMismatch,
            StoredCertificateUnusableReason::InvalidChain => Self::InvalidChain,
            StoredCertificateUnusableReason::UnauthorizedBinding => Self::UnauthorizedBinding,
            StoredCertificateUnusableReason::MissingPrivateKey => Self::MissingPrivateKey,
            StoredCertificateUnusableReason::UnsafeMaterial => Self::UnsafeMaterial,
            StoredCertificateUnusableReason::Expired => Self::Expired,
            StoredCertificateUnusableReason::KnownRevoked => Self::KnownRevoked,
            StoredCertificateUnusableReason::FreshnessUnknown => Self::FreshnessUnknown,
            StoredCertificateUnusableReason::SafetyWindowTooShort => Self::SafetyWindowTooShort,
            StoredCertificateUnusableReason::ServingMaterialUnavailable => {
                Self::ServingMaterialUnavailable
            }
            StoredCertificateUnusableReason::ActivationRejected => Self::ActivationRejected,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredServingFailure {
    SnapshotRejected,
    SnapshotStale,
    CertificateUnusable,
    ReloadFailed,
    LastGoodExpired,
    LiveObservationUnknown,
}

impl From<&ServingFailure> for StoredServingFailure {
    fn from(failure: &ServingFailure) -> Self {
        match failure {
            ServingFailure::SnapshotRejected => Self::SnapshotRejected,
            ServingFailure::SnapshotStale => Self::SnapshotStale,
            ServingFailure::CertificateUnusable => Self::CertificateUnusable,
            ServingFailure::ReloadFailed => Self::ReloadFailed,
            ServingFailure::LastGoodExpired => Self::LastGoodExpired,
            ServingFailure::LiveObservationUnknown => Self::LiveObservationUnknown,
        }
    }
}

impl From<StoredServingFailure> for ServingFailure {
    fn from(failure: StoredServingFailure) -> Self {
        match failure {
            StoredServingFailure::SnapshotRejected => Self::SnapshotRejected,
            StoredServingFailure::SnapshotStale => Self::SnapshotStale,
            StoredServingFailure::CertificateUnusable => Self::CertificateUnusable,
            StoredServingFailure::ReloadFailed => Self::ReloadFailed,
            StoredServingFailure::LastGoodExpired => Self::LastGoodExpired,
            StoredServingFailure::LiveObservationUnknown => Self::LiveObservationUnknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_failure_payload_preserves_recording_failure() {
        let failure = CertificateFailure::FailureRecordUnavailable {
            primary: Box::new(CertificateFailure::ChallengeFailed),
            evidence: Box::new(CertificateFailure::IssuanceFailed),
        };

        let payload = certificate_failure_payload(&failure).expect("encode");

        assert_eq!(certificate_failure_from_payload(&payload), Ok(failure));
    }

    #[test]
    fn domain_status_failure_payload_preserves_domain_failure() {
        let failure = DomainStatusFailure::CertificateFailed(CertificateFailure::ChallengeFailed);

        let payload = domain_status_failure_payload(&failure).expect("encode");

        assert_eq!(domain_status_failure_from_payload(&payload), Ok(failure));
    }
}

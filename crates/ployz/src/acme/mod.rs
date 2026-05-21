//! Certificate and ACME product ports.

use std::time::SystemTime;

use crate::deploy::MutationContext;
use crate::error::CertificateFailure;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hostname(String);

impl Hostname {
    pub fn parse(value: impl Into<String>) -> std::result::Result<Self, CertificateFailure> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CertificateFailure::UnauthorizedBinding);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpsBinding {
    pub hostname: Hostname,
}

impl HttpsBinding {
    #[must_use]
    pub fn new(hostname: Hostname) -> Self {
        Self { hostname }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateDeadline {
    pub expires_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureCertificateOutcome {
    Usable(CertificateUsability),
    Unusable(CertificateUnusableReason),
    FreshnessUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateUsability {
    pub hostname: Hostname,
    pub not_after: SystemTime,
    pub activation: CertificateActivation,
    pub material: CertificateMaterialState,
    pub revocation: RevocationFreshness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateUnusableReason {
    Missing,
    HostnameMismatch,
    InvalidChain,
    UnauthorizedBinding,
    MissingPrivateKey,
    UnsafeMaterial,
    Expired,
    KnownRevoked,
    SafetyWindowTooShort,
    ServingMaterialUnavailable,
    ActivationRejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateActivation {
    Acknowledged,
    Rejected,
    Pending,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateMaterialState {
    PresentProtected,
    Missing,
    Unsafe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevocationFreshness {
    KnownFresh,
    KnownRevoked,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateStatus {
    Absent,
    Present(CertificateUsability),
    Unusable(CertificateUnusableReason),
    Unknown,
}

pub trait CertificatePort {
    fn ensure_usable(
        &self,
        context: &MutationContext,
        binding: &HttpsBinding,
        deadline: CertificateDeadline,
    ) -> Result<EnsureCertificateOutcome, CertificateFailure>;

    fn status(&self, binding: &HttpsBinding) -> Result<CertificateStatus, CertificateFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChallengeSlot(String);

impl ChallengeSlot {
    pub fn parse(value: impl Into<String>) -> std::result::Result<Self, CertificateFailure> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CertificateFailure::UnauthorizedBinding);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChallengeOwnership {
    Owned { slot: ChallengeSlot },
    Rejected(CertificateFailure),
}

pub trait ChallengeOwnershipPort {
    fn claim_challenge(
        &self,
        context: &MutationContext,
        binding: &HttpsBinding,
    ) -> Result<ChallengeOwnership, CertificateFailure>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_hostname_is_rejected() {
        assert_eq!(
            Hostname::parse(""),
            Err(CertificateFailure::UnauthorizedBinding)
        );
    }

    #[test]
    fn missing_certificate_is_not_success() {
        let outcome = EnsureCertificateOutcome::Unusable(CertificateUnusableReason::Missing);

        assert!(matches!(
            outcome,
            EnsureCertificateOutcome::Unusable(CertificateUnusableReason::Missing)
        ));
    }

    #[test]
    fn empty_challenge_slot_is_rejected() {
        assert_eq!(
            ChallengeSlot::parse(""),
            Err(CertificateFailure::UnauthorizedBinding)
        );
    }
}

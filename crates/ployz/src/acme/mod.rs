//! Certificate and ACME product ports.

use std::time::SystemTime;

use crate::error::CertificateFailure;
use crate::operation::MutationContext;

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
pub struct CertificateIssueRequest {
    pub binding: HttpsBinding,
    pub deadline: CertificateDeadline,
}

impl CertificateIssueRequest {
    #[must_use]
    pub fn new(binding: HttpsBinding, deadline: CertificateDeadline) -> Self {
        Self { binding, deadline }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureCertificateOutcome {
    Usable(CertificateUsability),
    Unusable(CertificateUnusableReason),
    Waiting(CertificateWaitReason),
    FreshnessUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateWaitReason {
    IssuanceInProgress,
    IssuanceInterrupted,
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
    FreshnessUnknown,
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

pub trait CertificateStatusPort {
    fn status(&self, binding: &HttpsBinding) -> Result<CertificateStatus, CertificateFailure>;
}

pub trait CertificateAuthorityPort {
    fn issue_certificate(
        &self,
        context: &MutationContext,
        request: &CertificateIssueRequest,
    ) -> Result<(), CertificateFailure>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateIssueOutcome {
    Issued,
    AlreadyIssued,
    InProgress,
    Interrupted,
}

pub trait CertificateIssuerPort {
    fn issue_certificate(
        &self,
        context: &MutationContext,
        request: &CertificateIssueRequest,
    ) -> Result<CertificateIssueOutcome, CertificateFailure>;
}

pub struct CertificateReadinessService<S, I> {
    certificates: S,
    issuer: I,
}

impl<S, I> CertificateReadinessService<S, I> {
    #[must_use]
    pub fn new(certificates: S, issuer: I) -> Self {
        Self {
            certificates,
            issuer,
        }
    }
}

impl<S, I> CertificatePort for CertificateReadinessService<S, I>
where
    S: CertificateStatusPort,
    I: CertificateIssuerPort,
{
    fn ensure_usable(
        &self,
        context: &MutationContext,
        binding: &HttpsBinding,
        deadline: CertificateDeadline,
    ) -> Result<EnsureCertificateOutcome, CertificateFailure> {
        match status_to_outcome(self.certificates.status(binding)?, binding, &deadline) {
            EnsureCertificateOutcome::Usable(certificate) => {
                return Ok(EnsureCertificateOutcome::Usable(certificate));
            }
            EnsureCertificateOutcome::Unusable(_) => {}
            EnsureCertificateOutcome::Waiting(reason) => {
                return Ok(EnsureCertificateOutcome::Waiting(reason));
            }
            EnsureCertificateOutcome::FreshnessUnknown => {
                return Ok(EnsureCertificateOutcome::FreshnessUnknown);
            }
        }

        let request = CertificateIssueRequest::new(binding.clone(), deadline);
        match self.issuer.issue_certificate(context, &request)? {
            CertificateIssueOutcome::Issued | CertificateIssueOutcome::AlreadyIssued => {
                Ok(status_to_outcome(
                    self.certificates.status(binding)?,
                    binding,
                    &request.deadline,
                ))
            }
            CertificateIssueOutcome::InProgress => Ok(EnsureCertificateOutcome::Waiting(
                CertificateWaitReason::IssuanceInProgress,
            )),
            CertificateIssueOutcome::Interrupted => Ok(EnsureCertificateOutcome::Waiting(
                CertificateWaitReason::IssuanceInterrupted,
            )),
        }
    }

    fn status(&self, binding: &HttpsBinding) -> Result<CertificateStatus, CertificateFailure> {
        self.certificates.status(binding)
    }
}

#[must_use]
pub fn certificate_is_usable(
    certificate: &CertificateUsability,
    binding: &HttpsBinding,
    minimum_valid_until: SystemTime,
) -> bool {
    certificate_unusable_reason(certificate, binding, minimum_valid_until).is_none()
}

#[must_use]
pub fn certificate_unusable_reason(
    certificate: &CertificateUsability,
    binding: &HttpsBinding,
    minimum_valid_until: SystemTime,
) -> Option<CertificateUnusableReason> {
    if certificate.hostname != binding.hostname {
        return Some(CertificateUnusableReason::HostnameMismatch);
    }
    if certificate.activation != CertificateActivation::Acknowledged {
        return Some(CertificateUnusableReason::ActivationRejected);
    }
    if certificate.material != CertificateMaterialState::PresentProtected {
        return Some(CertificateUnusableReason::UnsafeMaterial);
    }
    match certificate.revocation {
        RevocationFreshness::KnownFresh => {}
        RevocationFreshness::KnownRevoked => {
            return Some(CertificateUnusableReason::KnownRevoked);
        }
        RevocationFreshness::Unknown => return Some(CertificateUnusableReason::FreshnessUnknown),
    }
    if certificate.not_after < minimum_valid_until {
        return Some(CertificateUnusableReason::SafetyWindowTooShort);
    }
    None
}

#[must_use]
fn status_to_outcome(
    status: CertificateStatus,
    binding: &HttpsBinding,
    deadline: &CertificateDeadline,
) -> EnsureCertificateOutcome {
    match status {
        CertificateStatus::Present(certificate)
            if certificate_is_usable(&certificate, binding, deadline.expires_at) =>
        {
            EnsureCertificateOutcome::Usable(certificate)
        }
        CertificateStatus::Present(certificate) => EnsureCertificateOutcome::Unusable(
            certificate_unusable_reason(&certificate, binding, deadline.expires_at)
                .unwrap_or(CertificateUnusableReason::InvalidChain),
        ),
        CertificateStatus::Absent => {
            EnsureCertificateOutcome::Unusable(CertificateUnusableReason::Missing)
        }
        CertificateStatus::Unusable(reason) => EnsureCertificateOutcome::Unusable(reason),
        CertificateStatus::Unknown => EnsureCertificateOutcome::FreshnessUnknown,
    }
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
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, MutationContext, OperationId,
        PrincipalId, ScopeId,
    };
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{Duration, UNIX_EPOCH};

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

    #[test]
    fn existing_usable_certificate_skips_issuance() {
        let status = FakeCertificateStatus::new(CertificateStatus::Present(certificate()));
        let issuer = FakeIssuer::default();
        let service = CertificateReadinessService::new(status, issuer.clone());

        let outcome = service
            .ensure_usable(
                &context(),
                &binding(),
                CertificateDeadline {
                    expires_at: UNIX_EPOCH + Duration::from_secs(3_600),
                },
            )
            .expect("ensure");

        assert!(matches!(outcome, EnsureCertificateOutcome::Usable(_)));
        assert_eq!(*issuer.calls.borrow(), 0);
    }

    #[test]
    fn missing_certificate_issues_then_reobserves_status() {
        let status = FakeCertificateStatus::new(CertificateStatus::Absent);
        status.push(CertificateStatus::Present(certificate()));
        let issuer = FakeIssuer::default();
        let service = CertificateReadinessService::new(status, issuer.clone());

        let outcome = service
            .ensure_usable(
                &context(),
                &binding(),
                CertificateDeadline {
                    expires_at: UNIX_EPOCH + Duration::from_secs(3_600),
                },
            )
            .expect("ensure");

        assert!(matches!(outcome, EnsureCertificateOutcome::Usable(_)));
        assert_eq!(*issuer.calls.borrow(), 1);
    }

    #[test]
    fn completed_issuance_still_requires_observed_usable_certificate() {
        let status = FakeCertificateStatus::new(CertificateStatus::Absent);
        let issuer = FakeIssuer::with_outcome(CertificateIssueOutcome::AlreadyIssued);
        let service = CertificateReadinessService::new(status, issuer);

        let outcome = service
            .ensure_usable(
                &context(),
                &binding(),
                CertificateDeadline {
                    expires_at: UNIX_EPOCH + Duration::from_secs(3_600),
                },
            )
            .expect("ensure");

        assert!(matches!(
            outcome,
            EnsureCertificateOutcome::Unusable(CertificateUnusableReason::Missing)
        ));
    }

    #[test]
    fn in_progress_issuance_reports_wait_state() {
        let status = FakeCertificateStatus::new(CertificateStatus::Absent);
        let issuer = FakeIssuer::with_outcome(CertificateIssueOutcome::InProgress);
        let service = CertificateReadinessService::new(status, issuer);

        let outcome = service
            .ensure_usable(
                &context(),
                &binding(),
                CertificateDeadline {
                    expires_at: UNIX_EPOCH + Duration::from_secs(3_600),
                },
            )
            .expect("ensure");

        assert_eq!(
            outcome,
            EnsureCertificateOutcome::Waiting(CertificateWaitReason::IssuanceInProgress)
        );
    }

    #[test]
    fn interrupted_issuance_reports_wait_state() {
        let status = FakeCertificateStatus::new(CertificateStatus::Absent);
        let issuer = FakeIssuer::with_outcome(CertificateIssueOutcome::Interrupted);
        let service = CertificateReadinessService::new(status, issuer);

        let outcome = service
            .ensure_usable(
                &context(),
                &binding(),
                CertificateDeadline {
                    expires_at: UNIX_EPOCH + Duration::from_secs(3_600),
                },
            )
            .expect("ensure");

        assert_eq!(
            outcome,
            EnsureCertificateOutcome::Waiting(CertificateWaitReason::IssuanceInterrupted)
        );
    }

    #[derive(Clone)]
    struct FakeCertificateStatus {
        statuses: Rc<RefCell<Vec<CertificateStatus>>>,
    }

    impl FakeCertificateStatus {
        fn new(status: CertificateStatus) -> Self {
            Self {
                statuses: Rc::new(RefCell::new(vec![status])),
            }
        }

        fn push(&self, status: CertificateStatus) {
            self.statuses.borrow_mut().push(status);
        }
    }

    impl CertificateStatusPort for FakeCertificateStatus {
        fn status(&self, _binding: &HttpsBinding) -> Result<CertificateStatus, CertificateFailure> {
            let mut statuses = self.statuses.borrow_mut();
            if statuses.len() > 1 {
                return Ok(statuses.remove(0));
            }
            let Some(status) = statuses.first() else {
                return Ok(CertificateStatus::Unknown);
            };
            Ok(status.clone())
        }
    }

    #[derive(Clone)]
    struct FakeIssuer {
        outcome: CertificateIssueOutcome,
        calls: Rc<RefCell<usize>>,
    }

    impl Default for FakeIssuer {
        fn default() -> Self {
            Self::with_outcome(CertificateIssueOutcome::Issued)
        }
    }

    impl FakeIssuer {
        fn with_outcome(outcome: CertificateIssueOutcome) -> Self {
            Self {
                outcome,
                calls: Rc::new(RefCell::new(0)),
            }
        }
    }

    impl CertificateIssuerPort for FakeIssuer {
        fn issue_certificate(
            &self,
            _context: &MutationContext,
            _request: &CertificateIssueRequest,
        ) -> Result<CertificateIssueOutcome, CertificateFailure> {
            *self.calls.borrow_mut() += 1;
            Ok(self.outcome)
        }
    }

    fn context() -> MutationContext {
        MutationContext::new(
            OperationId::parse("cert-ensure-1").expect("operation"),
            IdempotencyKey::parse("cert-ensure-1").expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn binding() -> HttpsBinding {
        HttpsBinding::new(Hostname::parse("app.example.com").expect("hostname"))
    }

    fn certificate() -> CertificateUsability {
        CertificateUsability {
            hostname: Hostname::parse("app.example.com").expect("hostname"),
            not_after: UNIX_EPOCH + Duration::from_secs(7_200),
            activation: CertificateActivation::Acknowledged,
            material: CertificateMaterialState::PresentProtected,
            revocation: RevocationFreshness::KnownFresh,
        }
    }
}

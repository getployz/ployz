//! Domain HTTPS readiness product primitive.

use std::time::SystemTime;

use thiserror::Error;

use crate::acme::{
    CertificateUnusableReason, CertificateUsability, Hostname, HttpsBinding,
    certificate_unusable_reason,
};
use crate::error::{CertificateFailure, ServingFailure};
use crate::operation::{ClaimGuard, MutationContext, TypedResourceId};
use crate::serving::{
    RouteId, ServingCheckpoint, ServingCommitRequest, ServingGeneration, ServingTarget,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DomainName(String);

impl DomainName {
    pub fn parse(value: impl Into<String>) -> Result<Self, DomainFailure> {
        let value = value.into();
        let value = value.trim();
        if !is_valid_domain_name(value) {
            return Err(DomainFailure::InvalidDomain);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn https_binding(&self) -> Result<HttpsBinding, DomainFailure> {
        let hostname = Hostname::parse(self.0.clone()).map_err(|_| DomainFailure::InvalidDomain)?;
        Ok(HttpsBinding::new(hostname))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainAdd {
    pub domain: DomainName,
    pub certificate_policy: CertificatePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificatePolicy {
    pub minimum_valid_until: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DomainStatus {
    Ready(DomainReadyRecord),
    Pending(DomainPendingReason),
    Failed(DomainFailure),
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainPendingReason {
    CertificateIssuance,
    ServingActivation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainReady {
    domain: DomainName,
    certificate: UsableDomainCertificate,
    serving: DomainServingActivation,
}

impl DomainReady {
    #[must_use]
    pub(crate) fn new(
        domain: DomainName,
        certificate: UsableDomainCertificate,
        serving: DomainServingActivation,
    ) -> Self {
        Self {
            domain,
            certificate,
            serving,
        }
    }

    #[must_use]
    pub fn domain(&self) -> &DomainName {
        &self.domain
    }

    #[must_use]
    pub fn certificate(&self) -> &UsableDomainCertificate {
        &self.certificate
    }

    #[must_use]
    pub fn serving(&self) -> &DomainServingActivation {
        &self.serving
    }

    #[must_use]
    pub fn record(&self) -> DomainReadyRecord {
        DomainReadyRecord {
            domain: self.domain.clone(),
            certificate: self.certificate.certificate().clone(),
            serving_generation: self.serving.checkpoint().generation(),
        }
    }

    #[must_use]
    pub fn serving_commit(
        &self,
        route: RouteId,
        target: ServingTarget,
        generation: ServingGeneration,
    ) -> ServingCommitRequest {
        ServingCommitRequest::new(
            route,
            self.certificate.certificate().hostname.clone(),
            target,
            generation,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainReadyRecord {
    domain: DomainName,
    certificate: CertificateUsability,
    serving_generation: ServingGeneration,
}

impl DomainReadyRecord {
    #[must_use]
    pub fn domain(&self) -> &DomainName {
        &self.domain
    }

    #[must_use]
    pub fn certificate(&self) -> &CertificateUsability {
        &self.certificate
    }

    #[must_use]
    pub fn serving_generation(&self) -> ServingGeneration {
        self.serving_generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsableDomainCertificate {
    certificate: CertificateUsability,
}

impl UsableDomainCertificate {
    pub fn new(
        domain: &DomainName,
        certificate: CertificateUsability,
        policy: CertificatePolicy,
    ) -> Result<Self, DomainFailure> {
        if let Some(reason) = certificate_unusable_reason(
            &certificate,
            &domain.https_binding()?,
            policy.minimum_valid_until,
        ) {
            return Err(DomainFailure::CertificateUnusable(reason));
        }
        Ok(Self { certificate })
    }

    #[must_use]
    pub fn certificate(&self) -> &CertificateUsability {
        &self.certificate
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainServingActivation {
    checkpoint: ServingCheckpoint,
}

impl DomainServingActivation {
    #[must_use]
    pub(crate) fn active(generation: ServingGeneration) -> Self {
        Self {
            checkpoint: ServingCheckpoint::new(generation),
        }
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_active(generation: ServingGeneration) -> Self {
        Self::active(generation)
    }

    #[must_use]
    pub fn checkpoint(&self) -> &ServingCheckpoint {
        &self.checkpoint
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainResource {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainClaim {
    domain: DomainName,
    guard: ClaimGuard<DomainResource>,
}

impl DomainClaim {
    pub(crate) fn new(
        domain: DomainName,
        guard: ClaimGuard<DomainResource>,
    ) -> Result<Self, DomainFailure> {
        let expected = domain_resource(&domain)?;
        if guard.resource() != &expected {
            return Err(DomainFailure::ClaimResourceMismatch);
        }
        Ok(Self { domain, guard })
    }

    #[must_use]
    pub fn domain(&self) -> &DomainName {
        &self.domain
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainClaimObservation {
    pub resource: TypedResourceId<DomainResource>,
    pub holder: crate::operation::PrincipalId,
    pub epoch: crate::operation::FenceEpoch,
    pub claim_hash: crate::operation::ClaimHash,
    pub expires_at: SystemTime,
}

impl DomainClaimObservation {
    fn try_into_claim_for(self, domain: DomainName) -> Result<DomainClaim, DomainFailure> {
        DomainClaim::new(
            domain,
            ClaimGuard::new(
                self.resource,
                self.holder,
                self.epoch,
                self.claim_hash,
                self.expires_at,
            ),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainServingReadiness {
    Active(DomainServingActivation),
    NotActive,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DomainFailure {
    #[error("domain is invalid")]
    InvalidDomain,
    #[error("domain claim was rejected")]
    ClaimRejected,
    #[error("domain claim resource does not match requested domain")]
    ClaimResourceMismatch,
    #[error("domain claim is stale")]
    StaleClaim,
    #[error("certificate is unusable: {0:?}")]
    CertificateUnusable(CertificateUnusableReason),
    #[error("certificate operation failed: {0}")]
    CertificateFailed(CertificateFailure),
    #[error("serving activation failed")]
    ServingActivationFailed,
    #[error("serving operation failed: {0}")]
    ServingFailed(ServingFailure),
    #[error("domain status is unavailable")]
    StatusUnavailable,
    #[error("domain readiness is unknown")]
    UnknownReadiness,
}

pub trait DomainClaimPort {
    fn claim_domain(
        &self,
        context: &MutationContext,
        resource: TypedResourceId<DomainResource>,
        domain: &DomainName,
    ) -> Result<DomainClaimObservation, DomainFailure>;
}

pub trait DomainCertificatePort {
    fn ensure_usable_certificate(
        &self,
        context: &MutationContext,
        claim: &DomainClaim,
        domain: &DomainName,
        policy: CertificatePolicy,
    ) -> Result<UsableDomainCertificate, DomainFailure>;
}

pub trait DomainServingPort {
    fn activate_certificate(
        &self,
        context: &MutationContext,
        claim: &DomainClaim,
        domain: &DomainName,
        certificate: &UsableDomainCertificate,
    ) -> Result<DomainServingActivation, DomainFailure>;

    fn verify_certificate_activation(
        &self,
        context: &MutationContext,
        domain: &DomainName,
        certificate: &UsableDomainCertificate,
        serving_generation: ServingGeneration,
    ) -> Result<DomainServingReadiness, DomainFailure>;
}

pub trait DomainStatusPort {
    fn status(&self, domain: &DomainName) -> Result<DomainStatus, DomainFailure>;

    fn record_pending(
        &self,
        context: &MutationContext,
        domain: &DomainName,
        reason: DomainPendingReason,
    ) -> Result<(), DomainFailure>;

    fn record_ready(
        &self,
        context: &MutationContext,
        ready: DomainReadyRecord,
    ) -> Result<(), DomainFailure>;

    fn record_failed(
        &self,
        context: &MutationContext,
        domain: &DomainName,
        failure: DomainFailure,
    ) -> Result<(), DomainFailure>;
}

pub trait DomainReadinessPort {
    fn ensure_ready(
        &self,
        context: &MutationContext,
        request: DomainAdd,
    ) -> Result<DomainReady, DomainFailure>;
}

pub struct DomainReadinessService<C, T, S, R> {
    claims: C,
    certificates: T,
    serving: S,
    records: R,
}

impl<C, T, S, R> DomainReadinessService<C, T, S, R> {
    #[must_use]
    pub fn new(claims: C, certificates: T, serving: S, records: R) -> Self {
        Self {
            claims,
            certificates,
            serving,
            records,
        }
    }
}

impl<C, T, S, R> DomainReadinessPort for DomainReadinessService<C, T, S, R>
where
    C: DomainClaimPort,
    T: DomainCertificatePort,
    S: DomainServingPort,
    R: DomainStatusPort,
{
    fn ensure_ready(
        &self,
        context: &MutationContext,
        request: DomainAdd,
    ) -> Result<DomainReady, DomainFailure> {
        self.ensure_ready(context, request)
    }
}

impl<C, T, S, R> DomainReadinessService<C, T, S, R>
where
    C: DomainClaimPort,
    T: DomainCertificatePort,
    S: DomainServingPort,
    R: DomainStatusPort,
{
    pub fn ensure_ready(
        &self,
        context: &MutationContext,
        request: DomainAdd,
    ) -> Result<DomainReady, DomainFailure> {
        let domain = request.domain.clone();
        let result = self.try_ensure_ready(context, request);
        if let Err(failure) = &result {
            self.records
                .record_failed(context, &domain, failure.clone())?;
        }
        result
    }

    fn try_ensure_ready(
        &self,
        context: &MutationContext,
        request: DomainAdd,
    ) -> Result<DomainReady, DomainFailure> {
        if let DomainStatus::Ready(record) = self.records.status(&request.domain)?
            && let Some(ready) = self.try_upgrade_ready_record(context, &record, &request)?
        {
            return Ok(ready);
        }

        self.records.record_pending(
            context,
            &request.domain,
            DomainPendingReason::CertificateIssuance,
        )?;

        let domain = request.domain;
        let certificate_policy = request.certificate_policy;
        let claim_observation =
            self.claims
                .claim_domain(context, domain_resource(&domain)?, &domain)?;
        let claim = claim_observation.try_into_claim_for(domain.clone())?;
        let certificate = self.certificates.ensure_usable_certificate(
            context,
            &claim,
            claim.domain(),
            certificate_policy,
        )?;

        self.records.record_pending(
            context,
            claim.domain(),
            DomainPendingReason::ServingActivation,
        )?;
        let serving =
            self.serving
                .activate_certificate(context, &claim, claim.domain(), &certificate)?;
        let ready = DomainReady::new(domain, certificate, serving);

        self.records.record_ready(context, ready.record())?;
        Ok(ready)
    }

    fn try_upgrade_ready_record(
        &self,
        context: &MutationContext,
        record: &DomainReadyRecord,
        request: &DomainAdd,
    ) -> Result<Option<DomainReady>, DomainFailure> {
        if record.domain() != &request.domain {
            return Ok(None);
        }
        let Ok(certificate) = UsableDomainCertificate::new(
            &request.domain,
            record.certificate().clone(),
            request.certificate_policy,
        ) else {
            return Ok(None);
        };
        match self.serving.verify_certificate_activation(
            context,
            &request.domain,
            &certificate,
            record.serving_generation(),
        )? {
            DomainServingReadiness::Active(serving) => Ok(Some(DomainReady::new(
                request.domain.clone(),
                certificate,
                serving,
            ))),
            DomainServingReadiness::NotActive => Ok(None),
        }
    }
}

fn domain_resource(domain: &DomainName) -> Result<TypedResourceId<DomainResource>, DomainFailure> {
    TypedResourceId::parse(format!("domain:{}", domain.as_str()))
        .map_err(|_| DomainFailure::InvalidDomain)
}

fn is_valid_domain_name(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 || value.starts_with('.') || value.ends_with('.') {
        return false;
    }

    value.split('.').all(is_valid_domain_label)
}

fn is_valid_domain_label(label: &str) -> bool {
    if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
        return false;
    }

    label
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acme::{CertificateActivation, CertificateMaterialState, RevocationFreshness};
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, FenceEpoch, IdempotencyKey, OperationId, PrincipalId,
        ScopeId,
    };
    use crate::serving::ServingGeneration;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn empty_or_invalid_domain_is_rejected() {
        for value in [
            "",
            " ",
            ".example.com",
            "example.com.",
            "-bad.example.com",
            "bad_.com",
        ] {
            assert_eq!(DomainName::parse(value), Err(DomainFailure::InvalidDomain));
        }
    }

    #[test]
    fn unusable_certificate_cannot_be_wrapped_as_domain_certificate() {
        let mut certificate = certificate();
        certificate.not_after = UNIX_EPOCH + Duration::from_secs(30);

        assert_eq!(
            UsableDomainCertificate::new(
                &domain(),
                certificate,
                CertificatePolicy {
                    minimum_valid_until: UNIX_EPOCH + Duration::from_secs(3_600),
                },
            ),
            Err(DomainFailure::CertificateUnusable(
                CertificateUnusableReason::SafetyWindowTooShort
            ))
        );
    }

    #[test]
    fn certificate_unusable_does_not_record_ready() {
        let records = FakeRecords::default();
        let mut short_lived = certificate();
        short_lived.not_after = UNIX_EPOCH + Duration::from_secs(30);
        let domains = DomainReadinessService::new(
            FakeClaims,
            FakeCertificates {
                certificate: short_lived,
            },
            FakeServing::success(),
            records.clone(),
        );

        assert_eq!(
            domains.ensure_ready(&context(), add()),
            Err(DomainFailure::CertificateUnusable(
                CertificateUnusableReason::SafetyWindowTooShort
            ))
        );
        assert!(
            records
                .recorded
                .borrow()
                .iter()
                .all(|status| !matches!(status, DomainStatus::Ready(_)))
        );
    }

    #[test]
    fn existing_ready_status_is_reused_when_still_usable() {
        let ready = DomainReady::new(
            domain(),
            UsableDomainCertificate::new(
                &domain(),
                certificate(),
                CertificatePolicy {
                    minimum_valid_until: UNIX_EPOCH + Duration::from_secs(3_600),
                },
            )
            .expect("usable certificate"),
            DomainServingActivation::active(ServingGeneration::new(7)),
        );
        let records = FakeRecords::with_status(DomainStatus::Ready(ready.record()));
        let claims = CountingClaims::default();
        let domains = DomainReadinessService::new(
            claims.clone(),
            FakeCertificates {
                certificate: certificate(),
            },
            FakeServing::success(),
            records.clone(),
        );

        assert_eq!(domains.ensure_ready(&context(), add()), Ok(ready));
        assert!(records.recorded.borrow().is_empty());
        assert_eq!(*claims.count.borrow(), 0);
    }

    #[test]
    fn existing_ready_status_requires_serving_verification() {
        let ready = DomainReady::new(
            domain(),
            UsableDomainCertificate::new(
                &domain(),
                certificate(),
                CertificatePolicy {
                    minimum_valid_until: UNIX_EPOCH + Duration::from_secs(3_600),
                },
            )
            .expect("usable certificate"),
            DomainServingActivation::active(ServingGeneration::new(7)),
        );
        let records = FakeRecords::with_status(DomainStatus::Ready(ready.record()));
        let domains = DomainReadinessService::new(
            CountingClaims::default(),
            FakeCertificates {
                certificate: certificate(),
            },
            FakeServing {
                outcome: Ok(DomainServingActivation::active(ServingGeneration::new(7))),
                verification: Err(DomainFailure::ServingActivationFailed),
            },
            records,
        );

        assert_eq!(
            domains.ensure_ready(&context(), add()),
            Err(DomainFailure::ServingActivationFailed)
        );
    }

    #[test]
    fn inactive_stored_ready_status_takes_fresh_activation_path() {
        let ready = DomainReady::new(
            domain(),
            UsableDomainCertificate::new(
                &domain(),
                certificate(),
                CertificatePolicy {
                    minimum_valid_until: UNIX_EPOCH + Duration::from_secs(3_600),
                },
            )
            .expect("usable certificate"),
            DomainServingActivation::active(ServingGeneration::new(7)),
        );
        let records = FakeRecords::with_status(DomainStatus::Ready(ready.record()));
        let claims = CountingClaims::default();
        let domains = DomainReadinessService::new(
            claims.clone(),
            FakeCertificates {
                certificate: certificate(),
            },
            FakeServing {
                outcome: Ok(DomainServingActivation::active(ServingGeneration::new(9))),
                verification: Ok(DomainServingReadiness::NotActive),
            },
            records,
        );

        let refreshed = domains
            .ensure_ready(&context(), add())
            .expect("fresh readiness");

        assert_eq!(
            refreshed.serving().checkpoint().generation(),
            ServingGeneration::new(9)
        );
        assert_eq!(*claims.count.borrow(), 1);
    }

    #[test]
    fn stale_stored_ready_certificate_takes_fresh_certificate_path() {
        let mut stale_certificate = certificate();
        stale_certificate.not_after = UNIX_EPOCH + Duration::from_secs(30);
        let ready = DomainReadyRecord {
            domain: domain(),
            certificate: stale_certificate,
            serving_generation: ServingGeneration::new(7),
        };
        let records = FakeRecords::with_status(DomainStatus::Ready(ready));
        let claims = CountingClaims::default();
        let domains = DomainReadinessService::new(
            claims.clone(),
            FakeCertificates {
                certificate: certificate(),
            },
            FakeServing::success(),
            records,
        );

        let refreshed = domains
            .ensure_ready(&context(), add())
            .expect("fresh readiness");

        assert_eq!(
            refreshed.serving().checkpoint().generation(),
            ServingGeneration::new(7)
        );
        assert_eq!(*claims.count.borrow(), 1);
    }

    #[test]
    fn mismatched_claim_resource_is_rejected_before_certificate() {
        let records = FakeRecords::default();
        let domains = DomainReadinessService::new(
            MismatchedClaims,
            FakeCertificates {
                certificate: certificate(),
            },
            FakeServing::success(),
            records,
        );

        assert_eq!(
            domains.ensure_ready(&context(), add()),
            Err(DomainFailure::ClaimResourceMismatch)
        );
    }

    #[test]
    fn serving_activation_failure_does_not_record_ready() {
        let records = FakeRecords::default();
        let domains = DomainReadinessService::new(
            FakeClaims,
            FakeCertificates {
                certificate: certificate(),
            },
            FakeServing {
                outcome: Err(DomainFailure::ServingActivationFailed),
                verification: Ok(DomainServingReadiness::Active(
                    DomainServingActivation::active(ServingGeneration::new(7)),
                )),
            },
            records.clone(),
        );

        assert_eq!(
            domains.ensure_ready(&context(), add()),
            Err(DomainFailure::ServingActivationFailed)
        );
        assert!(
            records
                .recorded
                .borrow()
                .iter()
                .all(|status| !matches!(status, DomainStatus::Ready(_)))
        );
    }

    #[test]
    fn success_records_ready_without_terminalizing_operation() {
        let records = FakeRecords::default();
        let domains = DomainReadinessService::new(
            FakeClaims,
            FakeCertificates {
                certificate: certificate(),
            },
            FakeServing::success(),
            records.clone(),
        );

        let ready = domains.ensure_ready(&context(), add()).expect("ready");

        assert_eq!(ready.domain().as_str(), "app.example.com");
        assert!(
            records
                .recorded
                .borrow()
                .iter()
                .any(|status| matches!(status, DomainStatus::Ready(_)))
        );
    }

    fn add() -> DomainAdd {
        DomainAdd {
            domain: domain(),
            certificate_policy: CertificatePolicy {
                minimum_valid_until: UNIX_EPOCH + Duration::from_secs(3_600),
            },
        }
    }

    fn context() -> MutationContext {
        MutationContext::new(
            OperationId::parse("domain-1").expect("operation"),
            IdempotencyKey::parse("domain-1").expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn domain() -> DomainName {
        DomainName::parse("app.example.com").expect("domain")
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

    fn guard(resource: TypedResourceId<DomainResource>) -> ClaimGuard<DomainResource> {
        ClaimGuard::new(
            resource,
            PrincipalId::parse("node-a").expect("holder"),
            FenceEpoch::new(1).expect("fence epoch"),
            crate::operation::ClaimHash::parse("claim-hash-a").expect("claim hash"),
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn claim_observation(resource: TypedResourceId<DomainResource>) -> DomainClaimObservation {
        let guard = guard(resource.clone());
        DomainClaimObservation {
            resource,
            holder: guard.holder().clone(),
            epoch: guard.epoch(),
            claim_hash: guard.claim_hash().clone(),
            expires_at: guard.expires_at(),
        }
    }

    #[derive(Clone, Copy)]
    struct FakeClaims;

    impl DomainClaimPort for FakeClaims {
        fn claim_domain(
            &self,
            _context: &MutationContext,
            resource: TypedResourceId<DomainResource>,
            _domain: &DomainName,
        ) -> Result<DomainClaimObservation, DomainFailure> {
            Ok(claim_observation(resource))
        }
    }

    #[derive(Clone, Copy)]
    struct MismatchedClaims;

    impl DomainClaimPort for MismatchedClaims {
        fn claim_domain(
            &self,
            _context: &MutationContext,
            _resource: TypedResourceId<DomainResource>,
            _domain: &DomainName,
        ) -> Result<DomainClaimObservation, DomainFailure> {
            Ok(claim_observation(
                TypedResourceId::parse("domain:other.example.com").expect("resource"),
            ))
        }
    }

    #[derive(Clone, Default)]
    struct CountingClaims {
        count: Rc<RefCell<usize>>,
    }

    impl DomainClaimPort for CountingClaims {
        fn claim_domain(
            &self,
            _context: &MutationContext,
            resource: TypedResourceId<DomainResource>,
            _domain: &DomainName,
        ) -> Result<DomainClaimObservation, DomainFailure> {
            *self.count.borrow_mut() += 1;
            Ok(claim_observation(resource))
        }
    }

    #[derive(Clone)]
    struct FakeCertificates {
        certificate: CertificateUsability,
    }

    impl DomainCertificatePort for FakeCertificates {
        fn ensure_usable_certificate(
            &self,
            _context: &MutationContext,
            _claim: &DomainClaim,
            domain: &DomainName,
            policy: CertificatePolicy,
        ) -> Result<UsableDomainCertificate, DomainFailure> {
            UsableDomainCertificate::new(domain, self.certificate.clone(), policy)
        }
    }

    #[derive(Clone)]
    struct FakeServing {
        outcome: Result<DomainServingActivation, DomainFailure>,
        verification: Result<DomainServingReadiness, DomainFailure>,
    }

    impl FakeServing {
        fn success() -> Self {
            Self {
                outcome: Ok(DomainServingActivation::active(ServingGeneration::new(7))),
                verification: Ok(DomainServingReadiness::Active(
                    DomainServingActivation::active(ServingGeneration::new(7)),
                )),
            }
        }
    }

    impl DomainServingPort for FakeServing {
        fn activate_certificate(
            &self,
            _context: &MutationContext,
            _claim: &DomainClaim,
            _domain: &DomainName,
            _certificate: &UsableDomainCertificate,
        ) -> Result<DomainServingActivation, DomainFailure> {
            self.outcome.clone()
        }

        fn verify_certificate_activation(
            &self,
            _context: &MutationContext,
            _domain: &DomainName,
            _certificate: &UsableDomainCertificate,
            _serving_generation: ServingGeneration,
        ) -> Result<DomainServingReadiness, DomainFailure> {
            self.verification.clone()
        }
    }

    #[derive(Clone, Default)]
    struct FakeRecords {
        status: Rc<RefCell<DomainStatus>>,
        recorded: Rc<RefCell<Vec<DomainStatus>>>,
    }

    impl FakeRecords {
        fn with_status(status: DomainStatus) -> Self {
            Self {
                status: Rc::new(RefCell::new(status)),
                recorded: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl DomainStatusPort for FakeRecords {
        fn status(&self, _domain: &DomainName) -> Result<DomainStatus, DomainFailure> {
            Ok(self.status.borrow().clone())
        }

        fn record_pending(
            &self,
            _context: &MutationContext,
            _domain: &DomainName,
            reason: DomainPendingReason,
        ) -> Result<(), DomainFailure> {
            self.recorded
                .borrow_mut()
                .push(DomainStatus::Pending(reason));
            Ok(())
        }

        fn record_ready(
            &self,
            _context: &MutationContext,
            ready: DomainReadyRecord,
        ) -> Result<(), DomainFailure> {
            self.recorded.borrow_mut().push(DomainStatus::Ready(ready));
            Ok(())
        }

        fn record_failed(
            &self,
            _context: &MutationContext,
            _domain: &DomainName,
            failure: DomainFailure,
        ) -> Result<(), DomainFailure> {
            self.recorded
                .borrow_mut()
                .push(DomainStatus::Failed(failure));
            Ok(())
        }
    }
}

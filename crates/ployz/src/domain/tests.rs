use super::*;
use crate::acme::{CertificateActivation, CertificateMaterialState, RevocationFreshness};
use crate::operation::{
    AuthorityContext, AuthorityEpoch, FenceEpoch, IdempotencyKey, OperationId, PrincipalId, ScopeId,
};
use crate::serving::ServingGeneration;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

#[tokio::test(flavor = "current_thread")]
async fn empty_or_invalid_domain_is_rejected() {
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

#[tokio::test(flavor = "current_thread")]
async fn unusable_certificate_cannot_be_wrapped_as_domain_certificate() {
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

#[tokio::test(flavor = "current_thread")]
async fn certificate_unusable_does_not_record_ready() {
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
        domains.ensure_ready(&context(), add()).await,
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

#[tokio::test(flavor = "current_thread")]
async fn existing_ready_status_is_reused_when_still_usable() {
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

    assert_eq!(
        domains.ensure_ready(&context(), add()).await,
        Ok(DomainReadinessOutcome::Ready(ready))
    );
    assert!(records.recorded.borrow().is_empty());
    assert_eq!(*claims.count.borrow(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn verify_ready_reuses_existing_ready_without_fresh_mutations() {
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

    assert_eq!(domains.verify_ready(&context(), add()).await, Ok(ready));
    assert!(records.recorded.borrow().is_empty());
    assert_eq!(*claims.count.borrow(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn verify_ready_rechecks_current_certificate_before_reusing_record() {
    let stored_ready = DomainReady::new(
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
    let mut revoked_certificate = certificate();
    revoked_certificate.revocation = RevocationFreshness::KnownRevoked;
    let records = FakeRecords::with_status(DomainStatus::Ready(stored_ready.record()));
    let claims = CountingClaims::default();
    let domains = DomainReadinessService::new(
        claims.clone(),
        FakeCertificates {
            certificate: revoked_certificate,
        },
        FakeServing::success(),
        records.clone(),
    );

    assert_eq!(
        domains.verify_ready(&context(), add()).await,
        Err(DomainFailure::UnknownReadiness)
    );
    assert!(records.recorded.borrow().is_empty());
    assert_eq!(*claims.count.borrow(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn stored_ready_with_stale_current_certificate_takes_fresh_path() {
    let stored_ready = DomainReady::new(
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
    let mut current_certificate = certificate();
    current_certificate.revocation = RevocationFreshness::KnownRevoked;
    let records = FakeRecords::with_status(DomainStatus::Ready(stored_ready.record()));
    let claims = CountingClaims::default();
    let domains = DomainReadinessService::new(
        claims.clone(),
        RefreshingCertificates {
            observed: current_certificate,
            issued: certificate(),
        },
        FakeServing::success(),
        records,
    );

    let refreshed = expect_ready(
        domains
            .ensure_ready(&context(), add())
            .await
            .expect("fresh readiness"),
    );

    assert_eq!(
        refreshed.serving().checkpoint().generation(),
        ServingGeneration::new(7)
    );
    assert_eq!(*claims.count.borrow(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn row_backed_ready_status_is_reused_without_fresh_mutations() {
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
    let records = crate::composition::in_memory_domain_status();
    records
        .record_ready(&context(), ready.record())
        .await
        .expect("ready status");
    let claims = CountingClaims::default();
    let domains = DomainReadinessService::new(
        claims.clone(),
        FakeCertificates {
            certificate: certificate(),
        },
        FakeServing::success(),
        records,
    );

    assert_eq!(
        domains.ensure_ready(&context(), add()).await,
        Ok(DomainReadinessOutcome::Ready(ready))
    );
    assert_eq!(*claims.count.borrow(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn verify_ready_does_not_take_fresh_readiness_path() {
    let records = FakeRecords::default();
    let claims = CountingClaims::default();
    let domains = DomainReadinessService::new(
        claims.clone(),
        FakeCertificates {
            certificate: certificate(),
        },
        FakeServing::success(),
        records.clone(),
    );

    assert_eq!(
        domains.verify_ready(&context(), add()).await,
        Err(DomainFailure::UnknownReadiness)
    );
    assert!(records.recorded.borrow().is_empty());
    assert_eq!(*claims.count.borrow(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn existing_ready_status_requires_serving_verification() {
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
        domains.ensure_ready(&context(), add()).await,
        Err(DomainFailure::ServingActivationFailed)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn inactive_stored_ready_status_takes_fresh_activation_path() {
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

    let refreshed = expect_ready(
        domains
            .ensure_ready(&context(), add())
            .await
            .expect("fresh readiness"),
    );

    assert_eq!(
        refreshed.serving().checkpoint().generation(),
        ServingGeneration::new(9)
    );
    assert_eq!(*claims.count.borrow(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn stale_stored_ready_certificate_reuses_current_certificate_observation() {
    let mut stale_certificate = certificate();
    stale_certificate.not_after = UNIX_EPOCH + Duration::from_secs(30);
    let ready = DomainReadyRecord::new(domain(), stale_certificate, ServingGeneration::new(7))
        .expect("stored ready record");
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

    let refreshed = expect_ready(
        domains
            .ensure_ready(&context(), add())
            .await
            .expect("fresh readiness"),
    );

    assert_eq!(
        refreshed.serving().checkpoint().generation(),
        ServingGeneration::new(7)
    );
    assert_eq!(refreshed.certificate().certificate(), &certificate());
    assert_eq!(*claims.count.borrow(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn ready_record_rejects_mismatched_certificate_hostname() {
    let result = DomainReadyRecord::new(
        domain(),
        certificate_for("other.example.com"),
        ServingGeneration::new(7),
    );

    assert_eq!(
        result,
        Err(DomainFailure::CertificateUnusable(
            CertificateUnusableReason::HostnameMismatch
        ))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn domain_claim_exposes_submitted_fence_capability() {
    let claim = claim(domain_resource(&domain()).expect("resource"), domain());
    let fence = claim.submitted_fence();

    assert_eq!(fence.resource.as_str(), "domain:app.example.com");
    assert_eq!(fence.holder.as_str(), "node-a");
    assert_eq!(fence.epoch.value(), 1);
    assert_eq!(fence.claim_hash.as_str(), "claim-hash-a");
}

#[tokio::test(flavor = "current_thread")]
async fn mismatched_claim_resource_is_rejected_before_certificate() {
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
        domains.ensure_ready(&context(), add()).await,
        Err(DomainFailure::ClaimResourceMismatch)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn serving_activation_failure_does_not_record_ready() {
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
        domains.ensure_ready(&context(), add()).await,
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

#[tokio::test(flavor = "current_thread")]
async fn failed_status_write_preserves_primary_failure() {
    let records = FakeRecords::failing_record_failed(DomainFailure::StatusUnavailable);
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
        records,
    );

    let error = domains
        .ensure_ready(&context(), add())
        .await
        .expect_err("status write should be attached to primary failure");

    assert_eq!(error.primary(), &DomainFailure::ServingActivationFailed);
    assert!(matches!(
        error,
        DomainFailure::FailureRecordUnavailable { .. }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn certificate_in_progress_records_pending_without_failed_status() {
    let records = FakeRecords::default();
    let domains = DomainReadinessService::new(
        FakeClaims,
        PendingCertificates {
            reason: DomainPendingReason::CertificateIssuance,
        },
        FakeServing::success(),
        records.clone(),
    );

    assert_eq!(
        domains.ensure_ready(&context(), add()).await,
        Ok(DomainReadinessOutcome::Pending(
            DomainPendingReason::CertificateIssuance
        ))
    );
    assert!(records.recorded.borrow().iter().any(|status| matches!(
        status,
        DomainStatus::Pending(DomainPendingReason::CertificateIssuance)
    )));
    assert!(
        records
            .recorded
            .borrow()
            .iter()
            .all(|status| !matches!(status, DomainStatus::Failed(_) | DomainStatus::Ready(_)))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn certificate_interrupted_records_interrupted_pending_without_failed_status() {
    let records = FakeRecords::default();
    let domains = DomainReadinessService::new(
        FakeClaims,
        PendingCertificates {
            reason: DomainPendingReason::CertificateIssuanceInterrupted,
        },
        FakeServing::success(),
        records.clone(),
    );

    assert_eq!(
        domains.ensure_ready(&context(), add()).await,
        Ok(DomainReadinessOutcome::Pending(
            DomainPendingReason::CertificateIssuanceInterrupted
        ))
    );
    assert!(records.recorded.borrow().iter().any(|status| matches!(
        status,
        DomainStatus::Pending(DomainPendingReason::CertificateIssuanceInterrupted)
    )));
    assert!(
        records
            .recorded
            .borrow()
            .iter()
            .all(|status| !matches!(status, DomainStatus::Failed(_) | DomainStatus::Ready(_)))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn success_records_ready_without_terminalizing_operation() {
    let records = FakeRecords::default();
    let domains = DomainReadinessService::new(
        FakeClaims,
        FakeCertificates {
            certificate: certificate(),
        },
        FakeServing::success(),
        records.clone(),
    );

    let ready = expect_ready(
        domains
            .ensure_ready(&context(), add())
            .await
            .expect("ready"),
    );

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
    certificate_for("app.example.com")
}

fn certificate_for(hostname: &str) -> CertificateUsability {
    CertificateUsability {
        hostname: Hostname::parse(hostname).expect("hostname"),
        not_after: UNIX_EPOCH + Duration::from_secs(7_200),
        activation: CertificateActivation::Acknowledged,
        material: CertificateMaterialState::PresentProtected,
        revocation: RevocationFreshness::KnownFresh,
    }
}

fn expect_ready(outcome: DomainReadinessOutcome) -> DomainReady {
    let DomainReadinessOutcome::Ready(ready) = outcome else {
        panic!("expected ready outcome");
    };
    ready
}

fn claim(resource: TypedResourceId<DomainResource>, domain: DomainName) -> DomainClaim {
    DomainClaim::from_guard(
        domain,
        ClaimGuard::test_new(
            resource,
            PrincipalId::parse("node-a").expect("holder"),
            FenceEpoch::new(1).expect("fence epoch"),
            crate::operation::ClaimHash::parse("claim-hash-a").expect("claim hash"),
            UNIX_EPOCH + Duration::from_secs(60),
        )
        .expect("claim guard"),
    )
    .expect("domain claim")
}

#[derive(Clone, Copy)]
struct FakeClaims;

impl DomainClaimPort for FakeClaims {
    fn claim_domain(
        &self,
        _context: &MutationContext,
        domain: &DomainName,
    ) -> Result<DomainClaim, DomainFailure> {
        Ok(claim(domain_resource(domain)?, domain.clone()))
    }
}

#[derive(Clone, Copy)]
struct MismatchedClaims;

impl DomainClaimPort for MismatchedClaims {
    fn claim_domain(
        &self,
        _context: &MutationContext,
        domain: &DomainName,
    ) -> Result<DomainClaim, DomainFailure> {
        DomainClaim::from_guard(
            domain.clone(),
            ClaimGuard::test_new(
                TypedResourceId::parse("domain:other.example.com").expect("resource"),
                PrincipalId::parse("node-a").expect("holder"),
                FenceEpoch::new(1).expect("fence epoch"),
                crate::operation::ClaimHash::parse("claim-hash-a").expect("claim hash"),
                UNIX_EPOCH + Duration::from_secs(60),
            )
            .expect("claim guard"),
        )
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
        domain: &DomainName,
    ) -> Result<DomainClaim, DomainFailure> {
        *self.count.borrow_mut() += 1;
        Ok(claim(domain_resource(domain)?, domain.clone()))
    }
}

#[derive(Clone)]
struct FakeCertificates {
    certificate: CertificateUsability,
}

impl DomainCertificatePort for FakeCertificates {
    async fn observe_usable_certificate(
        &self,
        _context: &MutationContext,
        domain: &DomainName,
        policy: CertificatePolicy,
    ) -> Result<Option<UsableDomainCertificate>, DomainFailure> {
        Ok(UsableDomainCertificate::new(domain, self.certificate.clone(), policy).ok())
    }

    async fn ensure_usable_certificate(
        &self,
        _context: &MutationContext,
        claim: &DomainClaim,
        policy: CertificatePolicy,
    ) -> Result<DomainCertificateReadiness, DomainFailure> {
        Ok(DomainCertificateReadiness::Usable(
            UsableDomainCertificate::new(claim.domain(), self.certificate.clone(), policy)?,
        ))
    }
}

#[derive(Clone)]
struct PendingCertificates {
    reason: DomainPendingReason,
}

impl DomainCertificatePort for PendingCertificates {
    async fn observe_usable_certificate(
        &self,
        _context: &MutationContext,
        _domain: &DomainName,
        _policy: CertificatePolicy,
    ) -> Result<Option<UsableDomainCertificate>, DomainFailure> {
        Ok(None)
    }

    async fn ensure_usable_certificate(
        &self,
        _context: &MutationContext,
        _claim: &DomainClaim,
        _policy: CertificatePolicy,
    ) -> Result<DomainCertificateReadiness, DomainFailure> {
        Ok(DomainCertificateReadiness::Pending(self.reason.clone()))
    }
}

#[derive(Clone)]
struct RefreshingCertificates {
    observed: CertificateUsability,
    issued: CertificateUsability,
}

impl DomainCertificatePort for RefreshingCertificates {
    async fn observe_usable_certificate(
        &self,
        _context: &MutationContext,
        domain: &DomainName,
        policy: CertificatePolicy,
    ) -> Result<Option<UsableDomainCertificate>, DomainFailure> {
        Ok(UsableDomainCertificate::new(domain, self.observed.clone(), policy).ok())
    }

    async fn ensure_usable_certificate(
        &self,
        _context: &MutationContext,
        claim: &DomainClaim,
        policy: CertificatePolicy,
    ) -> Result<DomainCertificateReadiness, DomainFailure> {
        Ok(DomainCertificateReadiness::Usable(
            UsableDomainCertificate::new(claim.domain(), self.issued.clone(), policy)?,
        ))
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

#[derive(Clone)]
struct FakeRecords {
    status: Rc<RefCell<DomainStatus>>,
    recorded: Rc<RefCell<Vec<DomainStatus>>>,
    record_failed_error: Rc<RefCell<Option<DomainFailure>>>,
}

impl Default for FakeRecords {
    fn default() -> Self {
        Self::with_status(DomainStatus::Unknown)
    }
}

impl FakeRecords {
    fn with_status(status: DomainStatus) -> Self {
        Self {
            status: Rc::new(RefCell::new(status)),
            recorded: Rc::new(RefCell::new(Vec::new())),
            record_failed_error: Rc::new(RefCell::new(None)),
        }
    }

    fn failing_record_failed(error: DomainFailure) -> Self {
        let records = Self::default();
        *records.record_failed_error.borrow_mut() = Some(error);
        records
    }
}

impl DomainStatusPort for FakeRecords {
    async fn status(
        &self,
        _context: &MutationContext,
        _domain: &DomainName,
    ) -> Result<DomainStatus, DomainFailure> {
        Ok(self.status.borrow().clone())
    }

    async fn record_pending(
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

    async fn record_ready(
        &self,
        _context: &MutationContext,
        ready: DomainReadyRecord,
    ) -> Result<(), DomainFailure> {
        self.recorded.borrow_mut().push(DomainStatus::Ready(ready));
        Ok(())
    }

    async fn record_failed(
        &self,
        _context: &MutationContext,
        _domain: &DomainName,
        failure: DomainStatusFailure,
    ) -> Result<(), DomainFailure> {
        if let Some(error) = self.record_failed_error.borrow_mut().take() {
            return Err(error);
        }
        self.recorded
            .borrow_mut()
            .push(DomainStatus::Failed(failure));
        Ok(())
    }
}

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use ployz::acme::{
    CertificateActivation, CertificateMaterialState, CertificateUnusableReason,
    CertificateUsability, Hostname, RevocationFreshness,
};
use ployz::domain::{
    CertificatePolicy, DomainAdd, DomainCertificatePort, DomainClaim, DomainClaimObservation,
    DomainClaimPort, DomainFailure, DomainName, DomainPendingReason, DomainReadinessService,
    DomainReadyRecord, DomainResource, DomainServingActivation, DomainServingPort,
    DomainServingReadiness, DomainStatus, DomainStatusPort, UsableDomainCertificate,
};
use ployz::operation::{
    AuthorityDecision, AuthorityEpoch, AuthorityPort, ClaimHash, CommandIssuer, FenceEpoch,
    IdempotencyKey, MutationContext, MutationIntent, OperationId, PrincipalId, ScopeId,
    TypedResourceId,
};
use ployz::serving::ServingGeneration;

#[derive(Clone, Default)]
struct FakeClaims {
    claims: Rc<RefCell<Vec<String>>>,
}

impl DomainClaimPort for FakeClaims {
    fn claim_domain(
        &self,
        _context: &MutationContext,
        resource: TypedResourceId<DomainResource>,
        _domain: &DomainName,
    ) -> Result<DomainClaimObservation, DomainFailure> {
        self.claims.borrow_mut().push(resource.as_str().to_owned());
        Ok(DomainClaimObservation {
            resource,
            holder: PrincipalId::parse("node-a").expect("holder"),
            epoch: FenceEpoch::new(1).expect("fence epoch"),
            claim_hash: ClaimHash::parse("claim-hash-a").expect("claim hash"),
            expires_at: UNIX_EPOCH + Duration::from_secs(60),
        })
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
    verifications: Rc<RefCell<usize>>,
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
        serving_generation: ServingGeneration,
    ) -> Result<DomainServingReadiness, DomainFailure> {
        *self.verifications.borrow_mut() += 1;
        Ok(DomainServingReadiness::Active(
            DomainServingActivation::test_active(serving_generation),
        ))
    }
}

#[derive(Clone, Default)]
struct FakeStatus {
    current: Rc<RefCell<DomainStatus>>,
    recorded: Rc<RefCell<Vec<DomainStatus>>>,
}

impl DomainStatusPort for FakeStatus {
    fn status(&self, _domain: &DomainName) -> Result<DomainStatus, DomainFailure> {
        Ok(self.current.borrow().clone())
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
        let status = DomainStatus::Ready(ready);
        *self.current.borrow_mut() = status.clone();
        self.recorded.borrow_mut().push(status);
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

fn context() -> MutationContext {
    CommandIssuer::new(AllowAuthority)
        .issue::<()>(MutationIntent {
            operation: OperationId::parse("domain-add-1").expect("operation"),
            idempotency: IdempotencyKey::parse("domain-add-1").expect("idempotency"),
            principal: PrincipalId::parse("node-a").expect("principal"),
            scope: ScopeId::parse("cluster").expect("scope"),
            command: ployz::operation::CommandKind::parse("domain-add").expect("command"),
            payload_hash: vec![1],
            resources: vec![
                ployz::operation::FingerprintedResource::parse("domain:app.example.com")
                    .expect("domain"),
            ],
            submitted_fence: None,
            deadline: UNIX_EPOCH + Duration::from_secs(60),
        })
        .expect("command")
        .context()
        .clone()
}

struct AllowAuthority;

impl AuthorityPort for AllowAuthority {
    fn decide(
        &self,
        _principal: &PrincipalId,
        _scope: &ScopeId,
    ) -> Result<AuthorityDecision, ployz::PrimitiveFailure> {
        Ok(AuthorityDecision::Allowed(AuthorityEpoch::new(7)))
    }
}

fn request() -> DomainAdd {
    DomainAdd {
        domain: DomainName::parse("app.example.com").expect("domain"),
        certificate_policy: CertificatePolicy {
            minimum_valid_until: UNIX_EPOCH + Duration::from_secs(3_600),
        },
    }
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

fn serving_success() -> FakeServing {
    FakeServing {
        outcome: Ok(DomainServingActivation::test_active(
            ServingGeneration::new(11),
        )),
        verifications: Rc::new(RefCell::new(0)),
    }
}

#[test]
fn domain_add_records_ready_without_generic_terminalization() {
    let status = FakeStatus::default();
    let claims = FakeClaims::default();
    let domains = DomainReadinessService::new(
        claims.clone(),
        FakeCertificates {
            certificate: certificate(),
        },
        serving_success(),
        status.clone(),
    );

    let ready = domains
        .ensure_ready(&context(), request())
        .expect("domain ready");

    assert_eq!(ready.domain().as_str(), "app.example.com");
    assert_eq!(
        claims.claims.borrow().as_slice(),
        ["domain:app.example.com"]
    );
    assert!(
        status
            .recorded
            .borrow()
            .iter()
            .any(|status| matches!(status, DomainStatus::Ready(_)))
    );
}

#[test]
fn domain_add_certificate_safety_window_failure_is_visible() {
    let status = FakeStatus::default();
    let mut short_lived = certificate();
    short_lived.not_after = UNIX_EPOCH + Duration::from_secs(30);
    let domains = DomainReadinessService::new(
        FakeClaims::default(),
        FakeCertificates {
            certificate: short_lived,
        },
        serving_success(),
        status.clone(),
    );

    assert_eq!(
        domains.ensure_ready(&context(), request()),
        Err(DomainFailure::CertificateUnusable(
            CertificateUnusableReason::SafetyWindowTooShort
        ))
    );
    assert!(status.recorded.borrow().iter().any(|status| matches!(
        status,
        DomainStatus::Failed(DomainFailure::CertificateUnusable(
            CertificateUnusableReason::SafetyWindowTooShort
        ))
    )));
    assert!(
        status
            .recorded
            .borrow()
            .iter()
            .all(|status| !matches!(status, DomainStatus::Ready(_)))
    );
}

#[test]
fn domain_add_serving_activation_failure_is_not_ready() {
    let status = FakeStatus::default();
    let domains = DomainReadinessService::new(
        FakeClaims::default(),
        FakeCertificates {
            certificate: certificate(),
        },
        FakeServing {
            outcome: Err(DomainFailure::ServingActivationFailed),
            verifications: Rc::new(RefCell::new(0)),
        },
        status.clone(),
    );

    assert_eq!(
        domains.ensure_ready(&context(), request()),
        Err(DomainFailure::ServingActivationFailed)
    );
    assert!(
        status
            .recorded
            .borrow()
            .iter()
            .all(|status| !matches!(status, DomainStatus::Ready(_)))
    );
}

#[test]
fn domain_add_retry_after_checkpoint_verifies_ready_invariants() {
    let status = FakeStatus::default();
    let claims = FakeClaims::default();
    let serving = serving_success();
    let verifications = serving.verifications.clone();
    let domains = DomainReadinessService::new(
        claims.clone(),
        FakeCertificates {
            certificate: certificate(),
        },
        serving,
        status.clone(),
    );

    let ready = domains
        .ensure_ready(&context(), request())
        .expect("domain ready");

    assert_eq!(ready.domain().as_str(), "app.example.com");
    assert_eq!(claims.claims.borrow().len(), 1);
    assert!(status.recorded.borrow().iter().any(|status| matches!(
        status,
        DomainStatus::Pending(DomainPendingReason::CertificateIssuance)
    )));
    assert!(status.recorded.borrow().iter().any(|status| matches!(
        status,
        DomainStatus::Pending(DomainPendingReason::ServingActivation)
    )));

    let replayed = domains
        .ensure_ready(&context(), request())
        .expect("domain ready replay");

    assert_eq!(replayed.domain().as_str(), "app.example.com");
    assert_eq!(claims.claims.borrow().len(), 1);
    assert_eq!(*verifications.borrow(), 1);
}

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use ployz::acme::{
    CertificateActivation, CertificateMaterialState, CertificateUsability, Hostname, HttpsBinding,
    RevocationFreshness,
};
use ployz::deploy::{DeployEngine, DeployManifest, DeployRequest, certificate_unusable_reason};
use ployz::domain::{
    CertificatePolicy, DomainCertificatePort, DomainClaim, DomainClaimPort, DomainFailure,
    DomainName, DomainPendingReason, DomainReadinessService, DomainReadyRecord,
    DomainServingActivation, DomainServingPort, DomainServingReadiness, DomainStatus,
    DomainStatusPort, UsableDomainCertificate,
};
use ployz::error::{DeployFailure, RuntimeFailure, ServingFailure};
use ployz::machine::MachineId;
use ployz::operation::{
    AuthorityEpoch, ClaimHash, FenceEpoch, IdempotencyKey, MutationContext, OperationId,
    PrincipalId, ScopeId, TypedResourceId,
};
use ployz::runtime::{
    ParticipantReceipt, RuntimeActivationOutcome, RuntimeActivationRequest,
    RuntimeParticipantStatus, RuntimeParticipantVerification, RuntimePort, RuntimeRevision,
    WorkloadId,
};
use ployz::serving::{
    RouteId, ServingActivationObservation, ServingCommitRequest, ServingGeneration, ServingPort,
    ServingTarget,
};

type FakeDomainReadiness =
    DomainReadinessService<FakeDomains, FakeDomains, FakeDomains, FakeDomains>;

#[derive(Clone)]
pub(super) struct FakeDomains {
    certificate: CertificateUsability,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
    status: Rc<RefCell<DomainStatus>>,
}

impl DomainClaimPort for FakeDomains {
    fn claim_domain(
        &self,
        context: &MutationContext,
        domain: &DomainName,
    ) -> Result<DomainClaim, DomainFailure> {
        self.contexts.borrow_mut().push(context.clone());
        DomainClaim::test_new(
            domain.clone(),
            TypedResourceId::parse(format!("domain:{}", domain.as_str())).expect("resource"),
            PrincipalId::parse("node-a").expect("holder"),
            FenceEpoch::new(1).expect("fence epoch"),
            ClaimHash::parse("claim-hash-a").expect("claim hash"),
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }
}

impl DomainCertificatePort for FakeDomains {
    fn ensure_usable_certificate(
        &self,
        _context: &MutationContext,
        claim: &DomainClaim,
        policy: CertificatePolicy,
    ) -> Result<UsableDomainCertificate, DomainFailure> {
        UsableDomainCertificate::new(claim.domain(), self.certificate.clone(), policy)
    }
}

impl DomainServingPort for FakeDomains {
    fn activate_certificate(
        &self,
        _context: &MutationContext,
        _claim: &DomainClaim,
        _certificate: &UsableDomainCertificate,
    ) -> Result<DomainServingActivation, DomainFailure> {
        Ok(DomainServingActivation::test_active(
            ServingGeneration::new(11),
        ))
    }

    fn verify_certificate_activation(
        &self,
        _context: &MutationContext,
        _domain: &DomainName,
        _certificate: &UsableDomainCertificate,
        serving_generation: ServingGeneration,
    ) -> Result<DomainServingReadiness, DomainFailure> {
        Ok(DomainServingReadiness::Active(
            DomainServingActivation::test_active(serving_generation),
        ))
    }
}

impl DomainStatusPort for FakeDomains {
    fn status(&self, _domain: &DomainName) -> Result<DomainStatus, DomainFailure> {
        Ok(self.status.borrow().clone())
    }

    fn record_pending(
        &self,
        _context: &MutationContext,
        _domain: &DomainName,
        reason: DomainPendingReason,
    ) -> Result<(), DomainFailure> {
        *self.status.borrow_mut() = DomainStatus::Pending(reason);
        Ok(())
    }

    fn record_ready(
        &self,
        _context: &MutationContext,
        ready: DomainReadyRecord,
    ) -> Result<(), DomainFailure> {
        *self.status.borrow_mut() = DomainStatus::Ready(ready);
        Ok(())
    }

    fn record_failed(
        &self,
        _context: &MutationContext,
        _domain: &DomainName,
        failure: DomainFailure,
    ) -> Result<(), DomainFailure> {
        *self.status.borrow_mut() = DomainStatus::Failed(failure);
        Ok(())
    }
}

#[derive(Clone)]
pub(super) struct FakeRuntime {
    outcome: RuntimeActivationOutcome,
    status: Rc<RefCell<RuntimeParticipantStatus>>,
    activation_visible: bool,
    pub(super) activations: Rc<RefCell<usize>>,
    pub(super) verifications: Rc<RefCell<usize>>,
}

impl RuntimePort for FakeRuntime {
    fn activate_participant(
        &self,
        request: RuntimeActivationRequest,
    ) -> Result<RuntimeActivationOutcome, RuntimeFailure> {
        *self.activations.borrow_mut() += 1;
        assert_eq!(request.context.authority().epoch(), AuthorityEpoch::new(7));
        let outcome = self.outcome.clone();
        if self.activation_visible
            && let RuntimeActivationOutcome::Activated(receipt) = &outcome
        {
            *self.status.borrow_mut() = RuntimeParticipantStatus::Active(receipt.clone());
        }
        Ok(outcome)
    }

    fn verify_participant(
        &self,
        request: RuntimeParticipantVerification,
    ) -> Result<RuntimeParticipantStatus, RuntimeFailure> {
        *self.verifications.borrow_mut() += 1;
        assert_eq!(request.context.authority().epoch(), AuthorityEpoch::new(7));
        Ok(self.status.borrow().clone())
    }
}

#[derive(Clone)]
pub(super) struct FakeServing {
    activation: ServingActivationObservation,
    pub(super) commits: Rc<RefCell<usize>>,
}

impl ServingPort for FakeServing {
    fn commit_snapshot(
        &self,
        context: &MutationContext,
        _request: &ServingCommitRequest,
    ) -> Result<(), ServingFailure> {
        *self.commits.borrow_mut() += 1;
        assert_eq!(context.authority().epoch(), AuthorityEpoch::new(7));
        Ok(())
    }

    fn activation_status(
        &self,
        _request: &ServingCommitRequest,
    ) -> Result<ServingActivationObservation, ServingFailure> {
        Ok(self.activation.clone())
    }
}

pub(super) fn usable_certificate() -> CertificateUsability {
    CertificateUsability {
        hostname: Hostname::parse("app.example.com").expect("hostname"),
        not_after: UNIX_EPOCH + Duration::from_secs(7_200),
        activation: CertificateActivation::Acknowledged,
        material: CertificateMaterialState::PresentProtected,
        revocation: RevocationFreshness::KnownFresh,
    }
}

pub(super) fn request() -> DeployRequest {
    let hostname = Hostname::parse("app.example.com").expect("hostname");
    DeployRequest {
        manifest: DeployManifest {
            https: HttpsBinding::new(hostname),
            route: RouteId::parse("route-app").expect("route"),
            serving_target: ServingTarget::parse("gateway-a").expect("target"),
            serving_generation: ServingGeneration::new(11),
            workload: WorkloadId::parse("workload-app").expect("workload"),
            machine: MachineId::parse("machine-a").expect("machine"),
            minimum_certificate_valid_until: UNIX_EPOCH + Duration::from_secs(3_600),
        },
        deadline: UNIX_EPOCH + Duration::from_secs(60),
    }
}

pub(super) fn activation_for(request: &DeployRequest) -> ServingActivationObservation {
    ServingActivationObservation::Acknowledged {
        route: request.manifest.route.clone(),
        hostname: request.manifest.https.hostname.clone(),
        target: request.manifest.serving_target.clone(),
        generation: request.manifest.serving_generation,
    }
}

pub(super) fn deploy_context() -> MutationContext {
    MutationContext::test_authorized(
        OperationId::parse("deploy-1").expect("operation"),
        IdempotencyKey::parse("idem-1").expect("idempotency"),
        PrincipalId::parse("node-a").expect("principal"),
        ScopeId::parse("cluster").expect("scope"),
        AuthorityEpoch::new(7),
        None,
        UNIX_EPOCH + Duration::from_secs(60),
    )
}

pub(super) fn engine(
    certificate: CertificateUsability,
    activation: ServingActivationObservation,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
) -> DeployEngine<FakeDomainReadiness, FakeRuntime, FakeServing> {
    engine_with_shared_state(
        certificate,
        activation,
        contexts,
        Rc::new(RefCell::new(DomainStatus::Unknown)),
        runtime_for(receipt()),
        Rc::new(RefCell::new(0)),
    )
}

fn engine_with_runtime(
    certificate: CertificateUsability,
    runtime: RuntimeActivationOutcome,
    activation: ServingActivationObservation,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
) -> DeployEngine<FakeDomainReadiness, FakeRuntime, FakeServing> {
    let runtime = FakeRuntime {
        outcome: runtime,
        status: Rc::new(RefCell::new(RuntimeParticipantStatus::Missing)),
        activation_visible: true,
        activations: Rc::new(RefCell::new(0)),
        verifications: Rc::new(RefCell::new(0)),
    };
    engine_with_shared_state(
        certificate,
        activation,
        contexts,
        Rc::new(RefCell::new(DomainStatus::Unknown)),
        runtime,
        Rc::new(RefCell::new(0)),
    )
}

pub(super) fn engine_with_shared_state(
    certificate: CertificateUsability,
    activation: ServingActivationObservation,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
    status: Rc<RefCell<DomainStatus>>,
    runtime: FakeRuntime,
    serving_commits: Rc<RefCell<usize>>,
) -> DeployEngine<FakeDomainReadiness, FakeRuntime, FakeServing> {
    let domains = FakeDomains {
        certificate,
        contexts,
        status,
    };
    DeployEngine::new(
        DomainReadinessService::new(domains.clone(), domains.clone(), domains.clone(), domains),
        runtime,
        FakeServing {
            activation,
            commits: serving_commits,
        },
    )
}

pub(super) fn runtime_for(receipt: ParticipantReceipt) -> FakeRuntime {
    FakeRuntime {
        outcome: RuntimeActivationOutcome::Activated(receipt.clone()),
        status: Rc::new(RefCell::new(RuntimeParticipantStatus::Missing)),
        activation_visible: true,
        activations: Rc::new(RefCell::new(0)),
        verifications: Rc::new(RefCell::new(0)),
    }
}

pub(super) fn receipt() -> ParticipantReceipt {
    ParticipantReceipt {
        workload: WorkloadId::parse("workload-app").expect("workload"),
        machine: MachineId::parse("machine-a").expect("machine"),
        revision: RuntimeRevision::new(3),
    }
}

#[test]
fn https_deploy_ensures_cert_commits_serving_and_verifies_activation() {
    let contexts = Rc::new(RefCell::new(Vec::new()));
    let deploy = engine(
        usable_certificate(),
        activation_for(&request()),
        contexts.clone(),
    );

    let outcome = deploy
        .deploy_https(&deploy_context(), request())
        .expect("deploy success");

    assert_eq!(outcome.domain.domain().as_str(), "app.example.com");
    assert_eq!(outcome.runtime.revision, RuntimeRevision::new(3));
    assert_eq!(outcome.serving.generation(), ServingGeneration::new(11));
    assert_eq!(contexts.borrow().len(), 1);
}

#[test]
fn certificate_usability_reasons_keep_unknown_distinct() {
    let binding = request().manifest.https;
    let mut cases = Vec::new();
    let mut unknown = usable_certificate();
    unknown.revocation = RevocationFreshness::Unknown;
    cases.push((
        unknown.clone(),
        ployz::acme::CertificateUnusableReason::FreshnessUnknown,
    ));
    let mut revoked = usable_certificate();
    revoked.revocation = RevocationFreshness::KnownRevoked;
    cases.push((
        revoked.clone(),
        ployz::acme::CertificateUnusableReason::KnownRevoked,
    ));
    let mut short_lived = usable_certificate();
    short_lived.not_after = UNIX_EPOCH + Duration::from_secs(30);
    cases.push((
        short_lived.clone(),
        ployz::acme::CertificateUnusableReason::SafetyWindowTooShort,
    ));

    for (certificate, reason) in cases {
        assert_eq!(
            certificate_unusable_reason(
                &certificate,
                &binding,
                UNIX_EPOCH + Duration::from_secs(3_600)
            ),
            Some(reason)
        );
    }

    for certificate in [unknown, revoked, short_lived] {
        let deploy = engine(
            certificate,
            activation_for(&request()),
            Rc::new(RefCell::new(Vec::new())),
        );
        assert_eq!(
            deploy.deploy_https(&deploy_context(), request()),
            Err(DeployFailure::CertificateUnusable)
        );
    }
}

#[test]
fn serving_commit_without_activation_is_not_success() {
    let deploy = engine(
        usable_certificate(),
        ServingActivationObservation::Unknown,
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        deploy.deploy_https(&deploy_context(), request()),
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );
}

#[test]
fn retry_after_missing_serving_activation_observes_current_state() {
    let status = Rc::new(RefCell::new(DomainStatus::Unknown));
    let runtime = runtime_for(receipt());
    let serving_commits = Rc::new(RefCell::new(0));
    let first = engine_with_shared_state(
        usable_certificate(),
        ServingActivationObservation::Unknown,
        Rc::new(RefCell::new(Vec::new())),
        status.clone(),
        runtime.clone(),
        serving_commits.clone(),
    );
    assert_eq!(
        first.deploy_https(&deploy_context(), request()),
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );

    let contexts = Rc::new(RefCell::new(Vec::new()));
    let deploy = engine_with_shared_state(
        usable_certificate(),
        activation_for(&request()),
        contexts.clone(),
        status,
        runtime.clone(),
        serving_commits.clone(),
    );

    let outcome = deploy
        .deploy_https(&deploy_context(), request())
        .expect("retry succeeds from observation");
    assert_eq!(outcome.serving.generation(), ServingGeneration::new(11));
    assert!(contexts.borrow().is_empty());
    assert_eq!(*runtime.activations.borrow(), 1);
    assert_eq!(*serving_commits.borrow(), 1);
}

#[test]
fn second_deploy_observes_current_state_without_mutation() {
    let contexts = Rc::new(RefCell::new(Vec::new()));
    let status = Rc::new(RefCell::new(DomainStatus::Unknown));
    let runtime = runtime_for(receipt());
    let serving_commits = Rc::new(RefCell::new(0));
    let deploy = engine_with_shared_state(
        usable_certificate(),
        activation_for(&request()),
        contexts.clone(),
        status,
        runtime.clone(),
        serving_commits.clone(),
    );

    let _first = deploy
        .deploy_https(&deploy_context(), request())
        .expect("initial deploy");
    contexts.borrow_mut().clear();

    let second = deploy
        .deploy_https(&deploy_context(), request())
        .expect("second deploy");

    assert_eq!(second.runtime.revision, RuntimeRevision::new(3));
    assert!(contexts.borrow().is_empty());
    assert_eq!(*runtime.activations.borrow(), 1);
    assert_eq!(*runtime.verifications.borrow(), 3);
    assert_eq!(*serving_commits.borrow(), 1);
}

#[test]
fn observed_ready_domain_repairs_missing_runtime_participant() {
    let status = Rc::new(RefCell::new(DomainStatus::Unknown));
    let first = engine_with_shared_state(
        usable_certificate(),
        activation_for(&request()),
        Rc::new(RefCell::new(Vec::new())),
        status.clone(),
        runtime_for(receipt()),
        Rc::new(RefCell::new(0)),
    );
    first
        .deploy_https(&deploy_context(), request())
        .expect("initial deploy");
    let activations = Rc::new(RefCell::new(0));

    let retry = engine_with_shared_state(
        usable_certificate(),
        activation_for(&request()),
        Rc::new(RefCell::new(Vec::new())),
        status,
        FakeRuntime {
            outcome: RuntimeActivationOutcome::Activated(receipt()),
            status: Rc::new(RefCell::new(RuntimeParticipantStatus::Missing)),
            activation_visible: true,
            activations: activations.clone(),
            verifications: Rc::new(RefCell::new(0)),
        },
        Rc::new(RefCell::new(0)),
    );

    let outcome = retry
        .deploy_https(&deploy_context(), request())
        .expect("missing runtime is repaired");
    assert_eq!(outcome.runtime.revision, RuntimeRevision::new(3));
    assert_eq!(*activations.borrow(), 1);
}

#[test]
fn runtime_activation_must_be_observed_before_deploy_succeeds() {
    let activations = Rc::new(RefCell::new(0));
    let verifications = Rc::new(RefCell::new(0));
    let serving_commits = Rc::new(RefCell::new(0));
    let deploy = engine_with_shared_state(
        usable_certificate(),
        activation_for(&request()),
        Rc::new(RefCell::new(Vec::new())),
        Rc::new(RefCell::new(DomainStatus::Unknown)),
        FakeRuntime {
            outcome: RuntimeActivationOutcome::Activated(receipt()),
            status: Rc::new(RefCell::new(RuntimeParticipantStatus::Missing)),
            activation_visible: false,
            activations: activations.clone(),
            verifications: verifications.clone(),
        },
        serving_commits.clone(),
    );

    assert_eq!(
        deploy.deploy_https(&deploy_context(), request()),
        Err(DeployFailure::RuntimeParticipantFailed)
    );
    assert_eq!(*activations.borrow(), 1);
    assert_eq!(*verifications.borrow(), 2);
    assert_eq!(*serving_commits.borrow(), 0);
}

#[test]
fn runtime_receipt_must_match_requested_participant() {
    let deploy = engine_with_runtime(
        usable_certificate(),
        RuntimeActivationOutcome::Activated(ParticipantReceipt {
            workload: WorkloadId::parse("other-workload").expect("workload"),
            machine: MachineId::parse("machine-a").expect("machine"),
            revision: RuntimeRevision::new(3),
        }),
        activation_for(&request()),
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        deploy.deploy_https(&deploy_context(), request()),
        Err(DeployFailure::RuntimeParticipantFailed)
    );
}

#[test]
fn serving_activation_must_match_committed_route_identity() {
    let deploy = engine(
        usable_certificate(),
        ServingActivationObservation::Acknowledged {
            route: RouteId::parse("other-route").expect("route"),
            hostname: Hostname::parse("app.example.com").expect("hostname"),
            target: ServingTarget::parse("gateway-a").expect("target"),
            generation: ServingGeneration::new(11),
        },
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        deploy.deploy_https(&deploy_context(), request()),
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );
}

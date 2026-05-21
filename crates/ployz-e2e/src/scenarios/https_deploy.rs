use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use ployz::acme::{
    CertificateActivation, CertificateMaterialState, CertificateUsability, Hostname, HttpsBinding,
    RevocationFreshness,
};
use ployz::deploy::{
    DeployCommand, DeployEngine, DeployManifest, DeployRequest, certificate_unusable_reason,
};
use ployz::domain::{
    CertificatePolicy, DomainCertificatePort, DomainClaim, DomainClaimPort, DomainFailure,
    DomainName, DomainPendingReason, DomainReadinessService, DomainReadyRecord,
    DomainServingActivation, DomainServingPort, DomainServingReadiness, DomainStatus,
    DomainStatusPort, UsableDomainCertificate,
};
use ployz::error::{DeployFailure, PrimitiveFailure, RuntimeFailure, ServingFailure};
use ployz::operation::{
    AuthorityDecision, AuthorityEpoch, AuthorityPort, ClaimHash, CommandEnvelope, CommandIssue,
    CommandIssuer, CommandRunner, FenceEpoch, IdempotencyKey, MutationContext, OperationId,
    PrincipalId, ScopeId, TypedResourceId,
};
use ployz::runtime::{
    MachineId, ParticipantReceipt, RuntimeActivationOutcome, RuntimeActivationRequest,
    RuntimeParticipantStatus, RuntimeParticipantVerification, RuntimePort, RuntimeRevision,
    WorkloadId,
};
use ployz::serving::{
    RouteId, ServingActivationCheckpoint, ServingActivationObservation, ServingCommitRequest,
    ServingGeneration, ServingPort, ServingTarget,
};
use polis::{OperationEvidence, TerminalMarker};

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
    status: RuntimeParticipantStatus,
    activations: Rc<RefCell<usize>>,
    verifications: Rc<RefCell<usize>>,
}

impl RuntimePort for FakeRuntime {
    fn activate_participant(
        &self,
        request: RuntimeActivationRequest,
    ) -> Result<RuntimeActivationOutcome, RuntimeFailure> {
        *self.activations.borrow_mut() += 1;
        assert_eq!(request.context.authority().epoch(), AuthorityEpoch::new(7));
        Ok(self.outcome.clone())
    }

    fn verify_participant(
        &self,
        request: RuntimeParticipantVerification,
    ) -> Result<RuntimeParticipantStatus, RuntimeFailure> {
        *self.verifications.borrow_mut() += 1;
        assert_eq!(request.context.authority().epoch(), AuthorityEpoch::new(7));
        Ok(self.status.clone())
    }
}

#[derive(Clone)]
pub(super) struct FakeServing {
    activation: ServingActivationObservation,
    commits: Rc<RefCell<usize>>,
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
        _checkpoint: &ServingActivationCheckpoint,
    ) -> Result<ServingActivationObservation, ServingFailure> {
        Ok(self.activation.clone())
    }
}

#[derive(Clone, Default)]
pub(super) struct FakeOperations {
    pub(super) evidence: Rc<RefCell<Vec<OperationEvidence>>>,
    pub(super) terminal: Rc<RefCell<Vec<TerminalMarker>>>,
    pub(super) replay: Rc<RefCell<Option<Option<TerminalMarker>>>>,
}

impl polis::OperationBackend for FakeOperations {
    fn start_or_replay(
        &self,
        request: &polis::OperationRequest,
    ) -> polis::Result<polis::BackendOperationStart> {
        if let Some(terminal) = self.replay.borrow().clone() {
            return Ok(polis::BackendOperationStart::Replayed {
                operation: request.operation().clone(),
                terminal,
            });
        }
        Ok(polis::BackendOperationStart::Started)
    }

    fn record(
        &self,
        _operation: &polis::OperationId,
        evidence: polis::OperationEvidence,
    ) -> polis::Result<()> {
        self.evidence.borrow_mut().push(evidence);
        Ok(())
    }

    fn close(
        &self,
        _operation: &polis::OperationId,
        marker: polis::TerminalMarker,
    ) -> polis::Result<()> {
        self.terminal.borrow_mut().push(marker);
        Ok(())
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

pub(super) fn command() -> CommandEnvelope<DeployCommand> {
    DeployCommand::issue(
        &CommandIssuer::new(AllowAuthority),
        CommandIssue {
            operation: OperationId::parse("deploy-1").expect("operation"),
            idempotency: IdempotencyKey::parse("idem-1").expect("idempotency"),
            principal: PrincipalId::parse("node-a").expect("principal"),
            scope: ScopeId::parse("cluster").expect("scope"),
            deadline: UNIX_EPOCH + Duration::from_secs(60),
        },
        &request(),
    )
    .expect("command")
}

struct AllowAuthority;

impl AuthorityPort for AllowAuthority {
    fn decide(
        &self,
        _principal: &PrincipalId,
        _scope: &ScopeId,
    ) -> Result<AuthorityDecision, PrimitiveFailure> {
        Ok(AuthorityDecision::Allowed(AuthorityEpoch::new(7)))
    }
}

pub(super) fn engine(
    certificate: CertificateUsability,
    activation: ServingActivationObservation,
    operations: FakeOperations,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
) -> DeployEngine<FakeDomainReadiness, FakeRuntime, FakeServing, CommandRunner<FakeOperations>> {
    engine_with_shared_state(
        certificate,
        activation,
        operations,
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
    operations: FakeOperations,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
) -> DeployEngine<FakeDomainReadiness, FakeRuntime, FakeServing, CommandRunner<FakeOperations>> {
    let runtime = FakeRuntime {
        outcome: runtime,
        status: RuntimeParticipantStatus::Active(receipt()),
        activations: Rc::new(RefCell::new(0)),
        verifications: Rc::new(RefCell::new(0)),
    };
    engine_with_shared_state(
        certificate,
        activation,
        operations,
        contexts,
        Rc::new(RefCell::new(DomainStatus::Unknown)),
        runtime,
        Rc::new(RefCell::new(0)),
    )
}

fn engine_with_shared_state(
    certificate: CertificateUsability,
    activation: ServingActivationObservation,
    operations: FakeOperations,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
    status: Rc<RefCell<DomainStatus>>,
    runtime: FakeRuntime,
    serving_commits: Rc<RefCell<usize>>,
) -> DeployEngine<FakeDomainReadiness, FakeRuntime, FakeServing, CommandRunner<FakeOperations>> {
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
        CommandRunner::new(operations),
    )
}

fn runtime_for(receipt: ParticipantReceipt) -> FakeRuntime {
    FakeRuntime {
        outcome: RuntimeActivationOutcome::Activated(receipt.clone()),
        status: RuntimeParticipantStatus::Active(receipt),
        activations: Rc::new(RefCell::new(0)),
        verifications: Rc::new(RefCell::new(0)),
    }
}

fn receipt() -> ParticipantReceipt {
    ParticipantReceipt {
        workload: WorkloadId::parse("workload-app").expect("workload"),
        machine: MachineId::parse("machine-a").expect("machine"),
        revision: RuntimeRevision::new(3),
    }
}

#[test]
fn https_deploy_ensures_cert_commits_serving_and_verifies_activation() {
    let operations = FakeOperations::default();
    let contexts = Rc::new(RefCell::new(Vec::new()));
    let deploy = engine(
        usable_certificate(),
        activation_for(&request()),
        operations.clone(),
        contexts.clone(),
    );

    let outcome = deploy
        .deploy_https(command(), request())
        .expect("deploy success");

    assert_eq!(outcome.domain.domain().as_str(), "app.example.com");
    assert_eq!(outcome.runtime.revision, RuntimeRevision::new(3));
    assert_eq!(outcome.serving.generation(), ServingGeneration::new(11));
    assert_eq!(contexts.borrow().len(), 1);
    assert!(matches!(
        operations.terminal.borrow().as_slice(),
        [TerminalMarker::Succeeded]
    ));
    assert!(operations.evidence.borrow().is_empty());
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
            FakeOperations::default(),
            Rc::new(RefCell::new(Vec::new())),
        );
        assert_eq!(
            deploy.deploy_https(command(), request()),
            Err(DeployFailure::CertificateUnusable)
        );
    }
}

#[test]
fn serving_commit_without_activation_is_not_success() {
    let operations = FakeOperations::default();
    let deploy = engine(
        usable_certificate(),
        ServingActivationObservation::Unknown,
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        deploy.deploy_https(command(), request()),
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );
    assert_eq!(
        operations.terminal.borrow().as_slice(),
        [TerminalMarker::Failed(Vec::new())]
    );
}

#[test]
fn terminal_success_replay_verifies_deploy_without_mutation() {
    let operations = FakeOperations::default();
    let contexts = Rc::new(RefCell::new(Vec::new()));
    let status = Rc::new(RefCell::new(DomainStatus::Unknown));
    let runtime = runtime_for(receipt());
    let serving_commits = Rc::new(RefCell::new(0));
    let deploy = engine_with_shared_state(
        usable_certificate(),
        activation_for(&request()),
        operations.clone(),
        contexts.clone(),
        status,
        runtime.clone(),
        serving_commits.clone(),
    );

    let _first = deploy
        .deploy_https(command(), request())
        .expect("initial deploy");
    *operations.replay.borrow_mut() = Some(Some(TerminalMarker::Succeeded));
    contexts.borrow_mut().clear();

    let replayed = deploy
        .deploy_https(command(), request())
        .expect("replayed deploy");

    assert_eq!(replayed.runtime.revision, RuntimeRevision::new(3));
    assert!(contexts.borrow().is_empty());
    assert_eq!(*runtime.activations.borrow(), 1);
    assert_eq!(*runtime.verifications.borrow(), 1);
    assert_eq!(*serving_commits.borrow(), 1);
    assert_eq!(
        operations.terminal.borrow().as_slice(),
        [TerminalMarker::Succeeded]
    );
}

#[test]
fn terminal_success_replay_rejects_missing_runtime_participant() {
    let operations = FakeOperations::default();
    let status = Rc::new(RefCell::new(DomainStatus::Unknown));
    let first = engine_with_shared_state(
        usable_certificate(),
        activation_for(&request()),
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
        status.clone(),
        runtime_for(receipt()),
        Rc::new(RefCell::new(0)),
    );
    first
        .deploy_https(command(), request())
        .expect("initial deploy");
    *operations.replay.borrow_mut() = Some(Some(TerminalMarker::Succeeded));

    let replay = engine_with_shared_state(
        usable_certificate(),
        activation_for(&request()),
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
        status,
        FakeRuntime {
            outcome: RuntimeActivationOutcome::Activated(receipt()),
            status: RuntimeParticipantStatus::Missing,
            activations: Rc::new(RefCell::new(0)),
            verifications: Rc::new(RefCell::new(0)),
        },
        Rc::new(RefCell::new(0)),
    );

    assert_eq!(
        replay.deploy_https(command(), request()),
        Err(DeployFailure::RuntimeParticipantFailed)
    );
}

#[test]
fn operation_evidence_does_not_render_private_key_material() {
    let operations = FakeOperations::default();
    let deploy = engine(
        usable_certificate(),
        activation_for(&request()),
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
    );

    let _outcome = deploy
        .deploy_https(command(), request())
        .expect("deploy success");

    assert!(operations.evidence.borrow().is_empty());
}

#[test]
fn runtime_receipt_must_match_requested_participant() {
    let operations = FakeOperations::default();
    let deploy = engine_with_runtime(
        usable_certificate(),
        RuntimeActivationOutcome::Activated(ParticipantReceipt {
            workload: WorkloadId::parse("other-workload").expect("workload"),
            machine: MachineId::parse("machine-a").expect("machine"),
            revision: RuntimeRevision::new(3),
        }),
        activation_for(&request()),
        operations,
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        deploy.deploy_https(command(), request()),
        Err(DeployFailure::RuntimeParticipantFailed)
    );
}

#[test]
fn serving_activation_must_match_committed_route_identity() {
    let operations = FakeOperations::default();
    let deploy = engine(
        usable_certificate(),
        ServingActivationObservation::Acknowledged {
            route: RouteId::parse("other-route").expect("route"),
            hostname: Hostname::parse("app.example.com").expect("hostname"),
            target: ServingTarget::parse("gateway-a").expect("target"),
            generation: ServingGeneration::new(11),
        },
        operations,
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        deploy.deploy_https(command(), request()),
        Err(DeployFailure::ServingFailed(
            ServingFailure::LiveObservationUnknown
        ))
    );
}

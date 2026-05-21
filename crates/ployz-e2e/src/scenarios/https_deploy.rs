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
    CertificatePolicy, DomainCertificatePort, DomainClaim, DomainClaimObservation, DomainClaimPort,
    DomainFailure, DomainName, DomainPendingReason, DomainReadinessService, DomainReadyRecord,
    DomainResource, DomainServingActivation, DomainServingPort, DomainServingReadiness,
    DomainStatus, DomainStatusPort, UsableDomainCertificate,
};
use ployz::error::{DeployFailure, PrimitiveFailure, RuntimeFailure, ServingFailure};
use ployz::operation::{
    AuthorityDecision, AuthorityEpoch, AuthorityPort, ClaimHash, CommandEnvelope, CommandIssuer,
    CommandRunner, FenceEpoch, IdempotencyKey, MutationContext, MutationIntent, OperationId,
    PrincipalId, ScopeId, TypedResourceId,
};
use ployz::runtime::{
    MachineId, ParticipantReceipt, RuntimeActivationOutcome, RuntimeActivationRequest, RuntimePort,
    RuntimeRevision, WorkloadId,
};
use ployz::serving::{
    RouteId, ServingActivationObservation, ServingCommitReceipt, ServingGeneration, ServingPort,
    ServingSnapshot, ServingTarget,
};
use polis::{EvidenceKind, OperationEvidence, TerminalMarker};

type FakeDomainReadiness =
    DomainReadinessService<FakeDomains, FakeDomains, FakeDomains, FakeDomains>;

#[derive(Clone)]
pub(super) struct FakeDomains {
    certificate: CertificateUsability,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
}

impl DomainClaimPort for FakeDomains {
    fn claim_domain(
        &self,
        context: &MutationContext,
        resource: TypedResourceId<DomainResource>,
        _domain: &DomainName,
    ) -> Result<DomainClaimObservation, DomainFailure> {
        self.contexts.borrow_mut().push(context.clone());
        Ok(DomainClaimObservation {
            resource,
            holder: PrincipalId::parse("node-a").expect("holder"),
            epoch: FenceEpoch::new(1).expect("fence epoch"),
            claim_hash: ClaimHash::parse("claim-hash-a").expect("claim hash"),
            expires_at: UNIX_EPOCH + Duration::from_secs(60),
        })
    }
}

impl DomainCertificatePort for FakeDomains {
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

impl DomainServingPort for FakeDomains {
    fn activate_certificate(
        &self,
        _context: &MutationContext,
        _claim: &DomainClaim,
        _domain: &DomainName,
        _certificate: &UsableDomainCertificate,
    ) -> Result<DomainServingActivation, DomainFailure> {
        Ok(DomainServingActivation::active(ServingGeneration::new(11)))
    }

    fn verify_certificate_activation(
        &self,
        _context: &MutationContext,
        _domain: &DomainName,
        _certificate: &UsableDomainCertificate,
        serving_generation: ServingGeneration,
    ) -> Result<DomainServingReadiness, DomainFailure> {
        Ok(DomainServingReadiness::Active(
            DomainServingActivation::active(serving_generation),
        ))
    }
}

impl DomainStatusPort for FakeDomains {
    fn status(&self, _domain: &DomainName) -> Result<DomainStatus, DomainFailure> {
        Ok(DomainStatus::Unknown)
    }

    fn record_pending(
        &self,
        _context: &MutationContext,
        _domain: &DomainName,
        _reason: DomainPendingReason,
    ) -> Result<(), DomainFailure> {
        Ok(())
    }

    fn record_ready(
        &self,
        _context: &MutationContext,
        _ready: DomainReadyRecord,
    ) -> Result<(), DomainFailure> {
        Ok(())
    }

    fn record_failed(
        &self,
        _context: &MutationContext,
        _domain: &DomainName,
        _failure: DomainFailure,
    ) -> Result<(), DomainFailure> {
        Ok(())
    }
}

#[derive(Clone)]
pub(super) struct FakeRuntime {
    outcome: RuntimeActivationOutcome,
}

impl RuntimePort for FakeRuntime {
    fn activate_participant(
        &self,
        request: RuntimeActivationRequest,
    ) -> Result<RuntimeActivationOutcome, RuntimeFailure> {
        assert_eq!(request.context.authority().epoch(), AuthorityEpoch::new(7));
        Ok(self.outcome.clone())
    }
}

#[derive(Clone)]
pub(super) struct FakeServing {
    activation: ServingActivationObservation,
}

impl ServingPort for FakeServing {
    fn commit_snapshot(
        &self,
        context: &MutationContext,
        snapshot: ServingSnapshot,
    ) -> Result<ServingCommitReceipt, ServingFailure> {
        assert_eq!(context.authority().epoch(), AuthorityEpoch::new(7));
        Ok(ServingCommitReceipt {
            generation: snapshot.generation,
        })
    }

    fn activation_status(
        &self,
        _target: &ServingTarget,
    ) -> Result<ServingActivationObservation, ServingFailure> {
        Ok(self.activation.clone())
    }
}

#[derive(Clone, Default)]
pub(super) struct FakeOperations {
    pub(super) evidence: Rc<RefCell<Vec<OperationEvidence>>>,
    pub(super) terminal: Rc<RefCell<Vec<TerminalMarker>>>,
}

impl polis::OperationBackend for FakeOperations {
    fn start_or_replay(
        &self,
        _request: &polis::OperationRequest,
    ) -> polis::Result<polis::BackendOperationStart> {
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

pub(super) fn command() -> CommandEnvelope<DeployCommand> {
    CommandIssuer::new(AllowAuthority)
        .issue::<DeployCommand>(MutationIntent {
            operation: OperationId::parse("deploy-1").expect("operation"),
            idempotency: IdempotencyKey::parse("idem-1").expect("idempotency"),
            principal: PrincipalId::parse("node-a").expect("principal"),
            scope: ScopeId::parse("cluster").expect("scope"),
            command: ployz::operation::CommandKind::parse("deploy").expect("command"),
            payload_hash: vec![1],
            resources: vec![
                ployz::operation::FingerprintedResource::parse("route:app").expect("route"),
            ],
            submitted_fence: None,
            deadline: UNIX_EPOCH + Duration::from_secs(60),
        })
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
    let domains = FakeDomains {
        certificate,
        contexts,
    };
    DeployEngine::new(
        DomainReadinessService::new(domains.clone(), domains.clone(), domains.clone(), domains),
        FakeRuntime {
            outcome: RuntimeActivationOutcome::Activated(ParticipantReceipt {
                workload: WorkloadId::parse("workload-app").expect("workload"),
                machine: MachineId::parse("machine-a").expect("machine"),
                revision: RuntimeRevision::new(3),
            }),
        },
        FakeServing { activation },
        CommandRunner::new(operations),
    )
}

fn engine_with_runtime(
    certificate: CertificateUsability,
    runtime: RuntimeActivationOutcome,
    activation: ServingActivationObservation,
    operations: FakeOperations,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
) -> DeployEngine<FakeDomainReadiness, FakeRuntime, FakeServing, CommandRunner<FakeOperations>> {
    let domains = FakeDomains {
        certificate,
        contexts,
    };
    DeployEngine::new(
        DomainReadinessService::new(domains.clone(), domains.clone(), domains.clone(), domains),
        FakeRuntime { outcome: runtime },
        FakeServing { activation },
        CommandRunner::new(operations),
    )
}

#[test]
fn https_deploy_ensures_cert_commits_serving_and_verifies_activation() {
    let operations = FakeOperations::default();
    let contexts = Rc::new(RefCell::new(Vec::new()));
    let deploy = engine(
        usable_certificate(),
        ServingActivationObservation::Acknowledged {
            generation: ServingGeneration::new(11),
        },
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
    assert!(
        operations
            .evidence
            .borrow()
            .iter()
            .any(|evidence| matches!(evidence.kind, EvidenceKind::Checkpoint(_)))
    );
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
            ServingActivationObservation::Acknowledged {
                generation: ServingGeneration::new(11),
            },
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
        Err(DeployFailure::ServingActivationFailed)
    );
    assert_eq!(
        operations.terminal.borrow().as_slice(),
        [TerminalMarker::Failed(Vec::new())]
    );
}

#[test]
fn operation_evidence_does_not_render_private_key_material() {
    let operations = FakeOperations::default();
    let deploy = engine(
        usable_certificate(),
        ServingActivationObservation::Acknowledged {
            generation: ServingGeneration::new(11),
        },
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
    );

    let _outcome = deploy
        .deploy_https(command(), request())
        .expect("deploy success");

    assert!(operations.evidence.borrow().iter().all(|evidence| matches!(
        evidence.kind,
        EvidenceKind::Checkpoint(_) | EvidenceKind::Observation(_) | EvidenceKind::Failure(_)
    )));
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
        ServingActivationObservation::Acknowledged {
            generation: ServingGeneration::new(11),
        },
        operations,
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        deploy.deploy_https(command(), request()),
        Err(DeployFailure::RuntimeParticipantFailed)
    );
}

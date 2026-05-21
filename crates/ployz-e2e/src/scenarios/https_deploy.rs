use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use ployz::acme::{
    CertificateActivation, CertificateDeadline, CertificateMaterialState, CertificatePort,
    CertificateStatus, CertificateUsability, EnsureCertificateOutcome, Hostname, HttpsBinding,
    RevocationFreshness,
};
use ployz::deploy::{DeployEngine, DeployManifest, DeployRequest};
use ployz::error::{
    CertificateFailure, DeployFailure, PrimitiveFailure, RuntimeFailure, ServingFailure,
};
use ployz::operation::{
    AuthorityContext, AuthorityEpoch, EvidenceKind, IdempotencyKey, MutationContext,
    OperationEvidence, OperationId, OperationPort, PrincipalId, ScopeId, TerminalMarker,
};
use ployz::runtime::{
    MachineId, ParticipantReceipt, RuntimeActivationOutcome, RuntimeActivationRequest, RuntimePort,
    RuntimeRevision, WorkloadId,
};
use ployz::serving::{
    RouteId, ServingActivationStatus, ServingCheckpoint, ServingGeneration, ServingPort,
    ServingSnapshot, ServingTarget,
};

#[derive(Clone)]
pub(super) struct FakeCertificates {
    outcome: EnsureCertificateOutcome,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
}

impl CertificatePort for FakeCertificates {
    fn ensure_usable(
        &self,
        context: &MutationContext,
        _binding: &HttpsBinding,
        _deadline: CertificateDeadline,
    ) -> Result<EnsureCertificateOutcome, CertificateFailure> {
        self.contexts.borrow_mut().push(context.clone());
        Ok(self.outcome.clone())
    }

    fn status(&self, _binding: &HttpsBinding) -> Result<CertificateStatus, CertificateFailure> {
        Ok(CertificateStatus::Unknown)
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
        assert_eq!(request.context.authority.epoch, AuthorityEpoch::new(7));
        Ok(self.outcome.clone())
    }
}

#[derive(Clone)]
pub(super) struct FakeServing {
    activation: ServingActivationStatus,
}

impl ServingPort for FakeServing {
    fn commit_snapshot(
        &self,
        context: &MutationContext,
        snapshot: ServingSnapshot,
    ) -> Result<ServingCheckpoint, ServingFailure> {
        assert_eq!(context.authority.epoch, AuthorityEpoch::new(7));
        Ok(ServingCheckpoint {
            generation: snapshot.generation,
        })
    }

    fn activation_status(
        &self,
        _target: &ServingTarget,
    ) -> Result<ServingActivationStatus, ServingFailure> {
        Ok(self.activation.clone())
    }
}

#[derive(Clone, Default)]
pub(super) struct FakeOperations {
    pub(super) evidence: Rc<RefCell<Vec<OperationEvidence>>>,
    pub(super) terminal: Rc<RefCell<Vec<TerminalMarker>>>,
}

impl OperationPort for FakeOperations {
    fn record_evidence(&self, evidence: OperationEvidence) -> Result<(), PrimitiveFailure> {
        self.evidence.borrow_mut().push(evidence);
        Ok(())
    }

    fn terminalize(
        &self,
        _operation: &OperationId,
        marker: TerminalMarker,
    ) -> Result<(), PrimitiveFailure> {
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
        operation: OperationId::parse("deploy-1").expect("operation"),
        idempotency: IdempotencyKey::parse("idem-1").expect("idempotency"),
        authority: AuthorityContext {
            principal: PrincipalId::parse("node-a").expect("principal"),
            scope: ScopeId::parse("cluster").expect("scope"),
            epoch: AuthorityEpoch::new(7),
        },
        fence: None,
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

pub(super) fn engine(
    certificate: CertificateUsability,
    activation: ServingActivationStatus,
    operations: FakeOperations,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
) -> DeployEngine<FakeCertificates, FakeRuntime, FakeServing, FakeOperations> {
    DeployEngine::new(
        FakeCertificates {
            outcome: EnsureCertificateOutcome::Usable(certificate),
            contexts,
        },
        FakeRuntime {
            outcome: RuntimeActivationOutcome::Activated(ParticipantReceipt {
                workload: WorkloadId::parse("workload-app").expect("workload"),
                machine: MachineId::parse("machine-a").expect("machine"),
                revision: RuntimeRevision::new(3),
            }),
        },
        FakeServing { activation },
        operations,
    )
}

#[test]
fn https_deploy_ensures_cert_commits_serving_and_verifies_activation() {
    let operations = FakeOperations::default();
    let contexts = Rc::new(RefCell::new(Vec::new()));
    let deploy = engine(
        usable_certificate(),
        ServingActivationStatus::Acknowledged(ServingCheckpoint {
            generation: ServingGeneration::new(11),
        }),
        operations.clone(),
        contexts.clone(),
    );

    let outcome = deploy.deploy_https(request()).expect("deploy success");

    assert_eq!(outcome.runtime.revision, RuntimeRevision::new(3));
    assert_eq!(outcome.serving.generation, ServingGeneration::new(11));
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
            .any(|evidence| evidence.kind == EvidenceKind::Checkpoint)
    );
}

#[test]
fn revoked_or_unknown_certificate_freshness_fails_deploy() {
    let mut certificate = usable_certificate();
    certificate.revocation = RevocationFreshness::Unknown;
    let operations = FakeOperations::default();
    let deploy = engine(
        certificate,
        ServingActivationStatus::Acknowledged(ServingCheckpoint {
            generation: ServingGeneration::new(11),
        }),
        operations,
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        deploy.deploy_https(request()),
        Err(DeployFailure::CertificateUnusable)
    );
}

#[test]
fn serving_commit_without_activation_is_not_success() {
    let operations = FakeOperations::default();
    let deploy = engine(
        usable_certificate(),
        ServingActivationStatus::Unknown,
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
    );

    assert_eq!(
        deploy.deploy_https(request()),
        Err(DeployFailure::ServingActivationFailed)
    );
    assert!(operations.terminal.borrow().is_empty());
}

#[test]
fn operation_evidence_does_not_render_private_key_material() {
    let operations = FakeOperations::default();
    let deploy = engine(
        usable_certificate(),
        ServingActivationStatus::Acknowledged(ServingCheckpoint {
            generation: ServingGeneration::new(11),
        }),
        operations.clone(),
        Rc::new(RefCell::new(Vec::new())),
    );

    let _outcome = deploy.deploy_https(request()).expect("deploy success");
    let rendered = format!("{:?}", operations.evidence.borrow());

    assert!(!rendered.contains("PRIVATE KEY"));
}

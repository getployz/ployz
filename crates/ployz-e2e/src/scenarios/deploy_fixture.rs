use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz::acme::{
    CertificateActivation, CertificateMaterialState, CertificateUsability, Hostname, HttpsBinding,
    RevocationFreshness,
};
use ployz::deploy::{DeployEngine, DeployManifest, DeployRequest};
use ployz::domain::{
    CertificatePolicy, DomainCertificatePort, DomainCertificateReadiness, DomainClaim,
    DomainClaimPort, DomainFailure, DomainName, DomainPendingReason, DomainReadinessService,
    DomainReadyRecord, DomainServingActivation, DomainServingPort, DomainServingReadiness,
    DomainStatus, DomainStatusFailure, DomainStatusPort, UsableDomainCertificate,
};
use ployz::error::{RuntimeFailure, ServingFailure};
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
    RouteId, ServingActivationObservation, ServingActivationPort, ServingCommitObservation,
    ServingCommitRequest, ServingCommittedSnapshot, ServingGeneration, ServingSnapshotPort,
    ServingTarget,
};

pub(super) type FakeDomainReadiness = DomainReadinessService<
    FakeDomainClaims,
    FakeDomainCertificates,
    FakeDomainServing,
    FakeDomainStatus,
>;

#[derive(Clone)]
pub(super) struct FakeDomainClaims {
    contexts: Rc<RefCell<Vec<MutationContext>>>,
}

impl DomainClaimPort for FakeDomainClaims {
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

#[derive(Clone)]
pub(super) struct FakeDomainCertificates {
    certificate: CertificateUsability,
}

impl DomainCertificatePort for FakeDomainCertificates {
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
pub(super) struct FakeDomainServing;

impl DomainServingPort for FakeDomainServing {
    fn activate_certificate(
        &self,
        _context: &MutationContext,
        _claim: &DomainClaim,
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

#[derive(Clone)]
pub(super) struct FakeDomainStatus {
    status: Rc<RefCell<DomainStatus>>,
}

impl DomainStatusPort for FakeDomainStatus {
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
        *self.status.borrow_mut() = DomainStatus::Pending(reason);
        Ok(())
    }

    async fn record_ready(
        &self,
        _context: &MutationContext,
        ready: DomainReadyRecord,
    ) -> Result<(), DomainFailure> {
        *self.status.borrow_mut() = DomainStatus::Ready(ready);
        Ok(())
    }

    async fn record_failed(
        &self,
        _context: &MutationContext,
        _domain: &DomainName,
        failure: DomainStatusFailure,
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
    snapshots: Rc<RefCell<Vec<ServingCommittedSnapshot>>>,
    observer_snapshots: Rc<RefCell<Vec<ServingCommittedSnapshot>>>,
    observer_mode: ServingObserverMode,
}

#[derive(Clone, Copy)]
enum ServingObserverMode {
    FollowsCommits,
    Lagging,
}

#[derive(Clone)]
pub(super) struct FakeServingCommitState {
    snapshots: Rc<RefCell<Vec<ServingCommittedSnapshot>>>,
    observer_snapshots: Rc<RefCell<Vec<ServingCommittedSnapshot>>>,
}

impl Default for FakeServingCommitState {
    fn default() -> Self {
        Self {
            snapshots: Rc::new(RefCell::new(Vec::new())),
            observer_snapshots: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

#[derive(Clone)]
pub(super) struct SharedDeployState {
    domain_status: Rc<RefCell<DomainStatus>>,
    runtime_status: Rc<RefCell<RuntimeParticipantStatus>>,
    serving_commits: Rc<RefCell<usize>>,
    serving_commit_state: FakeServingCommitState,
}

impl Default for SharedDeployState {
    fn default() -> Self {
        Self {
            domain_status: Rc::new(RefCell::new(DomainStatus::Unknown)),
            runtime_status: Rc::new(RefCell::new(RuntimeParticipantStatus::Missing)),
            serving_commits: Rc::new(RefCell::new(0)),
            serving_commit_state: FakeServingCommitState::default(),
        }
    }
}

impl SharedDeployState {
    #[must_use]
    pub(super) fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub(super) fn domain_status(&self) -> Rc<RefCell<DomainStatus>> {
        self.domain_status.clone()
    }

    #[must_use]
    pub(super) fn serving_commits(&self) -> Rc<RefCell<usize>> {
        self.serving_commits.clone()
    }

    #[must_use]
    pub(super) fn runtime_for(&self, receipt: ParticipantReceipt) -> FakeRuntime {
        FakeRuntime {
            outcome: RuntimeActivationOutcome::Activated(receipt.clone()),
            status: self.runtime_status.clone(),
            activation_visible: true,
            activations: Rc::new(RefCell::new(0)),
            verifications: Rc::new(RefCell::new(0)),
        }
    }
}

impl ServingSnapshotPort for FakeServing {
    async fn commit_snapshot(
        &self,
        context: &MutationContext,
        request: &ServingCommitRequest,
    ) -> Result<(), ServingFailure> {
        assert_eq!(context.authority().epoch(), AuthorityEpoch::new(7));
        *self.commits.borrow_mut() += 1;
        let snapshot = ServingCommittedSnapshot::from_request(request)?;
        self.snapshots.borrow_mut().push(snapshot.clone());
        match self.observer_mode {
            ServingObserverMode::FollowsCommits => {
                self.observer_snapshots.borrow_mut().push(snapshot);
            }
            ServingObserverMode::Lagging => {}
        }
        Ok(())
    }

    async fn commit_status(
        &self,
        _context: &MutationContext,
        request: &ServingCommitRequest,
    ) -> Result<ServingCommitObservation, ServingFailure> {
        let snapshots = self.snapshots.borrow();
        let current = snapshots
            .iter()
            .rev()
            .find(|snapshot| snapshot.identity().route() == request.route())
            .cloned();
        Ok(match current {
            Some(current) => ServingCommitObservation::Current(current),
            None => ServingCommitObservation::Missing,
        })
    }
}

impl ServingActivationPort for FakeServing {
    async fn await_observed_commit(
        &self,
        _context: &MutationContext,
        request: &ServingCommitRequest,
        _deadline: SystemTime,
    ) -> Result<ServingCommitObservation, ServingFailure> {
        let snapshots = self.observer_snapshots.borrow();
        let current = snapshots
            .iter()
            .rev()
            .find(|snapshot| snapshot.identity().route() == request.route())
            .cloned();
        Ok(match current {
            Some(current) => ServingCommitObservation::Current(current),
            None => ServingCommitObservation::Missing,
        })
    }

    fn activation_status(
        &self,
        _context: &MutationContext,
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
        deadline: SystemTime::now() + Duration::from_secs(60),
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
        SystemTime::now() + Duration::from_secs(60),
    )
}

pub(super) struct DeployFixtureBuilder {
    certificate: CertificateUsability,
    activation: ServingActivationObservation,
    contexts: Rc<RefCell<Vec<MutationContext>>>,
    status: Rc<RefCell<DomainStatus>>,
    runtime: FakeRuntime,
    serving_commits: Rc<RefCell<usize>>,
    serving_commit_state: FakeServingCommitState,
    observer_mode: ServingObserverMode,
}

impl DeployFixtureBuilder {
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            certificate: usable_certificate(),
            activation: activation_for(&request()),
            contexts: Rc::new(RefCell::new(Vec::new())),
            status: Rc::new(RefCell::new(DomainStatus::Unknown)),
            runtime: runtime_for(receipt()),
            serving_commits: Rc::new(RefCell::new(0)),
            serving_commit_state: FakeServingCommitState::default(),
            observer_mode: ServingObserverMode::FollowsCommits,
        }
    }

    #[must_use]
    pub(super) fn certificate(mut self, certificate: CertificateUsability) -> Self {
        self.certificate = certificate;
        self
    }

    #[must_use]
    pub(super) fn activation(mut self, activation: ServingActivationObservation) -> Self {
        self.activation = activation;
        self
    }

    #[must_use]
    pub(super) fn contexts(mut self, contexts: Rc<RefCell<Vec<MutationContext>>>) -> Self {
        self.contexts = contexts;
        self
    }

    #[must_use]
    pub(super) fn status(mut self, status: Rc<RefCell<DomainStatus>>) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub(super) fn runtime(mut self, runtime: FakeRuntime) -> Self {
        self.runtime = runtime;
        self
    }

    #[must_use]
    pub(super) fn runtime_outcome(mut self, outcome: RuntimeActivationOutcome) -> Self {
        self.runtime = FakeRuntime {
            outcome,
            status: Rc::new(RefCell::new(RuntimeParticipantStatus::Missing)),
            activation_visible: true,
            activations: Rc::new(RefCell::new(0)),
            verifications: Rc::new(RefCell::new(0)),
        };
        self
    }

    #[must_use]
    pub(super) fn state(mut self, state: SharedDeployState) -> Self {
        self.status = state.domain_status();
        self.runtime = state.runtime_for(receipt());
        self.serving_commits = state.serving_commits();
        self.serving_commit_state = state.serving_commit_state;
        self
    }

    #[must_use]
    pub(super) fn serving_commits(mut self, commits: Rc<RefCell<usize>>) -> Self {
        self.serving_commits = commits;
        self
    }

    #[must_use]
    pub(super) fn serving_commit_state(mut self, state: FakeServingCommitState) -> Self {
        self.serving_commit_state = state;
        self
    }

    #[must_use]
    pub(super) fn lagging_serving_observer(mut self) -> Self {
        self.observer_mode = ServingObserverMode::Lagging;
        self
    }

    #[must_use]
    pub(super) fn build(
        self,
    ) -> DeployEngine<FakeDomainReadiness, FakeRuntime, FakeServing, FakeServing> {
        let claims = FakeDomainClaims {
            contexts: self.contexts,
        };
        let certificates = FakeDomainCertificates {
            certificate: self.certificate,
        };
        let domain_serving = FakeDomainServing;
        let domain_status = FakeDomainStatus {
            status: self.status,
        };
        let serving = FakeServing {
            activation: self.activation,
            commits: self.serving_commits,
            snapshots: self.serving_commit_state.snapshots,
            observer_snapshots: self.serving_commit_state.observer_snapshots,
            observer_mode: self.observer_mode,
        };
        DeployEngine::new(
            DomainReadinessService::new(claims, certificates, domain_serving, domain_status),
            self.runtime,
            serving.clone(),
            serving,
        )
    }
}

pub(super) fn runtime_for(receipt: ParticipantReceipt) -> FakeRuntime {
    runtime_with_activation_visibility(
        receipt,
        true,
        Rc::new(RefCell::new(0)),
        Rc::new(RefCell::new(0)),
    )
}

pub(super) fn runtime_with_activation_visibility(
    receipt: ParticipantReceipt,
    activation_visible: bool,
    activations: Rc<RefCell<usize>>,
    verifications: Rc<RefCell<usize>>,
) -> FakeRuntime {
    FakeRuntime {
        outcome: RuntimeActivationOutcome::Activated(receipt.clone()),
        status: Rc::new(RefCell::new(RuntimeParticipantStatus::Missing)),
        activation_visible,
        activations,
        verifications,
    }
}

pub(super) fn receipt() -> ParticipantReceipt {
    ParticipantReceipt {
        workload: WorkloadId::parse("workload-app").expect("workload"),
        machine: MachineId::parse("machine-a").expect("machine"),
        revision: RuntimeRevision::new(3),
    }
}

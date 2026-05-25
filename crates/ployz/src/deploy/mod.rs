//! Deploy orchestration product ports.

use std::time::SystemTime;

use crate::acme::{
    CertificateUnusableReason, CertificateUsability, HttpsBinding,
    certificate_is_usable as acme_certificate_is_usable,
    certificate_unusable_reason as acme_certificate_unusable_reason,
};
use crate::domain::{
    CertificatePolicy, DomainAdd, DomainFailure, DomainName, DomainPendingReason,
    DomainReadinessOutcome, DomainReadinessPort, DomainReady,
};
use crate::error::{DeployFailure, ServingFailure};
use crate::machine::MachineId;
use crate::operation::MutationContext;
use crate::runtime::{
    ParticipantReceipt, RuntimeActivationOutcome, RuntimeActivationRequest,
    RuntimeParticipantStatus, RuntimeParticipantVerification, RuntimePort, WorkloadId,
};
use crate::serving::{
    RouteId, ServingActivationPort, ServingActivationProof, ServingCommitMatch, ServingGeneration,
    ServingSnapshotPort, ServingTarget,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployManifest {
    pub https: HttpsBinding,
    pub route: RouteId,
    pub serving_target: ServingTarget,
    pub serving_generation: ServingGeneration,
    pub workload: WorkloadId,
    pub machine: MachineId,
    pub minimum_certificate_valid_until: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployRequest {
    pub manifest: DeployManifest,
    pub deadline: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOutcome {
    pub domain: DomainReady,
    pub runtime: ParticipantReceipt,
    pub serving: ServingActivationProof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeployDesiredState {
    domain: DomainName,
    certificate_policy: CertificatePolicy,
    https: HttpsBinding,
    route: RouteId,
    serving_target: ServingTarget,
    serving_generation: ServingGeneration,
    workload: WorkloadId,
    machine: MachineId,
}

impl DeployDesiredState {
    fn from_request(request: &DeployRequest) -> Result<Self, DeployFailure> {
        Ok(Self {
            domain: DomainName::parse(request.manifest.https.hostname.as_str())
                .map_err(map_domain_to_deploy)?,
            certificate_policy: CertificatePolicy {
                minimum_valid_until: request.manifest.minimum_certificate_valid_until,
            },
            https: request.manifest.https.clone(),
            route: request.manifest.route.clone(),
            serving_target: request.manifest.serving_target.clone(),
            serving_generation: request.manifest.serving_generation,
            workload: request.manifest.workload.clone(),
            machine: request.manifest.machine.clone(),
        })
    }

    #[must_use]
    fn domain_request(&self) -> DomainAdd {
        DomainAdd {
            domain: self.domain.clone(),
            certificate_policy: self.certificate_policy,
        }
    }

    #[must_use]
    fn domain_matches(&self, ready: &DomainReady) -> bool {
        ready.domain() == &self.domain
            && certificate_is_usable(
                ready.certificate().certificate(),
                &self.https,
                self.certificate_policy.minimum_valid_until,
            )
    }

    #[must_use]
    fn runtime_receipt(&self, status: RuntimeParticipantStatus) -> Option<ParticipantReceipt> {
        let RuntimeParticipantStatus::Active(receipt) = status else {
            return None;
        };
        if receipt.workload == self.workload && receipt.machine == self.machine {
            Some(receipt)
        } else {
            None
        }
    }

    #[must_use]
    fn serving_matches(&self, proof: &ServingActivationProof) -> bool {
        proof.route() == &self.route
            && proof.hostname() == &self.https.hostname
            && proof.target() == &self.serving_target
            && proof.generation() == self.serving_generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeployPlanStep<T> {
    Current(T),
    Apply,
}

impl<T> DeployPlanStep<T> {
    #[cfg(test)]
    #[must_use]
    fn is_current(&self) -> bool {
        matches!(self, Self::Current(_))
    }

    #[cfg(test)]
    #[must_use]
    fn is_apply(&self) -> bool {
        matches!(self, Self::Apply)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeployPlan {
    domain: DeployPlanStep<DomainReady>,
    runtime: DeployPlanStep<ParticipantReceipt>,
    serving: DeployPlanStep<ServingActivationProof>,
}

impl DeployPlan {
    #[must_use]
    fn from_observations(
        desired: &DeployDesiredState,
        domain: Option<DomainReady>,
        runtime: Option<ParticipantReceipt>,
        serving: Option<ServingActivationProof>,
    ) -> Self {
        let domain = match domain {
            Some(ready) if desired.domain_matches(&ready) => DeployPlanStep::Current(ready),
            Some(_) | None => DeployPlanStep::Apply,
        };
        let runtime = match runtime {
            Some(receipt)
                if receipt.workload == desired.workload && receipt.machine == desired.machine =>
            {
                DeployPlanStep::Current(receipt)
            }
            Some(_) | None => DeployPlanStep::Apply,
        };
        let serving = match serving {
            Some(proof) if desired.serving_matches(&proof) => DeployPlanStep::Current(proof),
            Some(_) | None => DeployPlanStep::Apply,
        };
        Self {
            domain,
            runtime,
            serving,
        }
    }

    #[cfg(test)]
    #[must_use]
    fn is_noop(&self) -> bool {
        self.domain.is_current() && self.runtime.is_current() && self.serving.is_current()
    }
}

pub struct DeployEngine<D, R, S, A> {
    domains: D,
    runtime: R,
    serving_snapshots: S,
    serving_activation: A,
}

impl<D, R, S, A> DeployEngine<D, R, S, A> {
    #[must_use]
    pub fn new(domains: D, runtime: R, serving_snapshots: S, serving_activation: A) -> Self {
        Self {
            domains,
            runtime,
            serving_snapshots,
            serving_activation,
        }
    }
}

impl<D, R, S, A> DeployEngine<D, R, S, A>
where
    D: DomainReadinessPort,
    R: RuntimePort,
    S: ServingSnapshotPort,
    A: ServingActivationPort,
{
    pub async fn deploy_https(
        &self,
        context: &MutationContext,
        request: DeployRequest,
    ) -> Result<DeployOutcome, DeployFailure> {
        let desired = DeployDesiredState::from_request(&request)?;
        let plan = self.observe(context, &desired, request.deadline).await?;
        self.execute_plan(context, &request, &desired, plan).await
    }

    async fn observe(
        &self,
        context: &MutationContext,
        desired: &DeployDesiredState,
        deadline: SystemTime,
    ) -> Result<DeployPlan, DeployFailure> {
        let domain = self.observe_domain_ready(context, desired).await?;
        let runtime = self.observe_runtime(context, desired)?;
        let serving = match &domain {
            Some(ready) => {
                self.observe_serving(context, desired, ready, deadline)
                    .await?
            }
            None => None,
        };
        Ok(DeployPlan::from_observations(
            desired, domain, runtime, serving,
        ))
    }

    async fn execute_plan(
        &self,
        context: &MutationContext,
        request: &DeployRequest,
        desired: &DeployDesiredState,
        plan: DeployPlan,
    ) -> Result<DeployOutcome, DeployFailure> {
        let domain = match plan.domain {
            DeployPlanStep::Current(ready) => ready,
            DeployPlanStep::Apply => self.apply_domain_ready(context, desired).await?,
        };
        let runtime = match plan.runtime {
            DeployPlanStep::Current(receipt) => receipt,
            DeployPlanStep::Apply => self.apply_runtime(context, request, desired)?,
        };
        let serving = match plan.serving {
            DeployPlanStep::Current(proof) => proof,
            DeployPlanStep::Apply => {
                self.commit_and_verify_serving(context, request.deadline, desired, &domain)
                    .await?
            }
        };

        Ok(DeployOutcome {
            domain,
            runtime,
            serving,
        })
    }

    async fn observe_domain_ready(
        &self,
        context: &MutationContext,
        desired: &DeployDesiredState,
    ) -> Result<Option<DomainReady>, DeployFailure> {
        match self
            .domains
            .verify_ready(context, desired.domain_request())
            .await
        {
            Ok(ready) if desired.domain_matches(&ready) => Ok(Some(ready)),
            Ok(_) | Err(DomainFailure::UnknownReadiness) => Ok(None),
            Err(error) => Err(map_domain_to_deploy(error)),
        }
    }

    async fn apply_domain_ready(
        &self,
        context: &MutationContext,
        desired: &DeployDesiredState,
    ) -> Result<DomainReady, DeployFailure> {
        match self
            .domains
            .ensure_ready(context, desired.domain_request())
            .await
            .map_err(map_domain_to_deploy)?
        {
            DomainReadinessOutcome::Ready(_) => {}
            DomainReadinessOutcome::Pending(reason) => {
                return Err(map_domain_pending_to_deploy(reason));
            }
        }

        self.observe_domain_ready(context, desired)
            .await?
            .ok_or(DeployFailure::DomainReadinessFailed)
    }

    fn observe_runtime(
        &self,
        context: &MutationContext,
        desired: &DeployDesiredState,
    ) -> Result<Option<ParticipantReceipt>, DeployFailure> {
        let status = self
            .runtime
            .verify_participant(RuntimeParticipantVerification {
                workload: desired.workload.clone(),
                machine: desired.machine.clone(),
                context: context.clone(),
            })
            .map_err(|_| DeployFailure::RuntimeParticipantFailed)?;
        Ok(desired.runtime_receipt(status))
    }

    fn apply_runtime(
        &self,
        context: &MutationContext,
        request: &DeployRequest,
        desired: &DeployDesiredState,
    ) -> Result<ParticipantReceipt, DeployFailure> {
        let outcome = self
            .runtime
            .activate_participant(RuntimeActivationRequest {
                workload: request.manifest.workload.clone(),
                machine: request.manifest.machine.clone(),
                context: context.clone(),
                deadline: request.deadline,
            })
            .map_err(|_| DeployFailure::RuntimeParticipantFailed)?;

        let RuntimeActivationOutcome::Activated(receipt) = outcome else {
            return Err(DeployFailure::RuntimeParticipantFailed);
        };
        if receipt.workload != request.manifest.workload
            || receipt.machine != request.manifest.machine
        {
            return Err(DeployFailure::RuntimeParticipantFailed);
        }

        self.observe_runtime(context, desired)?
            .ok_or(DeployFailure::RuntimeParticipantFailed)
    }

    async fn observe_serving(
        &self,
        context: &MutationContext,
        desired: &DeployDesiredState,
        ready: &DomainReady,
        deadline: SystemTime,
    ) -> Result<Option<ServingActivationProof>, DeployFailure> {
        let commit = ready.serving_commit(
            desired.route.clone(),
            desired.serving_target.clone(),
            desired.serving_generation,
        );
        match self
            .serving_readiness(context, desired, &commit, deadline)
            .await?
        {
            ServingReadinessCheck::Ready(proof) => Ok(Some(proof)),
            ServingReadinessCheck::NotCurrent | ServingReadinessCheck::ActivationUnknown => {
                Ok(None)
            }
        }
    }

    async fn commit_and_verify_serving(
        &self,
        context: &MutationContext,
        deadline: SystemTime,
        desired: &DeployDesiredState,
        ready: &DomainReady,
    ) -> Result<ServingActivationProof, DeployFailure> {
        if !desired.domain_matches(ready) {
            return Err(DeployFailure::DomainReadinessFailed);
        }
        let commit = ready.serving_commit(
            desired.route.clone(),
            desired.serving_target.clone(),
            desired.serving_generation,
        );
        self.serving_snapshots
            .commit_snapshot(context, &commit)
            .await
            .map_err(DeployFailure::ServingFailed)?;

        match self
            .serving_readiness(context, desired, &commit, deadline)
            .await?
        {
            ServingReadinessCheck::Ready(proof) => Ok(proof),
            ServingReadinessCheck::NotCurrent => {
                Err(DeployFailure::ServingFailed(ServingFailure::SnapshotStale))
            }
            ServingReadinessCheck::ActivationUnknown => Err(DeployFailure::ServingFailed(
                ServingFailure::LiveObservationUnknown,
            )),
        }
    }

    async fn serving_readiness(
        &self,
        context: &MutationContext,
        desired: &DeployDesiredState,
        commit: &crate::serving::ServingCommitRequest,
        deadline: SystemTime,
    ) -> Result<ServingReadinessCheck, DeployFailure> {
        if let Err(not_current) = self
            .serving_snapshots
            .commit_status(context, commit)
            .await
            .map_err(DeployFailure::ServingFailed)?
            .try_confirm_commit(commit)
            .map_err(DeployFailure::ServingFailed)?
            .current_or_not()
        {
            return Ok(not_current);
        }

        if let Err(not_current) = self
            .activation_observed_commit(context, commit, deadline)
            .await?
            .current_or_not()
        {
            return Ok(not_current);
        }

        let activation = self
            .serving_activation
            .activation_status(context, commit)
            .map_err(DeployFailure::ServingFailed)?;
        match activation.try_acknowledge_commit(commit) {
            Ok(proof) if desired.serving_matches(&proof) => Ok(ServingReadinessCheck::Ready(proof)),
            Ok(_) => Ok(ServingReadinessCheck::NotCurrent),
            Err(ServingFailure::LiveObservationUnknown) => {
                Ok(ServingReadinessCheck::ActivationUnknown)
            }
            Err(error) => Err(DeployFailure::ServingFailed(error)),
        }
    }

    async fn activation_observed_commit(
        &self,
        context: &MutationContext,
        commit: &crate::serving::ServingCommitRequest,
        deadline: SystemTime,
    ) -> Result<ServingCommitMatch, DeployFailure> {
        self.serving_activation
            .await_observed_commit(context, commit, deadline)
            .await
            .map_err(DeployFailure::ServingFailed)?
            .try_confirm_commit(commit)
            .map_err(DeployFailure::ServingFailed)
    }
}

enum ServingReadinessCheck {
    Ready(ServingActivationProof),
    NotCurrent,
    ActivationUnknown,
}

impl ServingCommitMatch {
    fn current_or_not(self) -> Result<(), ServingReadinessCheck> {
        match self {
            Self::Current => Ok(()),
            Self::Missing | Self::DifferentCurrent => Err(ServingReadinessCheck::NotCurrent),
        }
    }
}

#[must_use]
pub fn certificate_is_usable(
    certificate: &CertificateUsability,
    binding: &HttpsBinding,
    minimum_valid_until: SystemTime,
) -> bool {
    acme_certificate_is_usable(certificate, binding, minimum_valid_until)
}

#[must_use]
pub fn certificate_unusable_reason(
    certificate: &CertificateUsability,
    binding: &HttpsBinding,
    minimum_valid_until: SystemTime,
) -> Option<CertificateUnusableReason> {
    acme_certificate_unusable_reason(certificate, binding, minimum_valid_until)
}

fn map_domain_to_deploy(error: DomainFailure) -> DeployFailure {
    match error {
        DomainFailure::FailureRecordUnavailable { primary, status } => {
            DeployFailure::DomainFailureRecordUnavailable { primary, status }
        }
        DomainFailure::InvalidDomain => DeployFailure::InvalidManifest,
        DomainFailure::ClaimRejected
        | DomainFailure::ClaimResourceMismatch
        | DomainFailure::StaleClaim => DeployFailure::ClaimRejected,
        DomainFailure::CertificateUnusable(_) | DomainFailure::CertificateFailed(_) => {
            DeployFailure::CertificateUnusable
        }
        DomainFailure::ServingActivationFailed => DeployFailure::ServingActivationFailed,
        DomainFailure::ServingFailed(error) => DeployFailure::ServingFailed(error),
        DomainFailure::StatusUnavailable
        | DomainFailure::StatusRowsPayloadInvalid
        | DomainFailure::StatusRowsTimeout
        | DomainFailure::StatusRowsStreamInterrupted
        | DomainFailure::StatusRowsMissedChanges
        | DomainFailure::UnknownReadiness => DeployFailure::DomainReadinessFailed,
    }
}

fn map_domain_pending_to_deploy(reason: DomainPendingReason) -> DeployFailure {
    match reason {
        DomainPendingReason::CertificateIssuance | DomainPendingReason::ServingActivation => {
            DeployFailure::OperationInProgress
        }
        DomainPendingReason::CertificateIssuanceInterrupted => DeployFailure::Interrupted,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::acme::{Hostname, HttpsBinding};
    use crate::domain::{DomainReady, DomainServingActivation, UsableDomainCertificate};
    use crate::serving::{ServingActivationObservation, ServingCommitRequest};

    #[test]
    fn desired_state_changes_with_deploy_material_inputs() {
        let request = request();
        let desired = DeployDesiredState::from_request(&request).expect("desired");

        for changed in [
            DeployRequest {
                manifest: DeployManifest {
                    route: RouteId::parse("route:admin").expect("route"),
                    ..request.manifest.clone()
                },
                ..request.clone()
            },
            DeployRequest {
                manifest: DeployManifest {
                    serving_target: ServingTarget::parse("target:admin").expect("target"),
                    ..request.manifest.clone()
                },
                ..request.clone()
            },
            DeployRequest {
                manifest: DeployManifest {
                    serving_generation: ServingGeneration::new(5),
                    ..request.manifest.clone()
                },
                ..request.clone()
            },
            DeployRequest {
                manifest: DeployManifest {
                    workload: WorkloadId::parse("workload:admin").expect("workload"),
                    ..request.manifest.clone()
                },
                ..request.clone()
            },
            DeployRequest {
                manifest: DeployManifest {
                    machine: MachineId::parse("machine:b").expect("machine"),
                    ..request.manifest.clone()
                },
                ..request.clone()
            },
            DeployRequest {
                manifest: DeployManifest {
                    minimum_certificate_valid_until: UNIX_EPOCH + Duration::from_secs(7_000),
                    ..request.manifest.clone()
                },
                ..request.clone()
            },
        ] {
            assert_ne!(
                desired,
                DeployDesiredState::from_request(&changed).expect("changed desired")
            );
        }
    }

    #[test]
    fn empty_diff_is_a_noop() {
        let request = request();
        let desired = DeployDesiredState::from_request(&request).expect("desired");
        let ready = ready_domain(&desired);
        let proof = serving_proof(&desired, &ready);

        let plan = DeployPlan::from_observations(
            &desired,
            Some(ready),
            Some(receipt(&desired)),
            Some(proof),
        );

        assert!(plan.is_noop());
        assert!(plan.domain.is_current());
        assert!(plan.runtime.is_current());
        assert!(plan.serving.is_current());
    }

    #[test]
    fn diff_keeps_deploy_changes_local_to_changed_surfaces() {
        let request = request();
        let desired = DeployDesiredState::from_request(&request).expect("desired");
        let ready = ready_domain(&desired);
        let proof = serving_proof(&desired, &ready);

        let route_change = DeployDesiredState::from_request(&DeployRequest {
            manifest: DeployManifest {
                route: RouteId::parse("route:admin").expect("route"),
                ..request.manifest.clone()
            },
            ..request.clone()
        })
        .expect("route desired");
        let plan = DeployPlan::from_observations(
            &route_change,
            Some(ready.clone()),
            Some(receipt(&desired)),
            Some(proof),
        );
        assert!(plan.domain.is_current());
        assert!(plan.runtime.is_current());
        assert!(plan.serving.is_apply());

        let workload_change = DeployDesiredState::from_request(&DeployRequest {
            manifest: DeployManifest {
                workload: WorkloadId::parse("workload:admin").expect("workload"),
                ..request.manifest
            },
            ..request
        })
        .expect("workload desired");
        let plan = DeployPlan::from_observations(
            &workload_change,
            Some(ready),
            Some(receipt(&desired)),
            None,
        );
        assert!(plan.domain.is_current());
        assert!(plan.runtime.is_apply());
        assert!(plan.serving.is_apply());
    }

    #[test]
    fn domain_pending_certificate_issuance_maps_to_deploy_in_progress() {
        assert_eq!(
            map_domain_pending_to_deploy(DomainPendingReason::CertificateIssuance),
            DeployFailure::OperationInProgress
        );
    }

    #[test]
    fn failed_domain_status_write_stays_visible_at_deploy_boundary() {
        let error = DomainFailure::FailureRecordUnavailable {
            primary: Box::new(DomainFailure::ServingActivationFailed),
            status: Box::new(DomainFailure::StatusUnavailable),
        };

        assert_eq!(
            map_domain_to_deploy(error),
            DeployFailure::DomainFailureRecordUnavailable {
                primary: Box::new(DomainFailure::ServingActivationFailed),
                status: Box::new(DomainFailure::StatusUnavailable),
            }
        );
    }

    #[test]
    fn domain_interrupted_certificate_issuance_maps_to_deploy_interrupted() {
        assert_eq!(
            map_domain_pending_to_deploy(DomainPendingReason::CertificateIssuanceInterrupted),
            DeployFailure::Interrupted
        );
    }

    fn request() -> DeployRequest {
        DeployRequest {
            manifest: DeployManifest {
                https: HttpsBinding::new(Hostname::parse("app.example.com").expect("hostname")),
                route: RouteId::parse("route:app").expect("route"),
                serving_target: ServingTarget::parse("target:app").expect("target"),
                serving_generation: ServingGeneration::new(4),
                workload: WorkloadId::parse("workload:app").expect("workload"),
                machine: MachineId::parse("machine:a").expect("machine"),
                minimum_certificate_valid_until: UNIX_EPOCH + Duration::from_secs(3_600),
            },
            deadline: UNIX_EPOCH + Duration::from_secs(60),
        }
    }

    fn ready_domain(desired: &DeployDesiredState) -> DomainReady {
        let certificate = CertificateUsability {
            hostname: desired.https.hostname.clone(),
            not_after: UNIX_EPOCH + Duration::from_secs(7_200),
            activation: crate::acme::CertificateActivation::Acknowledged,
            material: crate::acme::CertificateMaterialState::PresentProtected,
            revocation: crate::acme::RevocationFreshness::KnownFresh,
        };
        DomainReady::new(
            desired.domain.clone(),
            UsableDomainCertificate::new(&desired.domain, certificate, desired.certificate_policy)
                .expect("usable certificate"),
            DomainServingActivation::active(desired.serving_generation),
        )
    }

    fn serving_proof(desired: &DeployDesiredState, ready: &DomainReady) -> ServingActivationProof {
        let commit = ServingCommitRequest::replace_current_route(
            desired.route.clone(),
            ready.certificate().certificate().hostname.clone(),
            desired.serving_target.clone(),
            desired.serving_generation,
        );
        ServingActivationObservation::Acknowledged {
            route: desired.route.clone(),
            hostname: desired.https.hostname.clone(),
            target: desired.serving_target.clone(),
            generation: desired.serving_generation,
        }
        .try_acknowledge_commit(&commit)
        .expect("serving proof")
    }

    fn receipt(desired: &DeployDesiredState) -> ParticipantReceipt {
        ParticipantReceipt {
            workload: desired.workload.clone(),
            machine: desired.machine.clone(),
            revision: crate::runtime::RuntimeRevision::new(3),
        }
    }
}

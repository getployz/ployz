//! Deploy orchestration product ports.

use std::time::SystemTime;

use crate::acme::{
    CertificateUnusableReason, CertificateUsability, HttpsBinding,
    certificate_is_usable as acme_certificate_is_usable,
    certificate_unusable_reason as acme_certificate_unusable_reason,
};
use crate::domain::{
    CertificatePolicy, DomainAdd, DomainFailure, DomainName, DomainReadinessPort, DomainReady,
};
use crate::error::{DeployFailure, ServingFailure};
use crate::operation::MutationContext;
use crate::runtime::{
    MachineId, ParticipantReceipt, RuntimeActivationOutcome, RuntimeActivationRequest,
    RuntimeParticipantStatus, RuntimeParticipantVerification, RuntimePort, WorkloadId,
};
use crate::serving::{
    RouteId, ServingActivationProof, ServingGeneration, ServingPort, ServingTarget,
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
struct DeployObservedState {
    domain: Option<DomainReady>,
    runtime: Option<ParticipantReceipt>,
    serving: Option<ServingActivationProof>,
}

impl DeployObservedState {
    #[cfg(test)]
    #[must_use]
    fn new(
        domain: Option<DomainReady>,
        runtime: Option<ParticipantReceipt>,
        serving: Option<ServingActivationProof>,
    ) -> Self {
        Self {
            domain,
            runtime,
            serving,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeployPlanStep {
    AlreadyCurrent,
    Apply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeployPlan {
    domain: DeployPlanStep,
    runtime: DeployPlanStep,
    serving: DeployPlanStep,
}

impl DeployPlan {
    #[must_use]
    fn diff(observed: &DeployObservedState, desired: &DeployDesiredState) -> Self {
        let domain = match &observed.domain {
            Some(ready) if desired.domain_matches(ready) => DeployPlanStep::AlreadyCurrent,
            Some(_) | None => DeployPlanStep::Apply,
        };
        let runtime = match &observed.runtime {
            Some(receipt)
                if receipt.workload == desired.workload && receipt.machine == desired.machine =>
            {
                DeployPlanStep::AlreadyCurrent
            }
            Some(_) | None => DeployPlanStep::Apply,
        };
        let serving = match &observed.serving {
            Some(proof) if desired.serving_matches(proof) => DeployPlanStep::AlreadyCurrent,
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
    fn is_noop(self) -> bool {
        matches!(self.domain, DeployPlanStep::AlreadyCurrent)
            && matches!(self.runtime, DeployPlanStep::AlreadyCurrent)
            && matches!(self.serving, DeployPlanStep::AlreadyCurrent)
    }
}

pub struct DeployEngine<D, R, S> {
    domains: D,
    runtime: R,
    serving: S,
}

impl<D, R, S> DeployEngine<D, R, S> {
    #[must_use]
    pub fn new(domains: D, runtime: R, serving: S) -> Self {
        Self {
            domains,
            runtime,
            serving,
        }
    }
}

impl<D, R, S> DeployEngine<D, R, S>
where
    D: DomainReadinessPort,
    R: RuntimePort,
    S: ServingPort,
{
    pub fn deploy_https(
        &self,
        context: &MutationContext,
        request: DeployRequest,
    ) -> Result<DeployOutcome, DeployFailure> {
        let desired = DeployDesiredState::from_request(&request)?;
        let observed = self.observe(context, &desired)?;
        let plan = DeployPlan::diff(&observed, &desired);
        self.execute_plan(context, &request, &desired, observed, plan)
    }

    fn observe(
        &self,
        context: &MutationContext,
        desired: &DeployDesiredState,
    ) -> Result<DeployObservedState, DeployFailure> {
        let domain = self.observe_domain_ready(context, desired)?;
        let runtime = self.observe_runtime(context, desired)?;
        let serving = match &domain {
            Some(ready) => self.observe_serving(desired, ready)?,
            None => None,
        };
        Ok(DeployObservedState {
            domain,
            runtime,
            serving,
        })
    }

    fn execute_plan(
        &self,
        context: &MutationContext,
        request: &DeployRequest,
        desired: &DeployDesiredState,
        observed: DeployObservedState,
        plan: DeployPlan,
    ) -> Result<DeployOutcome, DeployFailure> {
        let domain = match plan.domain {
            DeployPlanStep::AlreadyCurrent => observed
                .domain
                .ok_or(DeployFailure::DomainReadinessFailed)?,
            DeployPlanStep::Apply => self.apply_domain_ready(context, desired)?,
        };
        let runtime = match plan.runtime {
            DeployPlanStep::AlreadyCurrent => observed
                .runtime
                .ok_or(DeployFailure::RuntimeParticipantFailed)?,
            DeployPlanStep::Apply => self.apply_runtime(context, request, desired)?,
        };
        let serving = match plan.serving {
            DeployPlanStep::AlreadyCurrent => observed
                .serving
                .ok_or(DeployFailure::ServingActivationFailed)?,
            DeployPlanStep::Apply => self.commit_and_verify_serving(context, desired, &domain)?,
        };

        Ok(DeployOutcome {
            domain,
            runtime,
            serving,
        })
    }

    fn observe_domain_ready(
        &self,
        context: &MutationContext,
        desired: &DeployDesiredState,
    ) -> Result<Option<DomainReady>, DeployFailure> {
        match self.domains.verify_ready(context, desired.domain_request()) {
            Ok(ready) if desired.domain_matches(&ready) => Ok(Some(ready)),
            Ok(_) | Err(DomainFailure::UnknownReadiness) => Ok(None),
            Err(error) => Err(map_domain_to_deploy(error)),
        }
    }

    fn apply_domain_ready(
        &self,
        context: &MutationContext,
        desired: &DeployDesiredState,
    ) -> Result<DomainReady, DeployFailure> {
        self.domains
            .ensure_ready(context, desired.domain_request())
            .map_err(map_domain_to_deploy)?;

        self.observe_domain_ready(context, desired)?
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

    fn observe_serving(
        &self,
        desired: &DeployDesiredState,
        ready: &DomainReady,
    ) -> Result<Option<ServingActivationProof>, DeployFailure> {
        let commit = ready.serving_commit(
            desired.route.clone(),
            desired.serving_target.clone(),
            desired.serving_generation,
        );
        let activation = self
            .serving
            .activation_status(&commit)
            .map_err(DeployFailure::ServingFailed)?;
        match activation.try_acknowledge_commit(&commit) {
            Ok(proof) if desired.serving_matches(&proof) => Ok(Some(proof)),
            Ok(_) | Err(ServingFailure::LiveObservationUnknown) => Ok(None),
            Err(error) => Err(DeployFailure::ServingFailed(error)),
        }
    }

    fn commit_and_verify_serving(
        &self,
        context: &MutationContext,
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
        self.serving
            .commit_snapshot(context, &commit)
            .map_err(DeployFailure::ServingFailed)?;

        let activation = self
            .serving
            .activation_status(&commit)
            .map_err(DeployFailure::ServingFailed)?;
        activation
            .try_acknowledge_commit(&commit)
            .map_err(DeployFailure::ServingFailed)
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
        DomainFailure::InvalidDomain => DeployFailure::InvalidManifest,
        DomainFailure::ClaimRejected
        | DomainFailure::ClaimResourceMismatch
        | DomainFailure::StaleClaim => DeployFailure::ClaimRejected,
        DomainFailure::CertificateUnusable(_) | DomainFailure::CertificateFailed(_) => {
            DeployFailure::CertificateUnusable
        }
        DomainFailure::ServingActivationFailed => DeployFailure::ServingActivationFailed,
        DomainFailure::ServingFailed(error) => DeployFailure::ServingFailed(error),
        DomainFailure::StatusUnavailable | DomainFailure::UnknownReadiness => {
            DeployFailure::DomainReadinessFailed
        }
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
        let observed = DeployObservedState::new(Some(ready), Some(receipt(&desired)), Some(proof));

        let plan = DeployPlan::diff(&observed, &desired);

        assert!(plan.is_noop());
        assert_eq!(plan.domain, DeployPlanStep::AlreadyCurrent);
        assert_eq!(plan.runtime, DeployPlanStep::AlreadyCurrent);
        assert_eq!(plan.serving, DeployPlanStep::AlreadyCurrent);
    }

    #[test]
    fn diff_keeps_deploy_changes_local_to_changed_surfaces() {
        let request = request();
        let desired = DeployDesiredState::from_request(&request).expect("desired");
        let ready = ready_domain(&desired);
        let proof = serving_proof(&desired, &ready);
        let observed =
            DeployObservedState::new(Some(ready.clone()), Some(receipt(&desired)), Some(proof));

        let route_change = DeployDesiredState::from_request(&DeployRequest {
            manifest: DeployManifest {
                route: RouteId::parse("route:admin").expect("route"),
                ..request.manifest.clone()
            },
            ..request.clone()
        })
        .expect("route desired");
        let plan = DeployPlan::diff(&observed, &route_change);
        assert_eq!(plan.domain, DeployPlanStep::AlreadyCurrent);
        assert_eq!(plan.runtime, DeployPlanStep::AlreadyCurrent);
        assert_eq!(plan.serving, DeployPlanStep::Apply);

        let workload_change = DeployDesiredState::from_request(&DeployRequest {
            manifest: DeployManifest {
                workload: WorkloadId::parse("workload:admin").expect("workload"),
                ..request.manifest
            },
            ..request
        })
        .expect("workload desired");
        let plan = DeployPlan::diff(
            &DeployObservedState::new(Some(ready), Some(receipt(&desired)), None),
            &workload_change,
        );
        assert_eq!(plan.domain, DeployPlanStep::AlreadyCurrent);
        assert_eq!(plan.runtime, DeployPlanStep::Apply);
        assert_eq!(plan.serving, DeployPlanStep::Apply);
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
        let commit = ServingCommitRequest::new(
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

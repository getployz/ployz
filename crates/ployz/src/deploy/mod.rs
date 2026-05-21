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
use crate::error::{DeployFailure, PrimitiveFailure};
use crate::operation::{CommandBackend, CommandContext, CommandEnvelope};
use crate::runtime::{
    MachineId, ParticipantReceipt, RuntimeActivationOutcome, RuntimeActivationRequest, RuntimePort,
    WorkloadId,
};
use crate::serving::{
    RouteId, ServingActivationObservation, ServingCheckpoint, ServingGeneration, ServingPort,
    ServingSnapshot, ServingTarget,
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
    pub serving: ServingCheckpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployCommand {}

pub struct DeployEngine<D, R, S, O> {
    domains: D,
    runtime: R,
    serving: S,
    commands: O,
}

impl<D, R, S, O> DeployEngine<D, R, S, O> {
    #[must_use]
    pub fn new(domains: D, runtime: R, serving: S, commands: O) -> Self {
        Self {
            domains,
            runtime,
            serving,
            commands,
        }
    }
}

impl<D, R, S, O> DeployEngine<D, R, S, O>
where
    D: DomainReadinessPort,
    R: RuntimePort,
    S: ServingPort,
    O: CommandBackend,
{
    pub fn deploy_https(
        &self,
        command: CommandEnvelope<DeployCommand>,
        request: DeployRequest,
    ) -> Result<DeployOutcome, DeployFailure> {
        self.commands
            .run(command, map_primitive_to_deploy, |context| {
                let domain = self.ensure_domain_ready(context, &request)?;
                let runtime = self.activate_runtime(context, &request)?;
                let serving = self.commit_and_verify_serving(context, &request, &domain)?;

                Ok(DeployOutcome {
                    domain,
                    runtime,
                    serving,
                })
            })
    }

    fn ensure_domain_ready(
        &self,
        context: &CommandContext<'_>,
        request: &DeployRequest,
    ) -> Result<DomainReady, DeployFailure> {
        let domain = DomainName::parse(request.manifest.https.hostname.as_str())
            .map_err(map_domain_to_deploy)?;
        let ready = self
            .domains
            .ensure_ready(
                context.mutation(),
                DomainAdd {
                    domain: domain.clone(),
                    certificate_policy: CertificatePolicy {
                        minimum_valid_until: request.manifest.minimum_certificate_valid_until,
                    },
                },
            )
            .map_err(map_domain_to_deploy)?;

        if ready.domain() != &domain {
            return Err(DeployFailure::DomainReadinessFailed);
        }
        Ok(ready)
    }

    fn activate_runtime(
        &self,
        context: &CommandContext<'_>,
        request: &DeployRequest,
    ) -> Result<ParticipantReceipt, DeployFailure> {
        let outcome = self
            .runtime
            .activate_participant(RuntimeActivationRequest {
                workload: request.manifest.workload.clone(),
                machine: request.manifest.machine.clone(),
                context: context.mutation().clone(),
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

        Ok(receipt)
    }

    fn commit_and_verify_serving(
        &self,
        context: &CommandContext<'_>,
        request: &DeployRequest,
        ready: &DomainReady,
    ) -> Result<ServingCheckpoint, DeployFailure> {
        if ready.domain().as_str() != request.manifest.https.hostname.as_str() {
            return Err(DeployFailure::DomainReadinessFailed);
        }
        let snapshot = ServingSnapshot {
            route: request.manifest.route.clone(),
            hostname: request.manifest.https.hostname.clone(),
            target: request.manifest.serving_target.clone(),
            generation: request.manifest.serving_generation,
        };
        let commit = self
            .serving
            .commit_snapshot(context.mutation(), snapshot)
            .map_err(|_| DeployFailure::ServingActivationFailed)?;
        let checkpoint = ServingCheckpoint::new(commit.generation);

        match self
            .serving
            .activation_status(&request.manifest.serving_target)
            .map_err(|_| DeployFailure::ServingActivationFailed)?
        {
            ServingActivationObservation::Acknowledged { generation }
                if generation == checkpoint.generation() =>
            {
                Ok(checkpoint)
            }
            ServingActivationObservation::Acknowledged { .. }
            | ServingActivationObservation::Failed(_)
            | ServingActivationObservation::Unknown => Err(DeployFailure::ServingActivationFailed),
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

fn map_primitive_to_deploy(error: PrimitiveFailure) -> DeployFailure {
    match error {
        PrimitiveFailure::Unauthorized => DeployFailure::Unauthorized,
        PrimitiveFailure::Conflict => DeployFailure::PreflightFailed,
        PrimitiveFailure::Timeout => DeployFailure::Interrupted,
        PrimitiveFailure::StaleFence => DeployFailure::ClaimRejected,
        PrimitiveFailure::NoResponder => DeployFailure::RuntimeParticipantFailed,
        PrimitiveFailure::FreshnessUnknown => DeployFailure::StaleEvidence,
        PrimitiveFailure::MalformedPayload => DeployFailure::InvalidManifest,
        PrimitiveFailure::TerminalAlreadyWritten | PrimitiveFailure::ReplayUnavailable => {
            DeployFailure::StaleEvidence
        }
    }
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
        DomainFailure::ServingActivationFailed | DomainFailure::ServingFailed(_) => {
            DeployFailure::ServingActivationFailed
        }
        DomainFailure::StatusUnavailable | DomainFailure::UnknownReadiness => {
            DeployFailure::DomainReadinessFailed
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::operation::AuthorityDecision;

    #[test]
    fn unknown_authority_is_not_allowed() {
        let check = AuthorityDecision::Unknown;

        assert_eq!(check.epoch(), None);
    }
}

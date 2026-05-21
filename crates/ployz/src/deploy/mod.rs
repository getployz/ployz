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
use crate::operation::{
    AuthorityPort, CommandBackend, CommandContext, CommandEnvelope, CommandFailureDisposition,
    CommandIssue, CommandIssuer, CommandKind, CommandPayload, FingerprintedResource,
    MutationContext, MutationIntent,
};
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
pub enum DeployCommand {}

pub struct IssuedDeployCommand {
    envelope: CommandEnvelope<DeployCommand>,
    request: DeployRequest,
}

impl DeployCommand {
    pub fn issue<A>(
        issuer: &CommandIssuer<A>,
        command: CommandIssue,
        request: DeployRequest,
    ) -> Result<IssuedDeployCommand, PrimitiveFailure>
    where
        A: AuthorityPort,
    {
        let mut payload = CommandPayload::new("ployz.deploy.https.v1");
        payload.field("hostname", request.manifest.https.hostname.as_str());
        payload.field("route", request.manifest.route.as_str());
        payload.field("serving_target", request.manifest.serving_target.as_str());
        payload.field_u64(
            "serving_generation",
            request.manifest.serving_generation.value(),
        );
        payload.field("workload", request.manifest.workload.as_str());
        payload.field("machine", request.manifest.machine.as_str());
        payload.field_time(
            "minimum_certificate_valid_until",
            request.manifest.minimum_certificate_valid_until,
        );
        payload.field_time("request_deadline", request.deadline);

        let envelope = issuer.issue(MutationIntent {
            operation: command.operation,
            idempotency: command.idempotency,
            principal: command.principal,
            scope: command.scope,
            command: CommandKind::parse("deploy")?,
            payload_hash: payload.finish_hash(),
            resources: vec![
                FingerprintedResource::parse(format!("route:{}", request.manifest.route.as_str()))?,
                FingerprintedResource::parse(format!(
                    "domain:{}",
                    request.manifest.https.hostname.as_str()
                ))?,
            ],
            submitted_fence: None,
            deadline: command.deadline,
        })?;
        Ok(IssuedDeployCommand { envelope, request })
    }
}

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
        command: IssuedDeployCommand,
    ) -> Result<DeployOutcome, DeployFailure> {
        let IssuedDeployCommand { envelope, request } = command;
        self.commands.run_with_replay_and_failure_disposition(
            envelope,
            map_primitive_to_deploy,
            CommandFailureDisposition::Interrupted,
            |context| {
                let domain = self.ensure_domain_ready(context, &request)?;
                let runtime = self.activate_runtime(context, &request)?;
                let serving = self.commit_and_verify_serving(context, &request, &domain)?;

                Ok(DeployOutcome {
                    domain,
                    runtime,
                    serving,
                })
            },
            |mutation| self.verify_replayed_success(mutation, &request),
        )
    }

    fn verify_replayed_success(
        &self,
        mutation: &MutationContext,
        request: &DeployRequest,
    ) -> Result<DeployOutcome, DeployFailure> {
        let domain = self.verify_domain_ready(mutation, request)?;
        let runtime = self.verify_runtime(mutation, request)?;
        let serving = self.verify_serving(request, &domain)?;
        Ok(DeployOutcome {
            domain,
            runtime,
            serving,
        })
    }

    fn ensure_domain_ready(
        &self,
        context: &CommandContext<'_>,
        request: &DeployRequest,
    ) -> Result<DomainReady, DeployFailure> {
        self.ready_domain(context.mutation(), request, |domains, mutation, request| {
            domains.ensure_ready(mutation, request)
        })
    }

    fn verify_domain_ready(
        &self,
        mutation: &MutationContext,
        request: &DeployRequest,
    ) -> Result<DomainReady, DeployFailure> {
        self.ready_domain(mutation, request, |domains, mutation, request| {
            domains.verify_ready(mutation, request)
        })
    }

    fn ready_domain<F>(
        &self,
        mutation: &MutationContext,
        request: &DeployRequest,
        readiness: F,
    ) -> Result<DomainReady, DeployFailure>
    where
        F: FnOnce(&D, &MutationContext, DomainAdd) -> Result<DomainReady, DomainFailure>,
    {
        let domain = DomainName::parse(request.manifest.https.hostname.as_str())
            .map_err(map_domain_to_deploy)?;
        let ready = readiness(
            &self.domains,
            mutation,
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

    fn verify_runtime(
        &self,
        mutation: &MutationContext,
        request: &DeployRequest,
    ) -> Result<ParticipantReceipt, DeployFailure> {
        let status = self
            .runtime
            .verify_participant(RuntimeParticipantVerification {
                workload: request.manifest.workload.clone(),
                machine: request.manifest.machine.clone(),
                context: mutation.clone(),
            })
            .map_err(|_| DeployFailure::RuntimeParticipantFailed)?;

        let RuntimeParticipantStatus::Active(receipt) = status else {
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
    ) -> Result<ServingActivationProof, DeployFailure> {
        if ready.domain().as_str() != request.manifest.https.hostname.as_str() {
            return Err(DeployFailure::DomainReadinessFailed);
        }
        let commit = ready.serving_commit(
            request.manifest.route.clone(),
            request.manifest.serving_target.clone(),
            request.manifest.serving_generation,
        );
        self.serving
            .commit_snapshot(context.mutation(), &commit)
            .map_err(DeployFailure::ServingFailed)?;
        let checkpoint = commit.checkpoint();

        let activation = self
            .serving
            .activation_status(&checkpoint)
            .map_err(DeployFailure::ServingFailed)?;
        activation
            .try_acknowledge(&checkpoint)
            .map_err(DeployFailure::ServingFailed)
    }

    fn verify_serving(
        &self,
        request: &DeployRequest,
        ready: &DomainReady,
    ) -> Result<ServingActivationProof, DeployFailure> {
        if ready.domain().as_str() != request.manifest.https.hostname.as_str() {
            return Err(DeployFailure::DomainReadinessFailed);
        }
        let commit = ready.serving_commit(
            request.manifest.route.clone(),
            request.manifest.serving_target.clone(),
            request.manifest.serving_generation,
        );
        let checkpoint = commit.checkpoint();
        let activation = self
            .serving
            .activation_status(&checkpoint)
            .map_err(DeployFailure::ServingFailed)?;
        activation
            .try_acknowledge(&checkpoint)
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

fn map_primitive_to_deploy(error: PrimitiveFailure) -> DeployFailure {
    match error {
        PrimitiveFailure::Unauthorized => DeployFailure::Unauthorized,
        PrimitiveFailure::Conflict => DeployFailure::PreflightFailed,
        PrimitiveFailure::Timeout => DeployFailure::Interrupted,
        PrimitiveFailure::StaleFence => DeployFailure::ClaimRejected,
        PrimitiveFailure::NoResponder => DeployFailure::RuntimeParticipantFailed,
        PrimitiveFailure::FreshnessUnknown => DeployFailure::StaleEvidence,
        PrimitiveFailure::MalformedPayload => DeployFailure::InvalidManifest,
        PrimitiveFailure::TerminalAlreadyWritten => DeployFailure::StaleEvidence,
        PrimitiveFailure::OperationAlreadySucceeded => DeployFailure::OperationAlreadySucceeded,
        PrimitiveFailure::OperationInProgress => DeployFailure::OperationInProgress,
        PrimitiveFailure::OperationAlreadyFailed => DeployFailure::OperationAlreadyFailed,
        PrimitiveFailure::OperationInterrupted => DeployFailure::Interrupted,
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
    use crate::operation::{
        AuthorityDecision, AuthorityEpoch, AuthorityPort, CommandIssuer, IdempotencyKey,
        OperationId, PrincipalId, ScopeId,
    };

    #[test]
    fn unknown_authority_is_not_allowed() {
        let check = AuthorityDecision::Unknown;

        assert_eq!(check.epoch(), None);
    }

    #[test]
    fn deploy_maps_replay_states_to_visible_failures() {
        assert_eq!(
            map_primitive_to_deploy(PrimitiveFailure::OperationInProgress),
            DeployFailure::OperationInProgress
        );
        assert_eq!(
            map_primitive_to_deploy(PrimitiveFailure::OperationAlreadyFailed),
            DeployFailure::OperationAlreadyFailed
        );
        assert_eq!(
            map_primitive_to_deploy(PrimitiveFailure::OperationAlreadySucceeded),
            DeployFailure::OperationAlreadySucceeded
        );
        assert_eq!(
            map_primitive_to_deploy(PrimitiveFailure::OperationInterrupted),
            DeployFailure::Interrupted
        );
    }

    #[test]
    fn deploy_command_issue_derives_fingerprint_from_request() {
        let request = request();
        let changed_route = DeployRequest {
            manifest: DeployManifest {
                route: RouteId::parse("route:admin").expect("route"),
                ..request.manifest.clone()
            },
            ..request.clone()
        };

        let first = DeployCommand::issue(
            &CommandIssuer::new(AllowAuthority),
            issue(),
            request.clone(),
        )
        .expect("first command");
        let second =
            DeployCommand::issue(&CommandIssuer::new(AllowAuthority), issue(), changed_route)
                .expect("second command");

        assert_ne!(
            first.envelope.fingerprint_for_test().payload_hash(),
            second.envelope.fingerprint_for_test().payload_hash()
        );
    }

    #[test]
    fn deploy_command_issue_includes_request_deadline_in_fingerprint() {
        let request = request();
        let changed_deadline = DeployRequest {
            deadline: UNIX_EPOCH + Duration::from_secs(120),
            ..request.clone()
        };

        let first = DeployCommand::issue(&CommandIssuer::new(AllowAuthority), issue(), request)
            .expect("first command");
        let second = DeployCommand::issue(
            &CommandIssuer::new(AllowAuthority),
            issue(),
            changed_deadline,
        )
        .expect("second command");

        assert_ne!(
            first.envelope.fingerprint_for_test().payload_hash(),
            second.envelope.fingerprint_for_test().payload_hash()
        );
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

    fn issue() -> CommandIssue {
        CommandIssue {
            operation: OperationId::parse("deploy-1").expect("operation"),
            idempotency: IdempotencyKey::parse("idem-1").expect("idempotency"),
            principal: PrincipalId::parse("node-a").expect("principal"),
            scope: ScopeId::parse("cluster").expect("scope"),
            deadline: UNIX_EPOCH + Duration::from_secs(60),
        }
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
}

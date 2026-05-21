//! Deploy orchestration product ports.

use std::time::SystemTime;

use crate::acme::{
    CertificateActivation, CertificateDeadline, CertificateMaterialState, CertificatePort,
    CertificateUnusableReason, CertificateUsability, EnsureCertificateOutcome, HttpsBinding,
    RevocationFreshness,
};
use crate::error::{DeployFailure, PrimitiveFailure};
use crate::runtime::{
    MachineId, ParticipantReceipt, RuntimeActivationOutcome, RuntimeActivationRequest, RuntimePort,
    WorkloadId,
};
use crate::serving::{
    RouteId, ServingActivationStatus, ServingCheckpoint, ServingGeneration, ServingPort,
    ServingSnapshot, ServingTarget,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalId(String);

impl PrincipalId {
    pub fn parse(value: impl Into<String>) -> Result<Self, DeployFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(String);

impl ScopeId {
    pub fn parse(value: impl Into<String>) -> Result<Self, DeployFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorityEpoch(u64);

impl AuthorityEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityContext {
    pub principal: PrincipalId,
    pub scope: ScopeId,
    pub epoch: AuthorityEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityCheck {
    Allowed(AuthorityContext),
    Denied,
    Unknown,
}

pub trait AuthorityPort {
    fn check(
        &self,
        principal: &PrincipalId,
        scope: &ScopeId,
    ) -> Result<AuthorityCheck, DeployFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationId(String);

impl OperationId {
    pub fn parse(value: impl Into<String>) -> Result<Self, DeployFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, DeployFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(String);

impl ResourceId {
    pub fn parse(value: impl Into<String>) -> Result<Self, DeployFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FenceEpoch(u64);

impl FenceEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceToken {
    pub resource: ResourceId,
    pub holder: PrincipalId,
    pub epoch: FenceEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationContext {
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub authority: AuthorityContext,
    pub fence: Option<FenceToken>,
    pub deadline: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationEvidence {
    pub operation: OperationId,
    pub recorded_at: SystemTime,
    pub kind: EvidenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceKind {
    Checkpoint,
    Observation,
    Failure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalMarker {
    Succeeded,
    Failed(DeployFailure),
    Interrupted,
}

pub trait OperationPort {
    fn record_evidence(&self, evidence: OperationEvidence) -> Result<(), PrimitiveFailure>;

    fn terminalize(
        &self,
        operation: &OperationId,
        marker: TerminalMarker,
    ) -> Result<(), PrimitiveFailure>;
}

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
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub authority: AuthorityContext,
    pub fence: Option<FenceToken>,
    pub manifest: DeployManifest,
    pub deadline: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOutcome {
    pub certificate: CertificateUsability,
    pub runtime: ParticipantReceipt,
    pub serving: ServingCheckpoint,
}

pub struct DeployEngine<C, R, S, O> {
    certificates: C,
    runtime: R,
    serving: S,
    operations: O,
}

impl<C, R, S, O> DeployEngine<C, R, S, O> {
    #[must_use]
    pub fn new(certificates: C, runtime: R, serving: S, operations: O) -> Self {
        Self {
            certificates,
            runtime,
            serving,
            operations,
        }
    }
}

impl<C, R, S, O> DeployEngine<C, R, S, O>
where
    C: CertificatePort,
    R: RuntimePort,
    S: ServingPort,
    O: OperationPort,
{
    pub fn deploy_https(&self, request: DeployRequest) -> Result<DeployOutcome, DeployFailure> {
        let context = MutationContext {
            operation: request.operation.clone(),
            idempotency: request.idempotency.clone(),
            authority: request.authority.clone(),
            fence: request.fence.clone(),
            deadline: request.deadline,
        };

        self.record_evidence(&context.operation, EvidenceKind::Observation)?;
        let certificate = self.ensure_certificate(&context, &request)?;
        self.record_evidence(&context.operation, EvidenceKind::Checkpoint)?;
        let runtime = self.activate_runtime(&context, &request)?;
        self.record_evidence(&context.operation, EvidenceKind::Checkpoint)?;
        let serving = self.commit_and_verify_serving(&context, &request)?;

        self.operations
            .terminalize(&context.operation, TerminalMarker::Succeeded)
            .map_err(map_primitive_to_deploy)?;

        Ok(DeployOutcome {
            certificate,
            runtime,
            serving,
        })
    }

    fn ensure_certificate(
        &self,
        context: &MutationContext,
        request: &DeployRequest,
    ) -> Result<CertificateUsability, DeployFailure> {
        let outcome = self
            .certificates
            .ensure_usable(
                context,
                &request.manifest.https,
                CertificateDeadline {
                    expires_at: request.deadline,
                },
            )
            .map_err(|_| DeployFailure::CertificateUnusable)?;

        let EnsureCertificateOutcome::Usable(certificate) = outcome else {
            return Err(DeployFailure::CertificateUnusable);
        };

        if certificate_unusable_reason(
            &certificate,
            &request.manifest.https,
            request.manifest.minimum_certificate_valid_until,
        )
        .is_some()
        {
            return Err(DeployFailure::CertificateUnusable);
        }

        Ok(certificate)
    }

    fn activate_runtime(
        &self,
        context: &MutationContext,
        request: &DeployRequest,
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

        Ok(receipt)
    }

    fn commit_and_verify_serving(
        &self,
        context: &MutationContext,
        request: &DeployRequest,
    ) -> Result<ServingCheckpoint, DeployFailure> {
        let snapshot = ServingSnapshot {
            route: request.manifest.route.clone(),
            hostname: request.manifest.https.hostname.clone(),
            target: request.manifest.serving_target.clone(),
            generation: request.manifest.serving_generation,
        };
        let checkpoint = self
            .serving
            .commit_snapshot(context, snapshot)
            .map_err(|_| DeployFailure::ServingActivationFailed)?;

        match self
            .serving
            .activation_status(&request.manifest.serving_target)
            .map_err(|_| DeployFailure::ServingActivationFailed)?
        {
            ServingActivationStatus::Acknowledged(activated) if activated == checkpoint => {
                Ok(checkpoint)
            }
            ServingActivationStatus::Acknowledged(_)
            | ServingActivationStatus::Failed(_)
            | ServingActivationStatus::Unknown => Err(DeployFailure::ServingActivationFailed),
        }
    }

    fn record_evidence(
        &self,
        operation: &OperationId,
        kind: EvidenceKind,
    ) -> Result<(), DeployFailure> {
        self.operations
            .record_evidence(OperationEvidence {
                operation: operation.clone(),
                recorded_at: SystemTime::now(),
                kind,
            })
            .map_err(map_primitive_to_deploy)
    }
}

#[must_use]
pub fn certificate_is_usable(
    certificate: &CertificateUsability,
    binding: &HttpsBinding,
    minimum_valid_until: SystemTime,
) -> bool {
    certificate_unusable_reason(certificate, binding, minimum_valid_until).is_none()
}

#[must_use]
pub fn certificate_unusable_reason(
    certificate: &CertificateUsability,
    binding: &HttpsBinding,
    minimum_valid_until: SystemTime,
) -> Option<CertificateUnusableReason> {
    if certificate.hostname != binding.hostname {
        return Some(CertificateUnusableReason::HostnameMismatch);
    }
    if certificate.activation != CertificateActivation::Acknowledged {
        return Some(CertificateUnusableReason::ActivationRejected);
    }
    if certificate.material != CertificateMaterialState::PresentProtected {
        return Some(CertificateUnusableReason::UnsafeMaterial);
    }
    if certificate.revocation != RevocationFreshness::KnownFresh {
        return Some(CertificateUnusableReason::KnownRevoked);
    }
    if certificate.not_after < minimum_valid_until {
        return Some(CertificateUnusableReason::SafetyWindowTooShort);
    }
    None
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
    }
}

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> Result<T, DeployFailure> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(DeployFailure::InvalidManifest);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_authority_is_not_allowed() {
        let check = AuthorityCheck::Unknown;

        assert!(!matches!(check, AuthorityCheck::Allowed(_)));
    }

    #[test]
    fn empty_operation_id_is_invalid_manifest() {
        assert_eq!(OperationId::parse(""), Err(DeployFailure::InvalidManifest));
    }
}
